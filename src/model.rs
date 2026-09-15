use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
pub struct DupeGroup {
    pub hash: [u8; 32],
    /// Sorted so that `files[0]` is the "original" (oldest `modified`, then shortest filename).
    pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug)]
pub enum ScanEvent {
    Progress { scanned: usize },
    GroupFound(DupeGroup),
    Done { elapsed_ms: u128 },
    Error(String),
}
