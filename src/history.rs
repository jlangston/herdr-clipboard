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
        let conn = Connection::open(state_dir.join("history.db")).map_err(io::Error::other)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(io::Error::other)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(io::Error::other)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id    INTEGER PRIMARY KEY,
                ts    INTEGER NOT NULL,
                kind  TEXT NOT NULL CHECK (kind IN ('text','image')),
                text  TEXT,
                img   BLOB,
                img_w INTEGER,
                img_h INTEGER
            );",
        )
        .map_err(io::Error::other)?;
        Ok(Self { conn, max_entries, max_entry_bytes, max_image_bytes })
    }

    /// Entries newest-first; image entries carry metadata only.
    pub fn load(&self) -> Vec<Entry> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, ts, kind, text, img_w, img_h, length(img)
             FROM entries ORDER BY ts DESC, id DESC",
        ) else {
            return Vec::new();
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
            Err(_) => Vec::new(),
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

    pub fn delete(&self, id: i64) -> io::Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn enforce_cap(tx: &rusqlite::Transaction, max_entries: usize) -> io::Result<()> {
        tx.execute(
            "DELETE FROM entries WHERE id IN (
                SELECT id FROM entries ORDER BY ts DESC, id DESC LIMIT -1 OFFSET ?1
            )",
            params![max_entries as i64],
        )
        .map_err(io::Error::other)?;
        Ok(())
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
}
