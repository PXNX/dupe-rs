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
    Progress {
        scanned: usize,
    },
    /// Sent once, right before the (slow) full-file hashing pass begins, once
    /// the total bytes that pass need reading is known.
    HashPhaseStarted {
        total_bytes: u64,
    },
    /// Cumulative bytes hashed so far during the full-file hashing pass.
    HashProgress {
        bytes_done: u64,
    },
    GroupFound(DupeGroup),
    Done {
        elapsed_ms: u128,
    },
    Error(String),
}
