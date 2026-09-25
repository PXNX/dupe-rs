mod group;
mod hash;
pub mod indexer;
mod similarity;
mod walk;

pub use hash::full_hash;

use crate::config::{ScanConfig, ScanMode};
use crate::control::JobControl;
use crate::model::{DupeGroup, FileEntry, ScanEvent};
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Entry point for a background scan; dispatches to the exact-content or
/// similar-media pipeline depending on `config.mode`.
pub fn run_scan(config: ScanConfig, tx: Sender<ScanEvent>, control: Arc<JobControl>) {
    match config.mode {
        ScanMode::ExactContent => run_exact_scan(config, tx, control),
        ScanMode::SimilarMedia => similarity::run_similarity_scan(config, tx, control),
        ScanMode::MatchingFiles => run_match_scan(config, tx, control),
    }
}

/// Lists every file passing the configured filters (no hashing at all).
fn run_match_scan(config: ScanConfig, tx: Sender<ScanEvent>, control: Arc<JobControl>) {
    let start = Instant::now();
    let files = walk::walk_and_filter(&config, &control, &tx);
    for batch in files.chunks(1000) {
        let _ = tx.send(ScanEvent::FilesMatched(batch.to_vec()));
    }
    let _ = tx.send(ScanEvent::Done {
        elapsed_ms: start.elapsed().as_millis(),
    });
}

/// Orchestrates a full exact-content scan: walk & filter, group by size,
/// narrow with a cheap partial-hash pass, then confirm with a full-file hash.
/// Intended to run on a background thread; `control` is polled (to cancel or
/// pause) between phases and inside the walk.
fn run_exact_scan(config: ScanConfig, tx: Sender<ScanEvent>, control: Arc<JobControl>) {
    let start = Instant::now();
    let candidates = walk::walk_and_filter(&config, &control, &tx);

    if control.checkpoint() {
        let _ = tx.send(ScanEvent::Done {
            elapsed_ms: start.elapsed().as_millis(),
        });
        return;
    }

    let by_size = group::group_by_size(candidates);
    let sized_dupes: Vec<Vec<FileEntry>> = by_size.into_values().filter(|v| v.len() > 1).collect();

    // Sub-group each same-size bucket by a cheap partial hash to cut down what
    // needs a full-file read.
    let by_partial: Vec<Vec<FileEntry>> = sized_dupes
        .into_par_iter()
        .flat_map_iter(|files| {
            let mut map: HashMap<[u8; 32], Vec<FileEntry>> = HashMap::new();
            for f in files {
                if control.checkpoint() {
                    break;
                }
                if let Ok(h) = hash::partial_hash(&f.path) {
                    map.entry(h).or_default().push(f);
                }
            }
            map.into_values().collect::<Vec<_>>()
        })
        .filter(|v| v.len() > 1)
        .collect();

    // The full-file hash pass is the slow, I/O-bound part on large trees, so
    // report byte-level progress (used by the UI for a GB-scanned/ETA readout)
    // rather than just a file count.
    let total_hash_bytes: u64 = by_partial.iter().flatten().map(|f| f.size).sum();
    let _ = tx.send(ScanEvent::HashPhaseStarted {
        total_bytes: total_hash_bytes,
    });
    let hash_bytes_done = AtomicU64::new(0);
    let last_reported = AtomicU64::new(0);
    let report_threshold = (total_hash_bytes / 200).max(4 * 1024 * 1024);
    let same_folder_only = config.same_folder_only;

    by_partial.into_par_iter().for_each(|files| {
        if control.checkpoint() {
            return;
        }
        let mut by_full: HashMap<[u8; 32], Vec<FileEntry>> = HashMap::new();
        for f in files {
            let size = f.size;
            if let Ok(h) = hash::full_hash(&f.path) {
                by_full.entry(h).or_default().push(f);
            }

            let done = hash_bytes_done.fetch_add(size, Ordering::Relaxed) + size;
            let prev = last_reported.load(Ordering::Relaxed);
            if done.saturating_sub(prev) >= report_threshold
                && last_reported
                    .compare_exchange(prev, done, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                let _ = tx.send(ScanEvent::HashProgress { bytes_done: done });
            }
        }
        for (hash, group_files) in by_full {
            if group_files.len() <= 1 {
                continue;
            }
            let subgroups = if same_folder_only {
                group::split_by_parent(group_files)
            } else {
                vec![group_files]
            };
            for mut subgroup in subgroups {
                group::sort_group_original(&mut subgroup);
                let _ = tx.send(ScanEvent::GroupFound(DupeGroup {
                    hash,
                    files: subgroup,
                }));
            }
        }
    });

    if total_hash_bytes > 0 {
        let _ = tx.send(ScanEvent::HashProgress {
            bytes_done: total_hash_bytes,
        });
    }

    let _ = tx.send(ScanEvent::Done {
        elapsed_ms: start.elapsed().as_millis(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_duplicate_groups_and_marks_oldest_as_original() {
        let dir = tempdir().unwrap();
        let path_a = dir.path().join("a_older.txt");
        let path_b = dir.path().join("b_newer.txt");
        let path_unique = dir.path().join("unique.txt");

        fs::write(&path_a, b"same content, same content, same content").unwrap();
        fs::write(&path_b, b"same content, same content, same content").unwrap();
        fs::write(&path_unique, b"totally different").unwrap();

        // Ensure a's mtime is strictly older than b's.
        let older = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        filetime::set_file_mtime(&path_a, filetime::FileTime::from_system_time(older)).unwrap();

        let config = ScanConfig {
            folders: vec![dir.path().to_path_buf()],
            ..ScanConfig::default()
        };

        let (tx, rx) = unbounded();
        let control = Arc::new(JobControl::default());
        run_scan(config, tx, control);

        let events: Vec<ScanEvent> = rx.try_iter().collect();
        let groups: Vec<DupeGroup> = events
            .into_iter()
            .filter_map(|e| match e {
                ScanEvent::GroupFound(g) => Some(g),
                _ => None,
            })
            .collect();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        assert_eq!(groups[0].files[0].path, path_a);
    }
}
