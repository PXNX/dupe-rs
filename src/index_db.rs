use crate::volume::{VolumeInfo, volume_key};
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One file captured by a reverse-search indexing pass. Persisted to disk so
/// it can be searched later even if the drive it lives on isn't currently
/// attached — which is why the volume's identity (label + serial) and the
/// letter it was mounted as are stored separately from `rel_path` rather than
/// just keeping a full absolute path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IndexedFile {
    pub volume_label: String,
    pub drive_letter: String,
    /// `None` for entries indexed before serials were recorded; those are
    /// told apart by letter + label until their drive is seen again (see
    /// `IndexDb::adopt_legacy`).
    #[serde(default)]
    pub volume_serial: Option<u32>,
    pub rel_path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

impl IndexedFile {
    /// Best-effort absolute path, only valid if `drive_letter` is currently
    /// mounted as the same drive it was indexed under.
    pub fn absolute_path(&self) -> PathBuf {
        self.absolute_path_on(&self.drive_letter)
    }

    /// The file's path if its volume is mounted as `drive_letter` now.
    pub fn absolute_path_on(&self, drive_letter: &str) -> PathBuf {
        Path::new(&format!("{drive_letter}\\")).join(&self.rel_path)
    }

    /// The volume this file lives on (see `volume::volume_key`).
    pub fn volume_key(&self) -> String {
        volume_key(&self.drive_letter, &self.volume_label, self.volume_serial)
    }

    fn is_legacy_entry_of(&self, volume: &VolumeInfo) -> bool {
        self.volume_serial.is_none()
            && self.drive_letter == volume.drive_letter
            && self.volume_label == volume.label
    }
}

/// How full a volume was the last time it was indexed or filled, so the
/// Drives tab can show each drive's usage even while it's unplugged.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct VolumeUsage {
    pub total: u64,
    pub free: u64,
    pub recorded_at: SystemTime,
}

impl VolumeUsage {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f32 / self.total as f32
        }
    }
}

/// One volume represented in the index, with totals over its entries.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedVolume {
    pub key: String,
    pub label: String,
    pub serial: Option<u32>,
    /// The letter it was mounted as when (some of) its files were indexed.
    pub drive_letter: String,
    pub files: usize,
    pub bytes: u64,
}

/// What two volumes hold that the other doesn't, compared by content hash
/// (so a renamed or moved file still counts as present on both).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContentDiff {
    pub only_a_files: usize,
    pub only_a_bytes: u64,
    pub only_b_files: usize,
    pub only_b_bytes: u64,
}

impl ContentDiff {
    pub fn in_sync(&self) -> bool {
        self.only_a_files == 0 && self.only_b_files == 0
    }
}

/// Where the index and drive list are kept: `%LOCALAPPDATA%\dupe-rs`, or
/// `DUPE_RS_DATA_DIR` if set (which tests use to stay away from the real
/// files).
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DUPE_RS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("dupe-rs")
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
    /// Per-volume usage as recorded by older versions, keyed by letter +
    /// label. Only read, so it can be handed to the drive registry (see
    /// `drives::DriveRegistry`), which keeps usage now.
    #[serde(default, rename = "volumes", skip_serializing)]
    legacy_usage: HashMap<String, VolumeUsage>,
    /// Bumped on every change, so callers can cache what they derive.
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    volumes_cache: OnceCell<Vec<IndexedVolume>>,
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

    fn changed(&mut self) {
        self.generation += 1;
        self.volumes_cache = OnceCell::new();
    }

    /// Changes whenever the index does.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Gives entries indexed before serials were recorded (matched by the
    /// letter + label they were indexed under) the serial of `volume`, which
    /// is mounted under that letter with that label right now — the same
    /// assumption those entries were always looked up by. Returns whether
    /// any entry changed.
    pub fn adopt_legacy(&mut self, volume: &VolumeInfo) -> bool {
        if volume.serial.is_none() {
            return false;
        }
        let mut adopted = false;
        for f in self.entries.values_mut().flatten() {
            if f.is_legacy_entry_of(volume) {
                f.volume_serial = volume.serial;
                adopted = true;
            }
        }
        if adopted {
            self.changed();
        }
        adopted
    }

    /// Replaces every previously indexed entry for `volume` with
    /// `new_entries`, so re-indexing a drive doesn't accumulate stale
    /// duplicates of files that were since moved, renamed, or deleted.
    /// Matching on the volume's identity rather than its letter matters
    /// because a letter can end up hosting a different physical disk later
    /// (e.g. plugging a different drive into `D:`) — that must not silently
    /// erase the previous disk's entries, since it may still be findable
    /// later under another letter.
    pub fn reindex_volume(&mut self, volume: &VolumeInfo, new_entries: Vec<(String, IndexedFile)>) {
        self.adopt_legacy(volume);
        let key = volume.key();
        for files in self.entries.values_mut() {
            files.retain(|f| f.volume_key() != key);
        }
        self.entries.retain(|_, files| !files.is_empty());
        for (hash_hex, file) in new_entries {
            self.entries.entry(hash_hex).or_default().push(file);
        }
        self.changed();
    }

    /// Adds `new_entries` without touching the rest of their volume (unlike
    /// `reindex_volume`), replacing any older entry recorded at the same
    /// volume + relative path, since that file's content may have changed.
    pub fn upsert(&mut self, new_entries: Vec<(String, IndexedFile)>) {
        let volumes: HashSet<_> = new_entries
            .iter()
            .map(|(_, f)| (f.drive_letter.clone(), f.volume_label.clone(), f.volume_serial))
            .collect();
        for (drive_letter, label, serial) in volumes {
            self.adopt_legacy(&VolumeInfo {
                drive_letter,
                label,
                serial,
            });
        }
        let key = |f: &IndexedFile| (f.volume_key(), f.rel_path.clone());
        let replaced: HashSet<_> = new_entries.iter().map(|(_, f)| key(f)).collect();
        for files in self.entries.values_mut() {
            files.retain(|f| !replaced.contains(&key(f)));
        }
        self.entries.retain(|_, files| !files.is_empty());
        for (hash_hex, file) in new_entries {
            self.entries.entry(hash_hex).or_default().push(file);
        }
        self.changed();
    }

    /// Drops every entry of the volume stored under `key`.
    pub fn forget_volume(&mut self, key: &str) {
        for files in self.entries.values_mut() {
            files.retain(|f| f.volume_key() != key);
        }
        self.entries.retain(|_, files| !files.is_empty());
        self.changed();
    }

    /// Every indexed file with the given content hash, excluding `exclude`
    /// itself (matched by volume identity + relative path, since a relative
    /// path alone can collide across different volumes).
    pub fn matches(&self, hash_hex: &str, exclude: &IndexedFile) -> Vec<&IndexedFile> {
        let exclude_key = exclude.volume_key();
        self.entries
            .get(hash_hex)
            .into_iter()
            .flatten()
            .filter(|f| !(f.rel_path == exclude.rel_path && f.volume_key() == exclude_key))
            .collect()
    }

    /// The volumes currently represented in the index, sorted by label, with
    /// file and byte totals. Computed once per change of the index.
    pub fn volumes(&self) -> &[IndexedVolume] {
        self.volumes_cache.get_or_init(|| {
            let mut by_key: HashMap<String, IndexedVolume> = HashMap::new();
            for f in self.entries.values().flatten() {
                let v = by_key.entry(f.volume_key()).or_insert_with(|| IndexedVolume {
                    key: f.volume_key(),
                    label: f.volume_label.clone(),
                    serial: f.volume_serial,
                    drive_letter: f.drive_letter.clone(),
                    files: 0,
                    bytes: 0,
                });
                v.files += 1;
                v.bytes += f.size;
            }
            let mut out: Vec<_> = by_key.into_values().collect();
            out.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.key.cmp(&b.key)));
            out
        })
    }

    /// Compares the content of the volumes stored under keys `a` and `b`
    /// (see `ContentDiff`). Each indexed copy counts, so two copies of some
    /// content on `a` and none on `b` count as two files.
    pub fn content_diff(&self, a: &str, b: &str) -> ContentDiff {
        let mut diff = ContentDiff::default();
        for files in self.entries.values() {
            let on_a = files.iter().any(|f| f.volume_key() == a);
            let on_b = files.iter().any(|f| f.volume_key() == b);
            if on_a == on_b {
                continue;
            }
            let only = if on_a { a } else { b };
            for f in files.iter().filter(|f| f.volume_key() == only) {
                if on_a {
                    diff.only_a_files += 1;
                    diff.only_a_bytes += f.size;
                } else {
                    diff.only_b_files += 1;
                    diff.only_b_bytes += f.size;
                }
            }
        }
        diff
    }

    /// Usage recorded by older versions, keyed by `volume::volume_key` (in
    /// its letter + label form), handed over once to be kept elsewhere.
    pub fn take_legacy_usage(&mut self) -> HashMap<String, VolumeUsage> {
        std::mem::take(&mut self.legacy_usage)
    }

    pub fn total_files(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    pub fn default_path() -> PathBuf {
        data_dir().join("reverse_index.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SERIAL_D: u32 = 0xD000_0001;

    fn vol(drive: &str, label: &str, serial: Option<u32>) -> VolumeInfo {
        VolumeInfo {
            drive_letter: drive.to_string(),
            label: label.to_string(),
            serial,
        }
    }

    fn d() -> VolumeInfo {
        vol("D:", "D:-label", Some(SERIAL_D))
    }

    fn file(drive: &str, rel: &str) -> IndexedFile {
        let serial = (drive == "D:").then_some(SERIAL_D).or(Some(0xE000_0001));
        file_on(&vol(drive, &format!("{drive}-label"), serial), rel)
    }

    fn file_on(volume: &VolumeInfo, rel: &str) -> IndexedFile {
        IndexedFile {
            volume_label: volume.label.clone(),
            drive_letter: volume.drive_letter.clone(),
            volume_serial: volume.serial,
            rel_path: PathBuf::from(rel),
            size: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    fn e() -> VolumeInfo {
        vol("E:", "E:-label", Some(0xE000_0001))
    }

    #[test]
    fn reindexing_a_volume_replaces_its_old_entries_but_keeps_other_drives() {
        let mut db = IndexDb::default();
        db.reindex_volume(&d(), vec![("hash1".to_string(), file("D:", "old.jpg"))]);
        db.reindex_volume(&e(), vec![("hash2".to_string(), file("E:", "keep.jpg"))]);

        db.reindex_volume(&d(), vec![("hash3".to_string(), file("D:", "new.jpg"))]);

        assert!(db.matches("hash1", &file("D:", "nonexistent")).is_empty());
        assert_eq!(db.matches("hash3", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(db.matches("hash2", &file("E:", "nonexistent")).len(), 1);
    }

    #[test]
    fn reindexing_a_letter_under_a_different_volume_keeps_the_old_volumes_entries() {
        // Simulates unplugging one physical drive and plugging a different
        // one into the same letter: re-indexing "D:" for the new volume must
        // not wipe out what was indexed from the old one, since that disk
        // might still be found again later.
        let old = vol("D:", "500GB-5", Some(1));
        let new = vol("D:", "data", Some(2));
        let mut db = IndexDb::default();
        db.reindex_volume(&old, vec![("hash1".to_string(), file_on(&old, "old.jpg"))]);

        db.reindex_volume(&new, vec![("hash2".to_string(), file_on(&new, "new.jpg"))]);

        assert_eq!(db.matches("hash1", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(db.matches("hash2", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(
            db.volumes().len(),
            2,
            "both volumes that have ever lived on D: should still be listed"
        );
    }

    #[test]
    fn same_label_on_two_drives_is_told_apart_by_serial() {
        // Mirrored drives are often labeled alike, and USB drives swap
        // letters; neither may merge them.
        let a = vol("E:", "Backup", Some(1));
        let b = vol("E:", "Backup", Some(2));
        let mut db = IndexDb::default();
        db.reindex_volume(&a, vec![("hash1".to_string(), file_on(&a, "x.jpg"))]);
        db.reindex_volume(&b, vec![("hash1".to_string(), file_on(&b, "x.jpg"))]);

        assert_eq!(db.volumes().len(), 2);
        assert_eq!(db.matches("hash1", &file_on(&a, "x.jpg")).len(), 1);
    }

    #[test]
    fn the_same_drive_under_another_letter_is_still_the_same_volume() {
        let at_e = vol("E:", "Backup", Some(7));
        let at_f = vol("F:", "Backup", Some(7));
        let mut db = IndexDb::default();
        db.reindex_volume(&at_e, vec![("old".to_string(), file_on(&at_e, "a.jpg"))]);

        db.reindex_volume(&at_f, vec![("new".to_string(), file_on(&at_f, "a.jpg"))]);

        assert_eq!(db.total_files(), 1);
        assert_eq!(db.volumes().len(), 1);
    }

    #[test]
    fn legacy_entries_are_adopted_by_the_drive_now_mounted_under_their_letter_and_label() {
        let legacy = vol("D:", "data", None);
        let mut db = IndexDb::default();
        db.upsert(vec![
            ("hash1".to_string(), file_on(&legacy, "a.jpg")),
            ("hash2".to_string(), file_on(&legacy, "b.jpg")),
        ]);
        db.upsert(vec![("hash3".to_string(), file_on(&vol("D:", "other", None), "c.jpg"))]);
        assert_eq!(db.volumes()[0].key, "D:|data");

        assert!(db.adopt_legacy(&vol("D:", "data", Some(0xAB))));

        let keys: Vec<_> = db.volumes().iter().map(|v| v.key.clone()).collect();
        assert_eq!(keys, vec!["0000-00AB|data", "D:|other"]);
        assert!(!db.adopt_legacy(&vol("D:", "data", Some(0xAB))), "nothing left to adopt");
    }

    #[test]
    fn reindexing_replaces_the_legacy_entries_of_that_drive() {
        let mut db = IndexDb::default();
        db.upsert(vec![("old".to_string(), file_on(&vol("D:", "data", None), "a.jpg"))]);
        let now = vol("D:", "data", Some(5));

        db.reindex_volume(&now, vec![("new".to_string(), file_on(&now, "b.jpg"))]);

        assert_eq!(db.total_files(), 1);
        assert_eq!(db.matches("new", &file("E:", "x")).len(), 1);
    }

    #[test]
    fn matches_excludes_the_queried_file_itself() {
        let mut db = IndexDb::default();
        db.reindex_volume(
            &d(),
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
        // "exclude self" filter must key off the volume, not just the
        // letter + path, or it would wrongly drop a real match.
        let data = vol("D:", "data", Some(1));
        let mut db = IndexDb::default();
        db.reindex_volume(&data, vec![("hash1".to_string(), file_on(&data, "a.jpg"))]);

        let results = db.matches("hash1", &file_on(&vol("D:", "500GB-5", Some(2)), "a.jpg"));
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn upsert_adds_files_and_replaces_same_path_without_wiping_the_volume() {
        let mut db = IndexDb::default();
        db.reindex_volume(&d(), vec![("old".to_string(), file("D:", "a.jpg"))]);
        db.reindex_volume(&d(), vec![
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
    fn volumes_total_files_and_bytes_and_track_changes() {
        let mut db = IndexDb::default();
        let before = db.generation();
        db.reindex_volume(&d(), vec![
            ("h1".to_string(), file("D:", "a.jpg")),
            ("h2".to_string(), file("D:", "b.jpg")),
        ]);
        assert_ne!(db.generation(), before);
        assert_eq!(db.volumes()[0].files, 2);
        assert_eq!(db.volumes()[0].bytes, 200);

        db.forget_volume(&d().key());
        assert!(db.volumes().is_empty());
    }

    #[test]
    fn content_diff_compares_by_hash_not_path() {
        let mut db = IndexDb::default();
        db.reindex_volume(&d(), vec![
            ("same".to_string(), file("D:", "a.jpg")),
            ("only_d".to_string(), file("D:", "b.jpg")),
            ("only_d".to_string(), file("D:", "b copy.jpg")),
        ]);
        db.reindex_volume(&e(), vec![
            ("same".to_string(), file("E:", "renamed/a.jpg")),
            ("only_e".to_string(), file("E:", "c.jpg")),
        ]);

        let diff = db.content_diff(&d().key(), &e().key());
        assert_eq!(diff.only_a_files, 2);
        assert_eq!(diff.only_a_bytes, 200);
        assert_eq!(diff.only_b_files, 1);
        assert!(!diff.in_sync());
        assert!(db.content_diff(&d().key(), &d().key()).in_sync());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");

        let mut db = IndexDb::default();
        db.reindex_volume(&d(), vec![("hash1".to_string(), file("D:", "a.jpg"))]);
        db.save(&path).unwrap();

        let loaded = IndexDb::load(&path);
        assert_eq!(loaded.total_files(), 1);
        assert_eq!(loaded.matches("hash1", &file("D:", "nonexistent")).len(), 1);
        assert_eq!(loaded.volumes()[0].serial, Some(SERIAL_D));
    }

    #[test]
    fn indexes_from_older_versions_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        std::fs::write(
            &path,
            br#"{"entries":{"h":[{"volume_label":"data","drive_letter":"D:","rel_path":"a.jpg",
                "size":5,"modified":{"secs_since_epoch":1,"nanos_since_epoch":0}}]},
               "volumes":{"D:|data":{"total":1000,"free":250,
                "recorded_at":{"secs_since_epoch":5,"nanos_since_epoch":0}}}}"#,
        )
        .unwrap();

        let mut db = IndexDb::load(&path);
        assert_eq!(db.volumes()[0].key, "D:|data");
        let usage = db.take_legacy_usage();
        assert_eq!(usage["D:|data"].used(), 750);

        db.save(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("\"volumes\""), "legacy usage isn't written back");
    }

    #[test]
    fn load_missing_file_returns_empty_db() {
        let db = IndexDb::load(Path::new("this/path/does/not/exist.json"));
        assert_eq!(db.total_files(), 0);
    }
}
