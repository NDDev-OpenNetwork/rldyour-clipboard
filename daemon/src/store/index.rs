//! The archive's metadata, in SQLite.
//!
//! The index is the authority on what the archive contains; the blob store is
//! only the bytes those records point at. Every mutation that can orphan a
//! blob returns the digests it orphaned, so the caller deletes files after the
//! transaction that stopped referencing them has committed. Losing that race
//! the other way — deleting a file an uncommitted transaction still needs —
//! would turn a crash into a hole in the archive.

use crate::kind::{self, Kind};
use crate::proto::Summary;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// A representation about to be recorded.
#[derive(Debug, Clone)]
pub struct Part {
    pub mime: String,
    pub digest: String,
    pub bytes: u64,
}

/// Everything an entry needs beyond its parts.
#[derive(Debug, Default)]
pub struct Facts {
    pub preview: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumb: Option<Part>,
    pub source: Option<String>,
    /// Text handed to the full-text index, already capped by the caller.
    pub body: Option<String>,
}

pub struct Index {
    connection: Connection,
}

/// A list request never returns more than this however much it asks for: the
/// UI draws a window, not an archive, and an unbounded answer would be one
/// allocation the peer chooses the size of.
pub const MAX_LIMIT: u32 = 500;

impl Index {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        // WAL lets the UI read while a capture writes, which is the only
        // concurrency the archive ever sees. NORMAL trades a fsync per commit
        // for the guarantee that only a host crash — not a daemon crash — can
        // lose the most recent entry.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // A blocked writer should wait rather than fail: the alternative is a
        // clipboard event lost because a list query held the lock.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let index = Self { connection };
        index.migrate()?;
        Ok(index)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        self.connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS blob (
                digest TEXT PRIMARY KEY,
                bytes  INTEGER NOT NULL,
                refs   INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS entry (
                id       INTEGER PRIMARY KEY,
                identity TEXT    NOT NULL UNIQUE,
                kind     TEXT    NOT NULL,
                bytes    INTEGER NOT NULL,
                preview  TEXT,
                width    INTEGER,
                height   INTEGER,
                thumb    TEXT,
                pinned   INTEGER NOT NULL DEFAULT 0,
                source   TEXT,
                at       INTEGER NOT NULL
            );

            -- The one ordering the UI ever asks for: pinned first, then newest.
            CREATE INDEX IF NOT EXISTS entry_recent
                ON entry(pinned DESC, at DESC, id DESC);

            CREATE TABLE IF NOT EXISTS part (
                entry  INTEGER NOT NULL REFERENCES entry(id) ON DELETE CASCADE,
                mime   TEXT    NOT NULL,
                digest TEXT    NOT NULL,
                bytes  INTEGER NOT NULL,
                rank   INTEGER NOT NULL,
                PRIMARY KEY (entry, mime)
            );

            CREATE INDEX IF NOT EXISTS part_digest ON part(digest);

            CREATE VIRTUAL TABLE IF NOT EXISTS entry_fts USING fts5(body);
            "#,
        )
    }

    /// Moves an existing entry's timestamp to `at`.
    ///
    /// Returns its id when the identity was already known, which is the common
    /// case: an application that re-asserts the clipboard unchanged every few
    /// seconds must not fill the archive with copies of one thing.
    pub fn touch(&self, identity: &str, at: i64) -> rusqlite::Result<Option<i64>> {
        let existing: Option<i64> = self
            .connection
            .query_row(
                "SELECT id FROM entry WHERE identity = ?1",
                params![identity],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            self.connection
                .execute("UPDATE entry SET at = ?2 WHERE id = ?1", params![id, at])?;
        }
        Ok(existing)
    }

    /// Records a new entry and every representation it holds.
    pub fn insert(
        &mut self,
        identity: &str,
        parts: &[Part],
        facts: &Facts,
        at: i64,
    ) -> rusqlite::Result<i64> {
        let transaction = self.connection.transaction()?;

        let total: u64 = parts.iter().map(|part| part.bytes).sum();
        let sample = facts.preview.as_deref();
        let derived = kind::classify(parts.iter().map(|part| part.mime.as_str()), sample);

        transaction.execute(
            "INSERT INTO entry (identity, kind, bytes, preview, width, height, thumb,
                                pinned, source, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)",
            params![
                identity,
                derived.as_str(),
                total as i64,
                facts.preview,
                facts.width,
                facts.height,
                facts.thumb.as_ref().map(|thumb| &thumb.digest),
                facts.source,
                at,
            ],
        )?;
        let id = transaction.last_insert_rowid();

        for part in parts {
            transaction.execute(
                "INSERT INTO part (entry, mime, digest, bytes, rank)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    part.mime,
                    part.digest,
                    part.bytes as i64,
                    kind::rank(&part.mime)
                ],
            )?;
            reference(&transaction, &part.digest, part.bytes)?;
        }

        if let Some(thumb) = &facts.thumb {
            reference(&transaction, &thumb.digest, thumb.bytes)?;
        }

        if let Some(body) = &facts.body {
            transaction.execute(
                "INSERT INTO entry_fts (rowid, body) VALUES (?1, ?2)",
                params![id, body],
            )?;
        }

        transaction.commit()?;
        Ok(id)
    }

    pub fn summary(&self, id: i64) -> rusqlite::Result<Option<Summary>> {
        let found = self
            .connection
            .query_row(
                "SELECT id, kind, bytes, preview, width, height, thumb, pinned, source, at
                 FROM entry WHERE id = ?1",
                params![id],
                read_summary,
            )
            .optional()?;

        match found {
            Some(mut summary) => {
                summary.mimes = self.mimes(id)?;
                Ok(Some(summary))
            }
            None => Ok(None),
        }
    }

    fn mimes(&self, id: i64) -> rusqlite::Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT mime FROM part WHERE entry = ?1 ORDER BY rank, mime")?;
        let rows = statement.query_map(params![id], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Entries for one window of the UI's list, pinned first and newest first.
    pub fn list(
        &self,
        limit: u32,
        before: Option<i64>,
        query: Option<&str>,
        only: Option<Kind>,
    ) -> rusqlite::Result<Vec<Summary>> {
        let limit = limit.clamp(1, MAX_LIMIT);
        let matching = query.map(fts_query).filter(|text| !text.is_empty());

        // Built by branch rather than by string concatenation so every value
        // stays a bound parameter.
        let mut summaries = match (&matching, only) {
            (None, None) => self.select(
                "SELECT id, kind, bytes, preview, width, height, thumb, pinned, source, at
                 FROM entry
                 WHERE (?1 IS NULL OR id < ?1)
                 ORDER BY pinned DESC, at DESC, id DESC LIMIT ?2",
                params![before, limit],
            )?,
            (None, Some(kind)) => self.select(
                "SELECT id, kind, bytes, preview, width, height, thumb, pinned, source, at
                 FROM entry
                 WHERE (?1 IS NULL OR id < ?1) AND kind = ?3
                 ORDER BY pinned DESC, at DESC, id DESC LIMIT ?2",
                params![before, limit, kind.as_str()],
            )?,
            (Some(text), None) => self.select(
                "SELECT e.id, e.kind, e.bytes, e.preview, e.width, e.height, e.thumb,
                        e.pinned, e.source, e.at
                 FROM entry e JOIN entry_fts f ON f.rowid = e.id
                 WHERE f.body MATCH ?3 AND (?1 IS NULL OR e.id < ?1)
                 ORDER BY e.pinned DESC, e.at DESC, e.id DESC LIMIT ?2",
                params![before, limit, text],
            )?,
            (Some(text), Some(kind)) => self.select(
                "SELECT e.id, e.kind, e.bytes, e.preview, e.width, e.height, e.thumb,
                        e.pinned, e.source, e.at
                 FROM entry e JOIN entry_fts f ON f.rowid = e.id
                 WHERE f.body MATCH ?3 AND (?1 IS NULL OR e.id < ?1) AND e.kind = ?4
                 ORDER BY e.pinned DESC, e.at DESC, e.id DESC LIMIT ?2",
                params![before, limit, text, kind.as_str()],
            )?,
        };

        for summary in &mut summaries {
            summary.mimes = self.mimes(summary.id)?;
        }
        Ok(summaries)
    }

    fn select(
        &self,
        sql: &str,
        arguments: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Summary>> {
        let mut statement = self.connection.prepare_cached(sql)?;
        let rows = statement.query_map(arguments, read_summary)?;
        rows.collect()
    }

    /// The blob backing one representation, or the preferred one when no mime
    /// is named.
    pub fn locate(
        &self,
        entry: i64,
        mime: Option<&str>,
    ) -> rusqlite::Result<Option<(String, String, u64)>> {
        let row = match mime {
            Some(mime) => self
                .connection
                .query_row(
                    "SELECT mime, digest, bytes FROM part WHERE entry = ?1 AND mime = ?2",
                    params![entry, mime],
                    read_location,
                )
                .optional()?,
            None => self
                .connection
                .query_row(
                    "SELECT mime, digest, bytes FROM part WHERE entry = ?1
                     ORDER BY rank, mime LIMIT 1",
                    params![entry],
                    read_location,
                )
                .optional()?,
        };
        Ok(row)
    }

    /// The best-ranked representation whose mime starts with `prefix`.
    ///
    /// `LIKE` metacharacters in the prefix are escaped, so a lookup only ever
    /// matches a real prefix.
    pub fn locate_prefixed(
        &self,
        entry: i64,
        prefix: &str,
    ) -> rusqlite::Result<Option<(String, String, u64)>> {
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let row = self
            .connection
            .query_row(
                "SELECT mime, digest, bytes FROM part WHERE entry = ?1
                 AND mime LIKE ?2 ESCAPE '\\' ORDER BY rank, mime LIMIT 1",
                params![entry, pattern],
                read_location,
            )
            .optional()?;
        Ok(row)
    }

    pub fn thumb(&self, entry: i64) -> rusqlite::Result<Option<(String, u64)>> {
        self.connection
            .query_row(
                "SELECT b.digest, b.bytes FROM entry e JOIN blob b ON b.digest = e.thumb
                 WHERE e.id = ?1",
                params![entry],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
            )
            .optional()
    }

    pub fn set_pinned(&self, entry: i64, pinned: bool) -> rusqlite::Result<bool> {
        let changed = self.connection.execute(
            "UPDATE entry SET pinned = ?2 WHERE id = ?1",
            params![entry, pinned as i32],
        )?;
        Ok(changed > 0)
    }

    /// Deletes one entry, returning the digests no record points at any more.
    pub fn remove(&mut self, entry: i64) -> rusqlite::Result<Vec<String>> {
        let transaction = self.connection.transaction()?;
        let orphaned = release_entry(&transaction, entry)?;
        let existed = transaction.execute("DELETE FROM entry WHERE id = ?1", params![entry])?;
        transaction.execute("DELETE FROM entry_fts WHERE rowid = ?1", params![entry])?;
        transaction.commit()?;

        Ok(if existed > 0 { orphaned } else { Vec::new() })
    }

    /// Deletes everything not pinned.
    pub fn clear(&mut self) -> rusqlite::Result<Vec<String>> {
        let transaction = self.connection.transaction()?;
        let doomed: Vec<i64> = {
            let mut statement = transaction.prepare("SELECT id FROM entry WHERE pinned = 0")?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let mut orphaned = Vec::new();
        for entry in doomed {
            orphaned.extend(release_entry(&transaction, entry)?);
            transaction.execute("DELETE FROM entry WHERE id = ?1", params![entry])?;
            transaction.execute("DELETE FROM entry_fts WHERE rowid = ?1", params![entry])?;
        }
        transaction.commit()?;
        Ok(orphaned)
    }

    /// Drops the oldest unpinned entries until the store fits in `budget`.
    ///
    /// Returns the orphaned digests and the ids that were removed, so the
    /// daemon can tell watching clients what disappeared under them.
    pub fn evict_to(&mut self, budget: i64) -> rusqlite::Result<(Vec<String>, Vec<i64>)> {
        let mut orphaned = Vec::new();
        let mut removed = Vec::new();

        loop {
            if self.bytes()? <= budget {
                break;
            }
            // Oldest first, and never a pinned one: pinning is the user saying
            // this outlives the budget.
            let victim: Option<i64> = self
                .connection
                .query_row(
                    "SELECT id FROM entry WHERE pinned = 0 ORDER BY at ASC, id ASC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;

            // Everything left is pinned. The budget is a policy for what the
            // daemon collects on its own, not a licence to drop what was
            // explicitly kept.
            let Some(victim) = victim else { break };

            orphaned.extend(self.remove(victim)?);
            removed.push(victim);
        }

        Ok((orphaned, removed))
    }

    pub fn entries(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM entry", [], |row| row.get(0))
    }

    pub fn bytes(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("SELECT COALESCE(SUM(bytes), 0) FROM blob", [], |row| {
                row.get(0)
            })
    }

    /// Digests the index knows about, for reconciling against the disk.
    pub fn known_digests(&self) -> rusqlite::Result<Vec<String>> {
        let mut statement = self.connection.prepare("SELECT digest FROM blob")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }
}

/// Adds one reference to a blob, recording its size the first time it is seen.
fn reference(connection: &Connection, digest: &str, bytes: u64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO blob (digest, bytes, refs) VALUES (?1, ?2, 1)
         ON CONFLICT(digest) DO UPDATE SET refs = refs + 1",
        params![digest, bytes as i64],
    )?;
    Ok(())
}

/// Drops every reference one entry holds, returning the digests that fell to
/// zero and whose files the caller should now delete.
fn release_entry(connection: &Connection, entry: i64) -> rusqlite::Result<Vec<String>> {
    let mut digests: Vec<String> = {
        let mut statement = connection.prepare("SELECT digest FROM part WHERE entry = ?1")?;
        let rows = statement.query_map(params![entry], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let thumb: Option<String> = connection
        .query_row(
            "SELECT thumb FROM entry WHERE id = ?1 AND thumb IS NOT NULL",
            params![entry],
            |row| row.get(0),
        )
        .optional()?;
    digests.extend(thumb);

    let mut orphaned = Vec::new();
    for digest in digests {
        connection.execute(
            "UPDATE blob SET refs = refs - 1 WHERE digest = ?1",
            params![&digest],
        )?;
        // A row that is already gone counts as zero: the entry is being
        // deleted either way, and failing here would abort the transaction
        // over an inconsistency the delete itself repairs.
        let remaining: i64 = connection
            .query_row(
                "SELECT refs FROM blob WHERE digest = ?1",
                params![&digest],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if remaining <= 0 {
            connection.execute("DELETE FROM blob WHERE digest = ?1", params![&digest])?;
            orphaned.push(digest);
        }
    }

    Ok(orphaned)
}

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    let thumb: Option<String> = row.get(6)?;
    Ok(Summary {
        id: row.get(0)?,
        kind: Kind::parse(&row.get::<_, String>(1)?)
            .unwrap_or(Kind::Text)
            .as_str(),
        // Filled by the caller, which holds the statement this row came from.
        mimes: Vec::new(),
        bytes: row.get(2)?,
        preview: row.get(3)?,
        width: row.get(4)?,
        height: row.get(5)?,
        thumb: thumb.is_some(),
        pinned: row.get::<_, i64>(7)? != 0,
        source: row.get(8)?,
        at: row.get(9)?,
    })
}

fn read_location(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, u64)> {
    Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? as u64))
}

/// Turns what the user typed into an FTS5 expression.
///
/// Every token is quoted, so punctuation the user typed is searched for rather
/// than interpreted as FTS5 syntax — an unquoted `"` or `*` would otherwise
/// turn a search into a parse error. The last token gets a prefix match, which
/// is what makes the list narrow as the user is still typing.
fn fts_query(text: &str) -> String {
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty())
        .collect();

    tokens
        .iter()
        .enumerate()
        .map(|(position, token)| {
            let last = position + 1 == tokens.len();
            if last {
                format!("\"{token}\"*")
            } else {
                format!("\"{token}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn index() -> (Index, TempDir) {
        let dir = TempDir::new();
        let index = Index::open(&dir.path().join("index.db")).unwrap();
        (index, dir)
    }

    fn part(mime: &str, digest: &str, bytes: u64) -> Part {
        Part {
            mime: mime.into(),
            digest: digest.into(),
            bytes,
        }
    }

    fn text_facts(body: &str) -> Facts {
        Facts {
            preview: Some(body.into()),
            body: Some(body.into()),
            ..Facts::default()
        }
    }

    #[test]
    fn records_an_entry_with_every_representation_it_holds() {
        let (mut index, _dir) = index();
        let parts = [part("text/html", "aa", 10), part("text/plain", "bb", 4)];
        let id = index
            .insert("identity-1", &parts, &text_facts("hello"), 100)
            .unwrap();

        let summary = index.summary(id).unwrap().unwrap();
        assert_eq!(summary.bytes, 14);
        assert_eq!(summary.kind, "text");
        // Mimes come back in serving preference, richest first.
        assert_eq!(summary.mimes, vec!["text/html", "text/plain"]);
        assert!(!summary.pinned);
    }

    #[test]
    fn a_repeated_copy_moves_the_timestamp_instead_of_adding_a_row() {
        let (mut index, _dir) = index();
        index
            .insert(
                "identity-1",
                &[part("text/plain", "aa", 4)],
                &text_facts("x"),
                100,
            )
            .unwrap();

        assert!(index.touch("identity-1", 200).unwrap().is_some());
        assert_eq!(index.entries().unwrap(), 1);
        assert_eq!(index.touch("identity-2", 200).unwrap(), None);
    }

    #[test]
    fn lists_pinned_entries_before_the_merely_recent() {
        let (mut index, _dir) = index();
        let old = index
            .insert("a", &[part("text/plain", "aa", 1)], &text_facts("old"), 100)
            .unwrap();
        index
            .insert("b", &[part("text/plain", "bb", 1)], &text_facts("new"), 200)
            .unwrap();

        index.set_pinned(old, true).unwrap();
        let listed = index.list(10, None, None, None).unwrap();
        assert_eq!(listed[0].id, old, "a pinned entry outranks a newer one");
    }

    #[test]
    fn searches_text_and_narrows_on_a_prefix_as_the_user_types() {
        let (mut index, _dir) = index();
        index
            .insert(
                "a",
                &[part("text/plain", "aa", 1)],
                &text_facts("git rebase interactive"),
                100,
            )
            .unwrap();
        index
            .insert(
                "b",
                &[part("text/plain", "bb", 1)],
                &text_facts("cargo clippy"),
                200,
            )
            .unwrap();

        assert_eq!(index.list(10, None, Some("rebase"), None).unwrap().len(), 1);
        // A half-typed word still matches, which is what makes search feel live.
        assert_eq!(index.list(10, None, Some("reb"), None).unwrap().len(), 1);
        assert_eq!(
            index
                .list(10, None, Some("git rebase"), None)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            index.list(10, None, Some("nonesuch"), None).unwrap().len(),
            0
        );
    }

    #[test]
    fn punctuation_in_a_search_is_searched_for_not_executed() {
        let (mut index, _dir) = index();
        index
            .insert(
                "a",
                &[part("text/plain", "aa", 1)],
                &text_facts("quoted"),
                100,
            )
            .unwrap();

        // Each of these is FTS5 syntax that would be a parse error unquoted.
        for query in ["\"", "*", "a OR", "NEAR(", "^", "quoted\""] {
            assert!(
                index.list(10, None, Some(query), None).is_ok(),
                "query {query:?} must not fail"
            );
        }
    }

    #[test]
    fn a_shared_representation_survives_until_the_last_entry_using_it_goes() {
        let (mut index, _dir) = index();
        let first = index
            .insert(
                "a",
                &[part("text/plain", "shared", 5)],
                &text_facts("x"),
                100,
            )
            .unwrap();
        let second = index
            .insert(
                "b",
                &[part("text/plain", "shared", 5)],
                &text_facts("y"),
                200,
            )
            .unwrap();

        // The blob is counted once however many entries point at it.
        assert_eq!(index.bytes().unwrap(), 5);

        assert!(index.remove(first).unwrap().is_empty(), "still referenced");
        assert_eq!(index.remove(second).unwrap(), vec!["shared".to_string()]);
        assert_eq!(index.bytes().unwrap(), 0);
    }

    #[test]
    fn clearing_keeps_what_was_pinned() {
        let (mut index, _dir) = index();
        let kept = index
            .insert(
                "a",
                &[part("text/plain", "aa", 1)],
                &text_facts("keep"),
                100,
            )
            .unwrap();
        index
            .insert(
                "b",
                &[part("text/plain", "bb", 1)],
                &text_facts("drop"),
                200,
            )
            .unwrap();
        index.set_pinned(kept, true).unwrap();

        assert_eq!(index.clear().unwrap(), vec!["bb".to_string()]);
        assert_eq!(index.entries().unwrap(), 1);
        assert_eq!(index.summary(kept).unwrap().unwrap().id, kept);
    }

    #[test]
    fn eviction_drops_the_oldest_first_and_stops_at_the_budget() {
        let (mut index, _dir) = index();
        for (identity, digest, at) in [("a", "aa", 100), ("b", "bb", 200), ("c", "cc", 300)] {
            index
                .insert(
                    identity,
                    &[part("image/png", digest, 100)],
                    &Facts::default(),
                    at,
                )
                .unwrap();
        }
        assert_eq!(index.bytes().unwrap(), 300);

        let (orphaned, removed) = index.evict_to(150).unwrap();
        assert_eq!(orphaned, vec!["aa".to_string(), "bb".to_string()]);
        assert_eq!(removed.len(), 2);
        assert_eq!(index.bytes().unwrap(), 100);
    }

    #[test]
    fn eviction_never_takes_a_pinned_entry_even_to_meet_the_budget() {
        let (mut index, _dir) = index();
        let pinned = index
            .insert("a", &[part("image/png", "aa", 500)], &Facts::default(), 100)
            .unwrap();
        index.set_pinned(pinned, true).unwrap();

        let (orphaned, removed) = index.evict_to(10).unwrap();
        // Over budget and stays over budget: pinning outranks the policy.
        assert!(orphaned.is_empty());
        assert!(removed.is_empty());
        assert_eq!(index.bytes().unwrap(), 500);
    }

    #[test]
    fn a_list_never_returns_more_than_the_cap_however_much_is_asked_for() {
        let (mut index, _dir) = index();
        index
            .insert("a", &[part("text/plain", "aa", 1)], &text_facts("x"), 100)
            .unwrap();
        assert!(index.list(u32::MAX, None, None, None).unwrap().len() <= MAX_LIMIT as usize);
    }

    #[test]
    fn locate_prefers_the_richest_representation_unless_one_is_named() {
        let (mut index, _dir) = index();
        let id = index
            .insert(
                "a",
                &[part("text/plain", "plain", 4), part("image/png", "png", 40)],
                &Facts::default(),
                100,
            )
            .unwrap();

        let (mime, digest, _) = index.locate(id, None).unwrap().unwrap();
        assert_eq!((mime.as_str(), digest.as_str()), ("image/png", "png"));

        let (mime, digest, _) = index.locate(id, Some("text/plain")).unwrap().unwrap();
        assert_eq!((mime.as_str(), digest.as_str()), ("text/plain", "plain"));

        assert!(index.locate(id, Some("text/html")).unwrap().is_none());
    }
}
