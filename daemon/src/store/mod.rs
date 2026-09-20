//! The archive: metadata in SQLite, bytes in a content-addressed blob store.
//!
//! The split matters for more than tidiness. Streaming a representation in is
//! unbounded work, and it happens with no lock held, because a blob's name is
//! derived from its own content and so cannot collide with anything another
//! connection is writing. Only the commit that publishes an entry takes the
//! index lock, and that is a handful of small statements. One enormous paste
//! therefore never stalls the picker's search.

pub mod blobs;
pub mod index;
pub mod thumb;

use crate::kind;
use crate::proto::Summary;
use blobs::{Blobs, Written};
use index::{Facts, Index, Part};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How much of a text representation is read back to feed previews and search.
///
/// A clipboard can carry a whole file; indexing all of it would grow the index
/// without making search better, because nobody recognises an entry by its
/// hundred-thousandth character.
const INDEXED_TEXT: u64 = 64 * 1024;

/// The largest representation the daemon will decode looking for a thumbnail.
/// Past this it records the entry and its dimensions but draws a kind icon.
const THUMBNAILED_IMAGE: u64 = 64 * 1024 * 1024;

/// Bytes the archive may occupy before the oldest unpinned entries are evicted.
pub const DEFAULT_BUDGET: i64 = 5 * 1024 * 1024 * 1024;

pub struct Store {
    blobs: Blobs,
    index: Mutex<Index>,
    budget: i64,
}

/// One representation a draft has accepted.
pub struct Accepted {
    pub mime: String,
    pub digest: String,
    pub bytes: u64,
}

/// What a commit did.
pub struct Committed {
    pub entry: i64,
    /// False when identical content was already in the archive and only its
    /// timestamp moved.
    pub created: bool,
    pub summary: Summary,
    /// Entries eviction removed to get back under budget.
    pub evicted: Vec<i64>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Db(rusqlite::Error),
    TooLarge,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(error) => write!(f, "{error}"),
            StoreError::Db(error) => write!(f, "{error}"),
            StoreError::TooLarge => write!(f, "larger than the whole archive budget"),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        StoreError::Io(error)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Db(error)
    }
}

impl StoreError {
    /// The protocol code a client should see for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            StoreError::TooLarge => crate::proto::code::TOO_LARGE,
            _ => crate::proto::code::STORAGE,
        }
    }
}

type Result<T> = std::result::Result<T, StoreError>;

impl Store {
    pub fn open(root: &Path, budget: i64) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let blobs = Blobs::open(root.join("blobs"))?;
        // A daemon that died mid-transfer leaves a partial file behind. Now,
        // before anything else can be writing, is the only safe time to sweep.
        blobs.sweep_incoming()?;
        let index = Index::open(&root.join("index.db"))?;

        Ok(Self {
            blobs,
            index: Mutex::new(index),
            budget,
        })
    }

    pub fn budget(&self) -> i64 {
        self.budget
    }

    /// Streams one representation of known length into the blob store.
    ///
    /// Deliberately takes no lock: this is the unbounded part of recording an
    /// entry, and a picker searching the archive must not wait behind it.
    pub fn accept(&self, mime: &str, source: &mut impl Read, bytes: u64) -> Result<Accepted> {
        // Compared in unsigned terms: a declared length above `i64::MAX`
        // would wrap to a negative number and slip past a signed check.
        if bytes > self.budget.max(0) as u64 {
            // Reject rather than evict: nothing is worth emptying the whole
            // archive to hold, and the caller still has to drain the payload.
            return Err(StoreError::TooLarge);
        }
        let Written { digest, bytes, .. } = self.blobs.write_from(source, bytes)?;
        Ok(Accepted {
            mime: mime.to_string(),
            digest,
            bytes,
        })
    }

    /// Begins a representation whose length the client does not yet know.
    ///
    /// The budget is checked as the pieces arrive rather than up front, which
    /// is the only thing that can be done when nobody has said how much is
    /// coming.
    pub fn incoming(&self, mime: &str) -> Result<Incoming<'_>> {
        Ok(Incoming {
            writer: self.blobs.writer()?,
            mime: mime.to_string(),
            budget: self.budget,
        })
    }

    /// Publishes a draft's representations as one entry.
    pub fn commit(
        &self,
        accepted: &[Accepted],
        source: Option<&str>,
        at: i64,
    ) -> Result<Option<Committed>> {
        if accepted.is_empty() {
            return Ok(None);
        }
        // Defence in depth: the capture client filters secrets too, but the
        // macOS and Windows backends live inside this process and an archive
        // that holds a password it promises not to show still holds it.
        if kind::any_sensitive(accepted.iter().map(|part| part.mime.as_str())) {
            return Ok(None);
        }

        let identity = identity(accepted);
        let parts: Vec<Part> = accepted
            .iter()
            .map(|part| Part {
                mime: part.mime.clone(),
                digest: part.digest.clone(),
                bytes: part.bytes,
            })
            .collect();

        let mut index = self.index.lock().expect("index lock");

        // Identical content already held: move it to the top of the list
        // instead of recording it twice. This is what keeps an application
        // that re-asserts the clipboard from filling the archive.
        if let Some(existing) = index.touch(&identity, at)? {
            let summary = index
                .summary(existing)?
                .expect("an entry that was just touched still exists");
            return Ok(Some(Committed {
                entry: existing,
                created: false,
                summary,
                evicted: Vec::new(),
            }));
        }

        let mut facts = self.derive(accepted)?;
        facts.source = source.map(str::to_string);
        let entry = index.insert(&identity, &parts, &facts, at)?;
        let summary = index
            .summary(entry)?
            .expect("an entry that was just inserted exists");

        let (orphaned, evicted) = index.evict_to(self.budget)?;
        drop(index);
        self.sweep(orphaned);

        Ok(Some(Committed {
            entry,
            created: true,
            summary,
            evicted,
        }))
    }

    /// Reads back enough of the content to describe the entry: a text sample
    /// for the preview and search, and a thumbnail for a picture.
    fn derive(&self, accepted: &[Accepted]) -> Result<Facts> {
        let mut facts = Facts::default();

        if let Some(part) = self.best(accepted, kind::is_text) {
            let text = self.read_text(&part.digest, INDEXED_TEXT)?;
            facts.preview = Some(kind::preview(&text));
            facts.body = Some(text);
        }

        if let Some(part) = self.best(accepted, kind::is_image) {
            if part.bytes <= THUMBNAILED_IMAGE {
                let mut content = Vec::with_capacity(part.bytes as usize);
                self.blobs
                    .open_read(&part.digest)?
                    .take(part.bytes)
                    .read_to_end(&mut content)?;
                let made = thumb::make(&content);
                facts.width = made.width;
                facts.height = made.height;
                if let Some(png) = made.png {
                    let written = self.blobs.write_bytes(&png)?;
                    facts.thumb = Some(Part {
                        mime: "image/png".into(),
                        digest: written.digest,
                        bytes: written.bytes,
                    });
                }
            }
        }

        Ok(facts)
    }

    /// The most preferred representation matching a predicate.
    fn best<'a>(
        &self,
        accepted: &'a [Accepted],
        matches: fn(&str) -> bool,
    ) -> Option<&'a Accepted> {
        accepted
            .iter()
            .filter(|part| matches(&part.mime))
            .min_by_key(|part| kind::rank(&part.mime))
    }

    /// Reads at most `limit` bytes of a blob as text.
    ///
    /// Invalid UTF-8 is replaced rather than refused: a representation
    /// labelled text that is not quite text still deserves a preview, and the
    /// bytes served back to a pasting application are untouched either way.
    fn read_text(&self, digest: &str, limit: u64) -> Result<String> {
        let mut raw = Vec::new();
        self.blobs
            .open_read(digest)?
            .take(limit)
            .read_to_end(&mut raw)?;
        // A cut at `limit` can land inside a character; from_utf8_lossy turns
        // that tail into a replacement character rather than failing.
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }

    pub fn list(
        &self,
        limit: u32,
        before: Option<i64>,
        query: Option<&str>,
        only: Option<kind::Kind>,
    ) -> Result<Vec<Summary>> {
        let index = self.index.lock().expect("index lock");
        Ok(index.list(limit, before, query, only)?)
    }

    pub fn summary(&self, entry: i64) -> Result<Option<Summary>> {
        let index = self.index.lock().expect("index lock");
        Ok(index.summary(entry)?)
    }

    /// Opens one representation for serving back. The file handle is returned
    /// so the caller streams it to the socket without buffering it.
    pub fn open_part(
        &self,
        entry: i64,
        mime: Option<&str>,
    ) -> Result<Option<(String, std::fs::File, u64)>> {
        let located = {
            let index = self.index.lock().expect("index lock");
            index.locate(entry, mime)?
        };
        let Some((mime, digest, bytes)) = located else {
            return Ok(None);
        };
        Ok(Some((mime, self.blobs.open_read(&digest)?, bytes)))
    }

    /// The thumbnail for an entry, decoded into pixels ready to upload.
    ///
    /// Stored as PNG and served as RGBA: a few kilobytes is the right thing to
    /// keep on disk, and pixels are the only thing the shell can draw without
    /// decoding something itself.
    pub fn thumbnail(&self, entry: i64) -> Result<Option<thumb::Pixels>> {
        let located = {
            let index = self.index.lock().expect("index lock");
            index.thumb(entry)?
        };
        let Some((digest, bytes)) = located else {
            return Ok(None);
        };

        let mut png = Vec::with_capacity(bytes as usize);
        self.blobs
            .open_read(&digest)?
            .take(bytes)
            .read_to_end(&mut png)?;
        // A thumbnail that will not decode is one the UI draws a kind icon
        // for; it is not worth failing the request over.
        Ok(thumb::decode(&png))
    }

    pub fn set_pinned(&self, entry: i64, pinned: bool) -> Result<Option<Summary>> {
        let index = self.index.lock().expect("index lock");
        if !index.set_pinned(entry, pinned)? {
            return Ok(None);
        }
        Ok(index.summary(entry)?)
    }

    pub fn remove(&self, entry: i64) -> Result<bool> {
        let orphaned = {
            let mut index = self.index.lock().expect("index lock");
            if index.summary(entry)?.is_none() {
                return Ok(false);
            }
            index.remove(entry)?
        };
        self.sweep(orphaned);
        Ok(true)
    }

    pub fn clear(&self) -> Result<()> {
        let orphaned = {
            let mut index = self.index.lock().expect("index lock");
            index.clear()?
        };
        self.sweep(orphaned);
        Ok(())
    }

    pub fn stats(&self) -> Result<(i64, i64)> {
        let index = self.index.lock().expect("index lock");
        Ok((index.entries()?, index.bytes()?))
    }

    /// Deletes blob files the index no longer references.
    ///
    /// Always called after the transaction that dropped the reference has
    /// committed. A file left behind by a failure here is wasted space the
    /// next startup reclaims; a file deleted too early would be a hole in the
    /// archive, so the order is not interchangeable.
    fn sweep(&self, orphaned: Vec<String>) {
        for digest in orphaned {
            if let Err(error) = self.blobs.remove(&digest) {
                eprintln!("rldyour-clipboardd: could not remove blob {digest}: {error}");
            }
        }
    }

    /// Deletes blob files no index record points at.
    ///
    /// Only a crash between a commit and its sweep can leave one, so this runs
    /// at startup and never again.
    pub fn reconcile(&self, root: &Path) -> Result<u64> {
        let known: std::collections::HashSet<String> = {
            let index = self.index.lock().expect("index lock");
            index.known_digests()?.into_iter().collect()
        };

        let mut reclaimed = 0;
        let blob_root = root.join("blobs");
        for shard in std::fs::read_dir(&blob_root)? {
            let shard = shard?.path();
            if !shard.is_dir() || shard.file_name().is_some_and(|name| name == "incoming") {
                continue;
            }
            let Some(prefix) = shard.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            for blob in std::fs::read_dir(&shard)? {
                let blob = blob?;
                let Some(rest) = blob.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let digest = format!("{prefix}{rest}");
                if !known.contains(&digest) {
                    reclaimed += blob.metadata().map(|meta| meta.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(blob.path());
                }
            }
        }
        Ok(reclaimed)
    }
}

/// A representation being streamed in without a declared length.
pub struct Incoming<'a> {
    writer: blobs::Writer<'a>,
    mime: String,
    budget: i64,
}

impl Incoming<'_> {
    /// Takes one more piece of the representation.
    pub fn absorb(&mut self, source: &mut impl Read, bytes: u64) -> Result<()> {
        self.writer.absorb(source, bytes)?;
        if self.writer.written() > self.budget.max(0) as u64 {
            // Stop as soon as it is clear this cannot fit rather than after
            // writing all of it. Dropping the writer removes what was stored.
            return Err(StoreError::TooLarge);
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Accepted> {
        let Written { digest, bytes, .. } = self.writer.finish()?;
        Ok(Accepted {
            mime: self.mime,
            digest,
            bytes,
        })
    }
}

/// An entry's identity: the digest of its sorted `(mime, content digest)` set.
///
/// Two clipboard events are the same entry when they offer the same
/// representations with the same bytes, whatever order they arrived in and
/// whatever application produced them.
fn identity(accepted: &[Accepted]) -> String {
    use sha2::{Digest, Sha256};

    let mut pairs: Vec<String> = accepted
        .iter()
        .map(|part| format!("{}\u{0}{}", part.mime, part.digest))
        .collect();
    pairs.sort();

    let mut hasher = Sha256::new();
    for pair in pairs {
        hasher.update(pair.as_bytes());
        hasher.update(*b"\n");
    }

    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The archive directory for this platform.
pub fn default_root() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(Path::new(&data).join("rldyour-clipboard"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::other("HOME and XDG_DATA_HOME are unset"))?;
        return Ok(Path::new(&home).join(".local/share/rldyour-clipboard"));
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unset"))?;
        // Application Support, not Caches: Caches is purgeable under disk
        // pressure, and an archive that silently empties is worse than none.
        return Ok(Path::new(&home).join("Library/Application Support/rldyour-clipboard"));
    }

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| io::Error::other("LOCALAPPDATA is unset"))?;
        return Ok(Path::new(&base).join("rldyour-clipboard"));
    }

    #[allow(unreachable_code)]
    Err(io::Error::other("no archive location on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn store(budget: i64) -> (Store, TempDir) {
        let dir = TempDir::new();
        let store = Store::open(dir.path(), budget).unwrap();
        (store, dir)
    }

    fn accept(store: &Store, mime: &str, content: &[u8]) -> Accepted {
        store
            .accept(mime, &mut io::Cursor::new(content), content.len() as u64)
            .unwrap()
    }

    #[test]
    fn records_an_entry_and_describes_it_without_being_asked() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let parts = [
            accept(&store, "text/html", b"<b>git rebase</b>"),
            accept(&store, "text/plain", b"git rebase"),
        ];

        let done = store.commit(&parts, Some("firefox"), 100).unwrap().unwrap();
        assert!(done.created);
        assert_eq!(done.summary.kind, "text");
        // The preview comes from the richest text representation.
        assert_eq!(done.summary.preview.as_deref(), Some("<b>git rebase</b>"));
        assert_eq!(done.summary.source.as_deref(), Some("firefox"));
        assert_eq!(done.summary.mimes, vec!["text/html", "text/plain"]);
    }

    #[test]
    fn the_same_content_copied_again_is_the_same_entry() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let first = store
            .commit(&[accept(&store, "text/plain", b"same")], None, 100)
            .unwrap()
            .unwrap();
        let second = store
            .commit(&[accept(&store, "text/plain", b"same")], None, 200)
            .unwrap()
            .unwrap();

        assert_eq!(first.entry, second.entry);
        assert!(first.created);
        assert!(
            !second.created,
            "a re-copy moves the timestamp, nothing more"
        );
        assert_eq!(second.summary.at, 200);
        assert_eq!(store.stats().unwrap().0, 1);
    }

    #[test]
    fn representation_order_does_not_change_an_entrys_identity() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let forwards = [
            accept(&store, "text/html", b"<b>x</b>"),
            accept(&store, "text/plain", b"x"),
        ];
        let backwards = [
            accept(&store, "text/plain", b"x"),
            accept(&store, "text/html", b"<b>x</b>"),
        ];

        let first = store.commit(&forwards, None, 100).unwrap().unwrap();
        let second = store.commit(&backwards, None, 200).unwrap().unwrap();
        assert_eq!(first.entry, second.entry);
    }

    #[test]
    fn a_secret_is_refused_whole_and_not_merely_hidden() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let parts = [
            accept(&store, "text/plain", b"hunter2"),
            accept(&store, "x-kde-passwordManagerHint", b"secret"),
        ];

        assert!(
            store
                .commit(&parts, Some("keepassxc"), 100)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.stats().unwrap().0, 0);
    }

    #[test]
    fn serves_back_exactly_the_bytes_that_were_recorded() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let content: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        let done = store
            .commit(&[accept(&store, "image/png", &content)], None, 100)
            .unwrap()
            .unwrap();

        let (mime, mut file, bytes) = store.open_part(done.entry, None).unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, content.len() as u64);
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(read, content);
    }

    #[test]
    fn a_representation_larger_than_the_whole_budget_is_refused() {
        let (store, _dir) = store(1024);
        let big = vec![0u8; 4096];
        let outcome = store.accept("image/png", &mut io::Cursor::new(&big), big.len() as u64);
        assert!(matches!(outcome, Err(StoreError::TooLarge)));
        // Refusing it left the archive untouched rather than emptying it.
        assert_eq!(store.stats().unwrap(), (0, 0));
    }

    #[test]
    fn committing_over_budget_evicts_the_oldest_unpinned_entries() {
        let (store, _dir) = store(2048);
        let mut entries = Vec::new();
        for (index, at) in [(0u8, 100), (1, 200), (2, 300)].into_iter() {
            let content = vec![index; 900];
            let done = store
                .commit(&[accept(&store, "image/png", &content)], None, at)
                .unwrap()
                .unwrap();
            entries.push(done);
        }

        // Three 900-byte entries do not fit in 2048, so the oldest went.
        let last = entries.last().unwrap();
        assert_eq!(last.evicted, vec![entries[0].entry]);
        assert!(store.summary(entries[0].entry).unwrap().is_none());
        assert!(store.summary(entries[2].entry).unwrap().is_some());
        assert!(store.stats().unwrap().1 <= 2048);
    }

    #[test]
    fn search_finds_an_entry_by_its_text_whatever_else_it_holds() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        store
            .commit(
                &[
                    accept(&store, "text/plain", b"deployment runbook"),
                    accept(&store, "image/png", b"not really a png"),
                ],
                None,
                100,
            )
            .unwrap();

        assert_eq!(
            store.list(10, None, Some("runbook"), None).unwrap().len(),
            1
        );
        assert_eq!(
            store.list(10, None, Some("missing"), None).unwrap().len(),
            0
        );
    }

    #[test]
    fn a_pinned_entry_survives_a_clear() {
        let (store, _dir) = store(DEFAULT_BUDGET);
        let kept = store
            .commit(&[accept(&store, "text/plain", b"keep")], None, 100)
            .unwrap()
            .unwrap();
        store
            .commit(&[accept(&store, "text/plain", b"drop")], None, 200)
            .unwrap();

        store.set_pinned(kept.entry, true).unwrap();
        store.clear().unwrap();

        assert_eq!(store.stats().unwrap().0, 1);
        assert!(store.summary(kept.entry).unwrap().is_some());
    }

    #[test]
    fn startup_reclaims_blobs_a_crash_left_unreferenced() {
        let dir = TempDir::new();
        let store = Store::open(dir.path(), DEFAULT_BUDGET).unwrap();

        // A blob written but never committed is exactly what a crash between
        // the two leaves behind.
        let stranded = accept(&store, "text/plain", b"never committed");
        assert!(store.blobs.exists(&stranded.digest));

        let reclaimed = store.reconcile(dir.path()).unwrap();
        assert_eq!(reclaimed, 15);
        assert!(!store.blobs.exists(&stranded.digest));
    }
}
