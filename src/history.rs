use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: u64,
    pub text: String,
}

/// JSONL-backed clipboard history. On disk the file is oldest-first;
/// the public API is newest-first. Mutations take an exclusive file lock
/// (watcher appends and picker deletes can race) and write via
/// temp-file + rename so a crash can't truncate history.
pub struct HistoryStore {
    path: PathBuf,
    lock_path: PathBuf,
    pub max_entries: usize,
    pub max_entry_bytes: usize,
}

impl HistoryStore {
    pub fn new(state_dir: &Path, max_entries: usize, max_entry_bytes: usize) -> std::io::Result<Self> {
        fs::create_dir_all(state_dir)?;
        Ok(Self {
            path: state_dir.join("history.jsonl"),
            lock_path: state_dir.join("history.lock"),
            max_entries,
            max_entry_bytes,
        })
    }

    /// Entries newest-first. Missing file or unparseable lines are not errors.
    pub fn load(&self) -> Vec<Entry> {
        let Ok(file) = File::open(&self.path) else { return Vec::new() };
        let mut entries: Vec<Entry> = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect();
        entries.reverse();
        entries
    }

    /// Store `text` as the newest entry. Returns false when skipped
    /// (empty, oversized, or identical to the current newest entry).
    pub fn append(&self, text: &str, ts: u64) -> std::io::Result<bool> {
        if text.is_empty() || text.len() > self.max_entry_bytes {
            return Ok(false);
        }
        let _lock = self.lock()?;
        let mut entries = self.load();
        if entries.first().is_some_and(|e| e.text == text) {
            return Ok(false);
        }
        entries.retain(|e| e.text != text); // move-to-front dedup
        entries.insert(0, Entry { ts, text: text.to_string() });
        entries.truncate(self.max_entries);
        self.write_all(&entries)?;
        Ok(true)
    }

    fn write_all(&self, newest_first: &[Entry]) -> std::io::Result<()> {
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut f = File::create(&tmp)?;
        for e in newest_first.iter().rev() {
            writeln!(f, "{}", serde_json::to_string(e).expect("entry serializes"))?;
        }
        f.sync_all()?;
        fs::rename(&tmp, &self.path)
    }

    /// Exclusive advisory lock; released when the returned File drops.
    fn lock(&self) -> std::io::Result<File> {
        let f = OpenOptions::new().create(true).write(true).open(&self.lock_path)?;
        f.try_lock_exclusive().or_else(|_| f.lock_exclusive())?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path) -> HistoryStore {
        HistoryStore::new(dir, 50, 256 * 1024).unwrap()
    }

    #[test]
    fn load_returns_empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store(dir.path()).load().is_empty());
    }

    #[test]
    fn append_then_load_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.append("first", 1).unwrap());
        assert!(s.append("second", 2).unwrap());
        let entries = s.load();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], Entry { ts: 2, text: "second".into() });
        assert_eq!(entries[1], Entry { ts: 1, text: "first".into() });
    }

    #[test]
    fn history_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).append("kept", 1).unwrap();
        assert_eq!(store(dir.path()).load()[0].text, "kept");
    }
}
