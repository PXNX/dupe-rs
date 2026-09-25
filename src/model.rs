use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub created: SystemTime,
    pub modified: SystemTime,
}

#[derive(Clone, Debug)]
pub struct DupeGroup {
    pub hash: [u8; 32],
    /// Sorted so that `files[0]` is the "original" (oldest `modified`, then shortest filename).
    pub files: Vec<FileEntry>,
}

/// An image or video file considered by `ScanMode::SimilarMedia`. Decoded
/// dimensions are what let that mode pick the highest-resolution copy as the
/// original, unlike `ScanMode::ExactContent`'s oldest-file rule.
#[derive(Clone, Debug)]
pub struct MediaEntry {
    pub path: PathBuf,
    pub size: u64,
    pub created: SystemTime,
    pub modified: SystemTime,
    pub width: u32,
    pub height: u32,
}

/// A cluster of images/videos that look like the same shot at different
/// resolutions. Sorted so that `files[0]` is the "original": highest
/// resolution first, oldest `modified` as a tiebreaker.
#[derive(Clone, Debug)]
pub struct SimilarGroup {
    pub files: Vec<MediaEntry>,
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
    SimilarGroupFound(SimilarGroup),
    /// A batch of files found by `ScanMode::MatchingFiles`.
    FilesMatched(Vec<FileEntry>),
    Done {
        elapsed_ms: u128,
    },
    Error(String),
}

/// Progress from a background trash-deletion, so a large selection doesn't
/// freeze the UI or leave the user staring at nothing.
#[derive(Clone, Debug)]
pub enum DeleteEvent {
    /// The worker is about to delete `path`; drives the "currently deleting"
    /// readout.
    FileStarted { path: PathBuf },
    /// One file has been (attempted to be) deleted; `deleted` is false if it
    /// was missing or the trash call failed.
    FileDone { path: PathBuf, deleted: bool },
    Finished,
}
