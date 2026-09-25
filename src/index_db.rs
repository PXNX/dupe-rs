use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One file captured by a reverse-search indexing pass. Persisted to disk so
/// it can be searched later even if the drive it lives on isn't currently
/// attached — which is why `volume_label` and `drive_letter` are stored
/// separately from `rel_path` rather than just keeping a full absolute path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IndexedFile {
    pub volume_label: String,
    pub drive_letter: String,
    pub rel_path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

impl IndexedFile {
    /// Best-effort absolute path, only valid if `drive_letter` is currently
    /// mounted as the same drive it was indexed under.
    pub fn absolute_path(&self) -> PathBuf {
        Path::new(&format!("{}\\", self.drive_letter)).join(&self.rel_path)
    }
}

/// Hex-encodes a content hash for use as an `IndexDb` key — `serde_json`
/// requires string map keys, so the raw `[u8; 32]` blake3 hash can't be used
/// directly.
pub fn hex_encode(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// A persisted, drive-spanning index of file hashes, built by one or more
/// indexing passes (see `scanner::indexer::run_index_scan`) and queried by
/// reverse search: given one file's hash, find every other indexed copy of
/// it, on any drive that's ever been indexed.
#[derive(Default, Serialize, Deserialize)]
pub struct IndexDb {
    entries: HashMap<String, Vec<IndexedFile>>,
}

impl IndexDb {
    /// Loads the index from disk, or an empty one if the file doesn't exist
    /// yet or can't be parsed (e.g. from an incompatible older version).
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        std::fs::write(path, bytes)
    }

    /// Replaces every previously indexed entry for the *volume* identified by
    /// `drive_letter` + `volume_label` with `new_entries`, so re-indexing a
    /// drive doesn't accumulate stale duplicates of files that were since
    /// moved, renamed, or deleted. Keying on the label as well as the letter
    /// matters because a drive letter can end up hosting a different physical
    /// disk later (e.g. plugging a different drive into `D:`) — that must not
    /// silently erase the previous disk's entries, since it may still be
    /// findable later under another letter.
    pub fn reindex_volume(
        &mut self,
        drive_letter: &str,
        volume_label: &str,
        new_entries: Vec<(String, IndexedFile)>,
    ) {
        for files in self.entries.values_mut() {
            files.retain(|f| !(f.drive_letter == drive_letter && f.volume_label == volume_label));
        }
        self.entries.retain(|_, files| !files.is_empty());
        for (hash_hex, file) in new_entries {
            self.entries.entry(hash_hex).or_default().push(file);
        }
    }

    /// Adds `new_entries` without touching the rest of their volume (unlike
    /// `reindex_volume`), replacing any older entry recorded at the same
    /// volume + relative path, since that file's content may have changed.
    pub fn upsert(&mut self, new_entries: Vec<(String, IndexedFile)>) {
        let key = |f: &IndexedFile| (f.drive_letter.clone(), f.volume_label.clone(), f.rel_path.clone());
        let replaced: HashSet<_> = new_entries.iter().map(|(_, f)| key(f)).collect();
        for files in self.entries.values_mut() {
            files.retain(|f| !replaced.contains(&key(f)));
        }
        self.entries.retain(|_, files| !files.is_empty());
        for (hash_hex, file) in new_entries {
            self.entries.entry(hash_hex).or_default().push(file);
        }
    }

    /// Every indexed file with the given content hash, excluding `exclude`
    /// itself (matched by volume identity + relative path, since a relative
    /// path alone can collide across different volumes/drive letters).
    pub fn matches(&self, hash_hex: &str, exclude: &IndexedFile) -> Vec<&IndexedFile> {
        self.entries
            .get(hash_hex)
            .into_iter()
            .flatten()
            .filter(|f| {
                !(f.drive_letter == exclude.drive_letter
                    && f.volume_label == exclude.volume_label
                    && f.rel_path == exclude.rel_path)
            })
            .collect()
    }

    /// Unique (drive_letter, volume_label) pairs currently represented in the
    /// index, for showing the user what's been indexed so far. A drive letter
    /// can appear more than once here if it has hosted different labeled
    /// volumes across separate indexing passes.
    pub fn indexed_drives(&self) -> Vec<(String, String)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for files in self.entries.values() {
            for f in files {
                let key = (f.drive_letter.clone(), f.volume_label.clone());
                if seen.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
        out
    }

    pub fn total_files(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    pub fn default_path() -> PathBuf {
        let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("dupe-rs").join("reverse_index.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn file(drive: &str, rel: &str) -> IndexedFile {
        file_on(drive, &format!("{drive}-label"), rel)
    }

    fn file_on(drive: &str, label: &str, rel: &str) -> IndexedFile {
        IndexedFile {
            volume_label: label.to_string(),
            drive_letter: drive.to_string(),
            rel_path: PathBuf::from(rel),
            size: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    #[test]
    fn reindexing_a_volume_replaces_its_old_entries_but_keeps_other_drives() {
        let mut db = IndexDb::default();
        db.reindex_volume("D:", "D:-label", vec![("hash1".to_string(), file("D:", "old.jpg"))]);
        db.reindex_volume("E:", "E:-label", vec![("hash2".to_string(), file("E:", "keep.jpg"))]);

        db.reindex_volume("D:", "D:-label", vec![("hash3".to_string(), file("D:", "new.jpg"))]);

        assert!(db.matches("hash1", &file("D:", "nonexistent")).is_empty());
        assert_eq!(db.matches("hash3", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(db.matches("hash2", &file("E:", "nonexistent")).len(), 1);
    }

    #[test]
    fn reindexing_a_letter_under_a_different_volume_label_keeps_the_old_volumes_entries() {
        // Simulates unplugging one physical drive and plugging a different
        // one into the same letter: re-indexing "D:" under the new label
        // "data" must not wipe out what was indexed while "D:" was labeled
        // "500GB-5", since that disk might still be found again later.
        let mut db = IndexDb::default();
        db.reindex_volume(
            "D:",
            "500GB-5",
            vec![("hash1".to_string(), file_on("D:", "500GB-5", "old.jpg"))],
        );

        db.reindex_volume(
            "D:",
            "data",
            vec![("hash2".to_string(), file_on("D:", "data", "new.jpg"))],
        );

        assert_eq!(db.matches("hash1", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(db.matches("hash2", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(
            db.indexed_drives().len(),
            2,
            "both volumes that have ever lived on D: should still be listed"
        );
    }

    #[test]
    fn matches_excludes_the_queried_file_itself() {
        let mut db = IndexDb::default();
        db.reindex_volume(
            "D:",
            "D:-label",
            vec![
                ("hash1".to_string(), file("D:", "a.jpg")),
                ("hash1".to_string(), file("D:", "b.jpg")),
            ],
        );

        let results = db.matches("hash1", &file("D:", "a.jpg"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rel_path, PathBuf::from("b.jpg"));
    }

    #[test]
    fn matches_does_not_exclude_a_same_named_file_on_a_different_volume() {
        // Two different physical drives can share a drive letter over time
        // and happen to contain a file at the same relative path; the
        // "exclude self" filter must key off the volume label too, not just
        // the letter + path, or it would wrongly drop a real match.
        let mut db = IndexDb::default();
        db.reindex_volume(
            "D:",
            "data",
            vec![("hash1".to_string(), file_on("D:", "data", "a.jpg"))],
        );

        let results = db.matches("hash1", &file_on("D:", "500GB-5", "a.jpg"));
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn upsert_adds_files_and_replaces_same_path_without_wiping_the_volume() {
        let mut db = IndexDb::default();
        db.reindex_volume("D:", "D:-label", vec![("old".to_string(), file("D:", "a.jpg"))]);
        db.reindex_volume("D:", "D:-label", vec![
            ("old".to_string(), file("D:", "a.jpg")),
            ("keep".to_string(), file("D:", "keep.jpg")),
        ]);

        db.upsert(vec![
            ("new".to_string(), file("D:", "a.jpg")),
            ("added".to_string(), file("D:", "b.jpg")),
        ]);

        let probe = file("D:", "nonexistent");
        assert!(db.matches("old", &probe).is_empty());
        assert_eq!(db.matches("new", &probe).len(), 1);
        assert_eq!(db.matches("added", &probe).len(), 1);
        assert_eq!(db.matches("keep", &probe).len(), 1);
        assert_eq!(db.total_files(), 3);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");

        let mut db = IndexDb::default();
        db.reindex_volume("D:", "D:-label", vec![("hash1".to_string(), file("D:", "a.jpg"))]);
        db.save(&path).unwrap();

        let loaded = IndexDb::load(&path);
        assert_eq!(loaded.total_files(), 1);
        assert_eq!(loaded.matches("hash1", &file("D:", "nonexistent")).len(), 1);
    }

    #[test]
    fn load_missing_file_returns_empty_db() {
        let db = IndexDb::load(Path::new("this/path/does/not/exist.json"));
        assert_eq!(db.total_files(), 0);
    }
}
