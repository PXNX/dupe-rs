//! What a metadata backup records about each file, and reading/writing it
//! as JSON.

use super::Progress;
use super::exif::{self, ExifInfo};
use crate::control::JobControl;
use crate::scanner::hash::partial_hash;
use chrono::{DateTime, Local, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use walkdir::WalkDir;

/// Bumped whenever the JSON layout changes incompatibly.
const FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Relative to the snapshot's root.
    pub path: PathBuf,
    pub size: u64,
    pub created: Option<DateTime<Utc>>,
    pub modified: Option<DateTime<Utc>>,
    /// Hash of the first 64 KiB (hex). Together with `size` it recognises a
    /// file again after it was renamed or moved.
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exif: Option<ExifInfo>,
}

impl FileRecord {
    pub fn read(root: &Path, path: &Path) -> Option<FileRecord> {
        let relative = path.strip_prefix(root).ok()?;
        // serde_json can only store paths that are valid Unicode.
        relative.to_str()?;
        let meta = fs::metadata(path).ok()?;
        Some(FileRecord {
            path: relative.to_path_buf(),
            size: meta.len(),
            created: meta.created().ok().map(DateTime::<Utc>::from),
            modified: meta.modified().ok().map(DateTime::<Utc>::from),
            fingerprint: partial_hash(path)
                .ok()
                .map(|h| blake3::Hash::from(h).to_hex().to_string()),
            exif: exif::read(path),
        })
    }

    /// The key a renamed or moved file is recognised by.
    pub fn identity(&self) -> Option<(u64, &str)> {
        self.fingerprint.as_deref().map(|fp| (self.size, fp))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub root: PathBuf,
    pub taken_at: DateTime<Utc>,
    pub files: Vec<FileRecord>,
}

impl Snapshot {
    pub fn new(root: PathBuf, files: Vec<FileRecord>) -> Self {
        Self {
            format: FORMAT,
            root,
            taken_at: Utc::now(),
            files,
        }
    }

    pub fn load(path: &Path) -> Result<Snapshot, String> {
        let file =
            fs::File::open(path).map_err(|e| format!("Couldn't open {}: {e}", path.display()))?;
        let snapshot: Snapshot = serde_json::from_reader(BufReader::new(file))
            .map_err(|e| format!("{} isn't a metadata backup: {e}", path.display()))?;
        if snapshot.format > FORMAT {
            return Err(format!(
                "{} was written by a newer version of dupe-rs",
                path.display()
            ));
        }
        Ok(snapshot)
    }

    /// Writes the snapshot into `dir` under a name made from the folder and
    /// the time (plus `label`, if any), returning the file's path. Written
    /// to a temporary file first so a crash never leaves a half-written backup.
    pub fn save_in(&self, dir: &Path, label: &str) -> Result<PathBuf, String> {
        let fail =
            |e: std::io::Error| format!("Couldn't save the backup in {}: {e}", dir.display());
        fs::create_dir_all(dir).map_err(fail)?;
        let stamp = self
            .taken_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H%M%S");
        let mut name = format!("{} {stamp}", folder_label(&self.root));
        if !label.is_empty() {
            name = format!("{name} {label}");
        }
        let path = (1..)
            .map(|n| match n {
                1 => dir.join(format!("{name}.json")),
                n => dir.join(format!("{name} ({n}).json")),
            })
            .find(|p| !p.exists())
            .expect("some numbered name is free");
        let tmp = path.with_extension("json.tmp");
        (|| {
            let mut out = BufWriter::new(fs::File::create(&tmp)?);
            serde_json::to_writer(&mut out, self)?;
            out.flush()?;
            drop(out);
            fs::rename(&tmp, &path)
        })()
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            fail(e)
        })?;
        Ok(path)
    }
}

/// A file-name-safe label for `root`: its folder name, or the drive for a
/// drive root (`C:\` → `C`).
fn folder_label(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let cleaned: String = name
        .chars()
        .map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_matches(['_', ' ', '.']);
    if cleaned.is_empty() {
        "backup".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Records every file under `root`. `None` if cancelled.
pub fn capture(root: &Path, control: &JobControl, progress: &Progress) -> Option<Vec<FileRecord>> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if paths.len() % 1000 == 0 && control.checkpoint() {
            return None;
        }
        if entry.file_type().is_file() {
            paths.push(entry.into_path());
        }
    }
    progress.total.store(paths.len(), Ordering::Relaxed);

    let mut files: Vec<FileRecord> = paths
        .par_iter()
        .filter_map(|path| {
            if control.checkpoint() {
                return None;
            }
            let record = FileRecord::read(root, path);
            progress.done.fetch_add(1, Ordering::Relaxed);
            record
        })
        .collect();
    if control.is_cancelled() {
        return None;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

/// The backups in `dir`, newest first.
pub fn list_backups(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, e.path())
        })
        .collect();
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_label_is_safe_for_file_names() {
        assert_eq!(folder_label(Path::new(r"D:\Photos\2019")), "2019");
        assert_eq!(folder_label(Path::new(r"C:\")), "C");
        assert_eq!(folder_label(Path::new("")), "backup");
    }

    #[test]
    fn saves_and_loads_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pics");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/a.txt"), b"hello").unwrap();

        let files = capture(&root, &JobControl::default(), &Progress::default()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, Path::new("sub").join("a.txt"));
        assert_eq!(files[0].size, 5);
        assert!(files[0].modified.is_some() && files[0].fingerprint.is_some());

        let snapshot = Snapshot::new(root.clone(), files.clone());
        let backups = dir.path().join("backups");
        let first = snapshot.save_in(&backups, "").unwrap();
        let second = snapshot.save_in(&backups, "").unwrap();
        assert_ne!(first, second, "a second save never overwrites the first");
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pics ")
        );

        let loaded = Snapshot::load(&first).unwrap();
        assert_eq!(loaded.root, root);
        assert_eq!(loaded.files, files);
        assert_eq!(list_backups(&backups).len(), 2);
    }
}
