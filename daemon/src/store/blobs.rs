//! Content-addressed storage for clipboard representations.
//!
//! A blob is named by the hex digest of its own content, so identical
//! representations are stored once however many entries reference them. The
//! write path hashes while it copies: a representation is read from the socket
//! exactly once, never seeks, and never exists whole in memory. That is what
//! lets an entry be arbitrarily large without the daemon's residency following
//! it.

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Big enough that a large paste is a few thousand syscalls rather than a
/// million, small enough to stay in L2 and to never show up in the process's
/// resident size.
const COPY_BUFFER: usize = 64 * 1024;

/// Digests are hex, so a blob directory fans out over 256 subdirectories. That
/// keeps any one directory small enough for the filesystem to stay quick even
/// with a very long archive.
const FANOUT: usize = 2;

pub struct Blobs {
    root: PathBuf,
}

/// What a completed streaming write turned out to be.
pub struct Written {
    pub digest: String,
    pub bytes: u64,
    /// False when a blob with this digest was already stored, which is the
    /// common case for a clipboard that is re-asserted unchanged. Nothing acts
    /// on it yet; it is here because a write that stored nothing is a
    /// materially different outcome from one that did.
    #[cfg_attr(not(test), allow(dead_code))]
    pub created: bool,
}

impl Blobs {
    pub fn open(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("incoming"))?;
        Ok(Self { root })
    }

    fn path(&self, digest: &str) -> PathBuf {
        self.root.join(&digest[..FANOUT]).join(&digest[FANOUT..])
    }

    /// Whether the store already holds this content. Used by the tests and by
    /// the startup reconciliation, both of which compare disk against index.
    #[cfg(test)]
    pub fn exists(&self, digest: &str) -> bool {
        self.path(digest).exists()
    }

    pub fn open_read(&self, digest: &str) -> io::Result<File> {
        File::open(self.path(digest))
    }

    /// Copies exactly `bytes` from `source` into the store, naming the result
    /// by its own digest.
    ///
    /// The count comes from the frame rather than from end-of-stream, because
    /// the stream carries the frames that follow as well: reading to the end
    /// would consume the rest of the connection. A source that ends early is
    /// an error and leaves nothing behind.
    pub fn write_from(&self, source: &mut impl Read, bytes: u64) -> io::Result<Written> {
        let mut writer = self.writer()?;
        writer.absorb(source, bytes)?;
        writer.finish()
    }

    /// Begins a blob whose length is not known in advance.
    ///
    /// A compositor hands a selection over as a stream and never says how long
    /// it is, so a capture client cannot declare a count without first
    /// buffering the whole thing -- which is the one cost this design exists
    /// to avoid. This lets it feed what it has as it arrives and still pay one
    /// pass and one hash.
    pub fn writer(&self) -> io::Result<Writer<'_>> {
        let incoming = self.root.join("incoming").join(temporary_name());
        let file = File::create(&incoming)?;
        Ok(Writer {
            blobs: self,
            file: Some(file),
            hasher: Sha256::new(),
            bytes: 0,
            incoming,
        })
    }

    /// Stores a blob already held in memory. Only thumbnails take this path;
    /// clipboard content never does.
    pub fn write_bytes(&self, content: &[u8]) -> io::Result<Written> {
        self.write_from(&mut io::Cursor::new(content), content.len() as u64)
    }

    pub fn remove(&self, digest: &str) -> io::Result<()> {
        match fs::remove_file(self.path(digest)) {
            Ok(()) => Ok(()),
            // The index is the authority on what exists. A blob already gone
            // from the disk is the state the caller wanted.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Clears temporaries left by a process that died mid-transfer. Called at
    /// startup, when nothing else can be writing them.
    pub fn sweep_incoming(&self) -> io::Result<()> {
        for entry in fs::read_dir(self.root.join("incoming"))? {
            let _ = fs::remove_file(entry?.path());
        }
        Ok(())
    }
}

/// A blob being written a piece at a time.
///
/// Hashes while it copies, so the finished blob's name falls out of the same
/// single pass that stored it. Dropped without finishing, it removes its own
/// temporary: a partial blob would hash to something plausible and be served
/// later as if it were whole.
pub struct Writer<'a> {
    blobs: &'a Blobs,
    file: Option<File>,
    hasher: Sha256,
    bytes: u64,
    incoming: PathBuf,
}

impl Writer<'_> {
    /// Copies exactly `bytes` more from `source` into the blob.
    pub fn absorb(&mut self, source: &mut impl Read, bytes: u64) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("this blob has already been finished"));
        };

        let mut buffer = [0u8; COPY_BUFFER];
        let mut remaining = bytes;

        while remaining > 0 {
            let want = remaining.min(COPY_BUFFER as u64) as usize;
            let read = source.read(&mut buffer[..want])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("payload ended {remaining} bytes early"),
                ));
            }
            self.hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
            remaining -= read as u64;
        }

        self.bytes += bytes;
        Ok(())
    }

    /// How much has been absorbed so far, so a caller can enforce a budget
    /// against a stream that never declared its length.
    pub fn written(&self) -> u64 {
        self.bytes
    }

    /// Seals the blob under the name its own content chose.
    pub fn finish(mut self) -> io::Result<Written> {
        let Some(file) = self.file.take() else {
            return Err(io::Error::other("this blob has already been finished"));
        };
        // The archive outlives the process that wrote it, so a blob is on the
        // disk before anything in the index points at it.
        file.sync_all()?;
        drop(file);

        let digest = hex(&std::mem::take(&mut self.hasher).finalize());
        let bytes = self.bytes;
        let destination = self.blobs.path(&digest);

        if destination.exists() {
            let _ = fs::remove_file(&self.incoming);
            return Ok(Written {
                digest,
                bytes,
                created: false,
            });
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&self.incoming, &destination)?;
        Ok(Written {
            digest,
            bytes,
            created: true,
        })
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        // Still holding the file means `finish` was never reached: the
        // transfer failed or the connection went. Nothing partial survives.
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.incoming);
        }
    }
}

/// Unique within the daemon without needing a random source: one process
/// writes these, and a counter plus the pid cannot collide with a temporary
/// left by an earlier one.
fn temporary_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn store() -> (Blobs, TempDir) {
        let dir = TempDir::new();
        let blobs = Blobs::open(dir.path().join("blobs")).unwrap();
        (blobs, dir)
    }

    #[test]
    fn names_a_blob_by_the_digest_of_its_content() {
        let (blobs, _dir) = store();
        let written = blobs.write_bytes(b"clipboard").unwrap();
        // sha256("clipboard"), so the name is reproducible outside this code.
        assert_eq!(written.digest.len(), 64);
        assert!(written.created);
        assert_eq!(written.bytes, 9);
        assert!(blobs.exists(&written.digest));
    }

    #[test]
    fn stores_identical_content_once() {
        let (blobs, _dir) = store();
        let first = blobs.write_bytes(b"same").unwrap();
        let second = blobs.write_bytes(b"same").unwrap();
        assert_eq!(first.digest, second.digest);
        assert!(first.created);
        // The second copy is recognised and discarded rather than rewritten.
        assert!(!second.created);
    }

    #[test]
    fn reads_back_exactly_what_was_written() {
        let (blobs, _dir) = store();
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let written = blobs.write_bytes(&content).unwrap();
        let mut read = Vec::new();
        blobs
            .open_read(&written.digest)
            .unwrap()
            .read_to_end(&mut read)
            .unwrap();
        assert_eq!(read, content);
    }

    #[test]
    fn takes_only_the_declared_count_and_leaves_the_rest_of_the_stream() {
        let (blobs, _dir) = store();
        let mut stream = io::Cursor::new(b"paydata{\"op\":\"commit\"}".to_vec());
        let written = blobs.write_from(&mut stream, 7).unwrap();
        assert_eq!(written.bytes, 7);

        // The frame after the payload must still be there to be parsed.
        let mut rest = String::new();
        stream.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, r#"{"op":"commit"}"#);
    }

    #[test]
    fn a_payload_that_ends_early_stores_nothing() {
        let (blobs, _dir) = store();
        let mut stream = io::Cursor::new(b"short".to_vec());
        assert!(blobs.write_from(&mut stream, 4096).is_err());

        // Nothing is left behind to be mistaken for a whole blob later.
        let incoming: Vec<_> = fs::read_dir(blobs.root.join("incoming")).unwrap().collect();
        assert!(incoming.is_empty());
    }

    #[test]
    fn a_blob_fed_in_pieces_is_the_same_blob_as_one_fed_whole() {
        let (blobs, _dir) = store();
        let whole = blobs.write_bytes(b"one two three").unwrap();

        let mut writer = blobs.writer().unwrap();
        for piece in [&b"one "[..], b"two ", b"three"] {
            writer
                .absorb(&mut io::Cursor::new(piece), piece.len() as u64)
                .unwrap();
        }
        assert_eq!(writer.written(), 13);
        let pieced = writer.finish().unwrap();

        // The digest is over the content, not over how it arrived.
        assert_eq!(pieced.digest, whole.digest);
        assert_eq!(pieced.bytes, 13);
    }

    #[test]
    fn a_piecewise_blob_abandoned_partway_leaves_nothing_behind() {
        let (blobs, _dir) = store();
        {
            let mut writer = blobs.writer().unwrap();
            writer.absorb(&mut io::Cursor::new(b"half"), 4).unwrap();
            // Dropped without finishing, which is what a dropped connection
            // in the middle of a chunked part looks like.
        }

        let incoming: Vec<_> = fs::read_dir(blobs.root.join("incoming")).unwrap().collect();
        assert!(incoming.is_empty(), "no partial blob survives");
    }

    #[test]
    fn removing_something_already_gone_is_not_an_error() {
        let (blobs, _dir) = store();
        let written = blobs.write_bytes(b"transient").unwrap();
        blobs.remove(&written.digest).unwrap();
        blobs.remove(&written.digest).unwrap();
        assert!(!blobs.exists(&written.digest));
    }
}
