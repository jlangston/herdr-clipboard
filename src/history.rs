use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    /// Metadata only — fetch the PNG blob with `get_image(id)`.
    Image { w: u32, h: u32, bytes: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub ts: u64,
    pub content: Content,
}

/// SQLite-backed clipboard history (WAL mode). Concurrency between the
/// watcher (appends) and picker (deletes/reads) is handled by SQLite's
/// single-writer locking plus a busy timeout — no external file lock.
pub struct HistoryStore {
    conn: Connection,
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_image_bytes: usize,
}

impl HistoryStore {
    pub fn new(
        state_dir: &Path,
        max_entries: usize,
        max_entry_bytes: usize,
        max_image_bytes: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(state_dir)?;
        let mut conn = Connection::open(state_dir.join("history.db")).map_err(io::Error::other)?;
        // BEGIN IMMEDIATE up front so a writer racing another process's
        // writer fails fast into busy_timeout's retry loop instead of
        // surfacing SQLITE_BUSY_SNAPSHOT from a deferred transaction that
        // upgraded mid-flight (which busy_timeout does not retry).
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        conn.busy_timeout(Duration::from_secs(5)).map_err(io::Error::other)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(io::Error::other)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id    INTEGER PRIMARY KEY,
                ts    INTEGER NOT NULL,
                kind  TEXT NOT NULL,
                text  TEXT,
                img   BLOB,
                img_w INTEGER,
                img_h INTEGER,
                CHECK (
                    (kind = 'text' AND text IS NOT NULL AND img IS NULL)
                    OR (kind = 'image' AND img IS NOT NULL AND text IS NULL)
                )
            );",
        )
        .map_err(io::Error::other)?;
        let store = Self { conn, max_entries, max_entry_bytes, max_image_bytes };
        store.migrate_v1_jsonl(state_dir)?;
        Ok(store)
    }

    /// Entries newest-first (by insertion order — `id` is the monotonic
    /// SQLite rowid, immune to wall-clock skew; `ts` is display-only).
    /// Image entries carry metadata only.
    pub fn load(&self) -> Vec<Entry> {
        let mut stmt = match self
            .conn
            .prepare("SELECT id, ts, kind, text, img_w, img_h, length(img) FROM entries ORDER BY id DESC")
        {
            Ok(stmt) => stmt,
            Err(e) => {
                eprintln!("herdr-clip: history query failed: {e}");
                return Vec::new();
            }
        };
        let rows = stmt.query_map([], |row| {
            let kind: String = row.get(2)?;
            let content = if kind == "text" {
                Content::Text(row.get(3)?)
            } else {
                Content::Image {
                    w: row.get(4)?,
                    h: row.get(5)?,
                    bytes: row.get::<_, i64>(6)? as usize,
                }
            };
            Ok(Entry { id: row.get(0)?, ts: row.get::<_, i64>(1)? as u64, content })
        });
        match rows {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(e) => {
                eprintln!("herdr-clip: history query failed: {e}");
                Vec::new()
            }
        }
    }

    /// Store text as the newest entry. Returns false when skipped
    /// (empty, oversized, or identical to the current newest entry).
    pub fn append_text(&self, text: &str, ts: u64) -> io::Result<bool> {
        if text.is_empty() || text.len() > self.max_entry_bytes {
            return Ok(false);
        }
        let tx = self.conn.unchecked_transaction().map_err(io::Error::other)?;
        let newest: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT kind, text FROM entries ORDER BY ts DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(io::Error::other)?;
        if matches!(&newest, Some((k, Some(t))) if k == "text" && t == text) {
            return Ok(false); // tx drops → rollback (no writes yet anyway)
        }
        tx.execute("DELETE FROM entries WHERE kind = 'text' AND text = ?1", params![text])
            .map_err(io::Error::other)?;
        tx.execute(
            "INSERT INTO entries (ts, kind, text) VALUES (?1, 'text', ?2)",
            params![ts as i64, text],
        )
        .map_err(io::Error::other)?;
        Self::enforce_cap(&tx, self.max_entries)?;
        tx.commit().map_err(io::Error::other)?;
        Ok(true)
    }

    /// Store a PNG image as the newest entry; same skip/dedup semantics as
    /// text, keyed on the encoded bytes.
    pub fn append_image(&self, png: &[u8], w: u32, h: u32, ts: u64) -> io::Result<bool> {
        if png.is_empty() || png.len() > self.max_image_bytes {
            return Ok(false);
        }
        let tx = self.conn.unchecked_transaction().map_err(io::Error::other)?;
        let newest: Option<(String, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT kind, img FROM entries ORDER BY ts DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(io::Error::other)?;
        if matches!(&newest, Some((k, Some(b))) if k == "image" && b == png) {
            return Ok(false);
        }
        tx.execute("DELETE FROM entries WHERE kind = 'image' AND img = ?1", params![png])
            .map_err(io::Error::other)?;
        tx.execute(
            "INSERT INTO entries (ts, kind, img, img_w, img_h) VALUES (?1, 'image', ?2, ?3, ?4)",
            params![ts as i64, png, w, h],
        )
        .map_err(io::Error::other)?;
        Self::enforce_cap(&tx, self.max_entries)?;
        tx.commit().map_err(io::Error::other)?;
        Ok(true)
    }

    /// PNG bytes for an image entry; None if the id is gone or not an image.
    pub fn get_image(&self, id: i64) -> io::Result<Option<Vec<u8>>> {
        self.conn
            .query_row("SELECT img FROM entries WHERE id = ?1", params![id], |r| r.get(0))
            .optional()
            .map_err(io::Error::other)
            .map(Option::flatten)
    }

    pub fn delete(&self, id: i64) -> io::Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn enforce_cap(tx: &rusqlite::Transaction, max_entries: usize) -> io::Result<()> {
        tx.execute(
            "DELETE FROM entries WHERE id IN (
                SELECT id FROM entries ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![max_entries as i64],
        )
        .map_err(io::Error::other)?;
        Ok(())
    }

    /// One-shot import of v1's history.jsonl: only when the DB is empty and
    /// the file exists; the file is renamed to .bak afterwards so it never
    /// re-imports. Corrupt lines are skipped, matching v1's load behavior.
    fn migrate_v1_jsonl(&self, state_dir: &Path) -> io::Result<()> {
        let jsonl = state_dir.join("history.jsonl");
        if !jsonl.exists() {
            return Ok(());
        }
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))
            .map_err(io::Error::other)?;
        if count == 0 {
            #[derive(serde::Deserialize)]
            struct LegacyEntry {
                ts: u64,
                text: String,
            }
            for line in fs::read_to_string(&jsonl)?.lines() {
                let Ok(e) = serde_json::from_str::<LegacyEntry>(line) else { continue };
                self.append_text(&e.text, e.ts)?;
            }
        }
        fs::rename(&jsonl, state_dir.join("history.jsonl.bak"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path) -> HistoryStore {
        HistoryStore::new(dir, 50, 256 * 1024, 5 * 1024 * 1024).unwrap()
    }

    fn texts(s: &HistoryStore) -> Vec<String> {
        s.load()
            .iter()
            .map(|e| match &e.content {
                Content::Text(t) => t.clone(),
                Content::Image { .. } => "<image>".into(),
            })
            .collect()
    }

    #[test]
    fn load_returns_empty_when_no_db() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store(dir.path()).load().is_empty());
    }

    #[test]
    fn append_then_load_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.append_text("first", 1).unwrap());
        assert!(s.append_text("second", 2).unwrap());
        let entries = s.load();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, Content::Text("second".into()));
        assert_eq!(entries[0].ts, 2);
        assert_eq!(entries[1].content, Content::Text("first".into()));
    }

    #[test]
    fn history_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).append_text("kept", 1).unwrap();
        assert_eq!(texts(&store(dir.path())), vec!["kept"]);
    }

    #[test]
    fn append_identical_to_newest_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.append_text("same", 1).unwrap());
        assert!(!s.append_text("same", 2).unwrap());
        let entries = s.load();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ts, 1, "skipped re-copy must not touch the timestamp");
    }

    #[test]
    fn append_existing_older_text_moves_it_to_front() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.append_text("a", 1).unwrap();
        s.append_text("b", 2).unwrap();
        assert!(s.append_text("a", 3).unwrap());
        assert_eq!(texts(&s), vec!["a", "b"]);
        assert_eq!(s.load()[0].ts, 3, "moved entry gets the new timestamp");
    }

    #[test]
    fn history_is_capped_dropping_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path(), 3, 256 * 1024, 1024).unwrap();
        for (i, t) in ["a", "b", "c", "d"].iter().enumerate() {
            s.append_text(t, i as u64).unwrap();
        }
        assert_eq!(texts(&s), vec!["d", "c", "b"]);
    }

    #[test]
    fn oversized_and_empty_text_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path(), 50, 10, 1024).unwrap();
        assert!(!s.append_text("", 1).unwrap());
        assert!(!s.append_text("12345678901", 2).unwrap()); // 11 bytes > 10
        assert!(s.append_text("1234567890", 3).unwrap()); // exactly 10 is fine
        assert_eq!(s.load().len(), 1);
    }

    #[test]
    fn delete_removes_entry_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.append_text("keep", 1).unwrap();
        s.append_text("drop", 2).unwrap();
        let drop_id = s.load()[0].id;
        s.delete(drop_id).unwrap();
        assert_eq!(texts(&s), vec!["keep"]);
    }

    #[test]
    fn insertion_order_wins_over_wall_clock() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.append_text("first", 100).unwrap();
        s.append_text("second", 50).unwrap(); // clock went backwards
        assert_eq!(texts(&s), vec!["second", "first"]);
    }

    #[test]
    fn image_roundtrip_metadata_and_blob() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let png = vec![1u8, 2, 3, 4, 5];
        assert!(s.append_image(&png, 10, 20, 7).unwrap());
        let entries = s.load();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, Content::Image { w: 10, h: 20, bytes: 5 });
        assert_eq!(entries[0].ts, 7);
        assert_eq!(s.get_image(entries[0].id).unwrap(), Some(png));
        assert_eq!(s.get_image(9999).unwrap(), None);
    }

    #[test]
    fn image_dedup_and_oversize_mirror_text_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path(), 50, 1024, 4).unwrap();
        assert!(!s.append_image(&[0; 5], 1, 1, 1).unwrap()); // 5 bytes > 4
        assert!(!s.append_image(&[], 1, 1, 1).unwrap());
        assert!(s.append_image(&[1, 2, 3], 1, 1, 2).unwrap());
        assert!(!s.append_image(&[1, 2, 3], 1, 1, 3).unwrap()); // identical to newest
        assert_eq!(s.load()[0].ts, 2);
        s.append_text("interleaved", 4).unwrap();
        assert!(s.append_image(&[1, 2, 3], 1, 1, 5).unwrap()); // move-to-front
        assert_eq!(s.load().len(), 2);
        assert_eq!(s.load()[0].content, Content::Image { w: 1, h: 1, bytes: 3 });
        assert_eq!(s.load()[0].ts, 5);
    }

    #[test]
    fn text_and_images_share_the_entry_cap() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path(), 2, 1024, 1024).unwrap();
        s.append_text("old", 1).unwrap();
        s.append_image(&[9], 1, 1, 2).unwrap();
        s.append_text("new", 3).unwrap();
        assert_eq!(s.load().len(), 2);
        assert_eq!(texts(&s), vec!["new", "<image>"]);
    }

    #[test]
    fn migrates_v1_jsonl_once_and_renames_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("history.jsonl"),
            "{\"ts\":1,\"text\":\"old a\"}\n{not json\n{\"ts\":2,\"text\":\"old b\"}\n",
        )
        .unwrap();
        let s = store(dir.path());
        assert_eq!(texts(&s), vec!["old b", "old a"], "corrupt line skipped, order kept");
        assert!(!dir.path().join("history.jsonl").exists());
        assert!(dir.path().join("history.jsonl.bak").exists());
        // Reopening must not re-import from the .bak
        s.append_text("new", 3).unwrap();
        assert_eq!(texts(&store(dir.path())), vec!["new", "old b", "old a"]);
    }

    #[test]
    fn no_migration_when_db_already_has_data() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).append_text("existing", 1).unwrap();
        std::fs::write(dir.path().join("history.jsonl"), "{\"ts\":9,\"text\":\"stale\"}\n").unwrap();
        assert_eq!(texts(&store(dir.path())), vec!["existing"]);
    }
}
