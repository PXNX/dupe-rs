use crate::control::JobControl;
use crossbeam_channel::Sender;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

/// NTFS file-record size. Every file and folder costs one MFT record, which
/// grows the MFT out of the drive's free space, so it's counted on top of the
/// data clusters when estimating what a folder will really occupy.
const PER_ENTRY_OVERHEAD: u64 = 1024;

/// One top-level folder of the drive-fill source, sized up recursively.
#[derive(Clone, Debug)]
pub struct RootFolder {
    pub name: String,
    pub path: PathBuf,
    /// Sum of file sizes (what Explorer shows as "Size").
    pub size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub created: SystemTime,
    pub modified: SystemTime,
    /// Every file's size, kept so the on-disk footprint can be computed
    /// exactly for whatever cluster size the target drive turns out to use.
    file_sizes: Vec<u64>,
}

impl RootFolder {
    /// Upper bound on the space this folder takes once copied to a drive with
    /// `cluster` bytes per cluster: each file rounded up to whole clusters,
    /// plus one file record per file and folder.
    pub fn footprint(&self, cluster: u64) -> u64 {
        let cluster = cluster.max(1);
        let data: u64 = self
            .file_sizes
            .iter()
            .map(|&s| s.div_ceil(cluster) * cluster)
            .sum();
        data + (self.file_count + self.dir_count) * PER_ENTRY_OVERHEAD
    }
}

pub enum MeasureEvent {
    Progress { done: usize, total: usize },
    Done(Vec<RootFolder>),
    Error(String),
}

/// Lists the immediate subfolders of `source` and totals each one up.
/// Files sitting directly in `source` are ignored: only whole folders are
/// placed on target drives.
pub fn measure_root_folders(source: PathBuf, tx: Sender<MeasureEvent>, control: &JobControl) {
    let roots: Vec<(String, PathBuf)> = match std::fs::read_dir(&source) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(err) => {
            let _ = tx.send(MeasureEvent::Error(format!(
                "Couldn't read {}: {err}",
                source.display()
            )));
            let _ = tx.send(MeasureEvent::Done(Vec::new()));
            return;
        }
    };

    let total = roots.len();
    let mut folders = Vec::with_capacity(total);
    for (done, (name, path)) in roots.into_iter().enumerate() {
        if control.checkpoint() {
            break;
        }
        let _ = tx.send(MeasureEvent::Progress { done, total });
        if let Some(folder) = measure_folder(name, &path, control) {
            folders.push(folder);
        }
    }
    let _ = tx.send(MeasureEvent::Done(folders));
}

fn measure_folder(name: String, path: &Path, control: &JobControl) -> Option<RootFolder> {
    let meta = std::fs::metadata(path).ok()?;
    let mut folder = RootFolder {
        name,
        path: path.to_path_buf(),
        size: 0,
        file_count: 0,
        dir_count: 0,
        created: meta.created().unwrap_or(SystemTime::UNIX_EPOCH),
        modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        file_sizes: Vec::new(),
    };
    for entry in WalkDir::new(path).follow_links(false).min_depth(1) {
        if control.checkpoint() {
            return None;
        }
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_dir() {
            folder.dir_count += 1;
        } else if entry.file_type().is_file()
            && let Ok(m) = entry.metadata()
        {
            folder.size += m.len();
            folder.file_count += 1;
            folder.file_sizes.push(m.len());
        }
    }
    Some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn totals_each_top_level_folder_and_ignores_loose_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/nested")).unwrap();
        fs::create_dir(dir.path().join("b")).unwrap();
        fs::write(dir.path().join("a/one.bin"), vec![0u8; 100]).unwrap();
        fs::write(dir.path().join("a/nested/two.bin"), vec![0u8; 50]).unwrap();
        fs::write(dir.path().join("b/three.bin"), vec![0u8; 10]).unwrap();
        fs::write(dir.path().join("loose.bin"), vec![0u8; 999]).unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        measure_root_folders(dir.path().to_path_buf(), tx, &JobControl::default());
        let mut folders = rx
            .try_iter()
            .find_map(|e| match e {
                MeasureEvent::Done(f) => Some(f),
                _ => None,
            })
            .unwrap();
        folders.sort_by(|x, y| x.name.cmp(&y.name));

        assert_eq!(folders.len(), 2);
        assert_eq!((folders[0].name.as_str(), folders[0].size), ("a", 150));
        assert_eq!(folders[0].file_count, 2);
        assert_eq!(folders[0].dir_count, 1);
        assert_eq!((folders[1].name.as_str(), folders[1].size), ("b", 10));
        // Two 4 KiB clusters plus three file records.
        assert_eq!(folders[0].footprint(4096), 2 * 4096 + 3 * 1024);
    }
}
