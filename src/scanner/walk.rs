use crate::config::ScanConfig;
use crate::control::JobControl;
use crate::model::{FileEntry, ScanEvent};
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use walkdir::{DirEntry, WalkDir};

const PROGRESS_INTERVAL: usize = 200;

/// Walks every configured root, applies the size/extension filters, and returns
/// the surviving files. Each root's immediate subdirectories are walked as
/// independent rayon tasks, so a scan spanning many sibling folders — a
/// typical media library layout (`Music/<Artist>/<Album>/...`) — fans out
/// across multiple cores instead of running as one sequential walk. Zero-byte
/// files are always skipped (never meaningfully dedupable by content);
/// permission errors and unreadable metadata are logged via `ScanEvent::Error`
/// and skipped rather than aborting the whole scan.
pub fn walk_and_filter(
    config: &ScanConfig,
    control: &JobControl,
    tx: &Sender<ScanEvent>,
) -> Vec<FileEntry> {
    let scanned = AtomicUsize::new(0);
    let mut candidates = Vec::new();
    let mut parallel_roots: Vec<PathBuf> = Vec::new();

    for root in &config.folders {
        if control.checkpoint() {
            return candidates;
        }

        if config.exclude_subfolders {
            walk_into(
                WalkDir::new(root).follow_links(false).max_depth(1),
                config,
                control,
                tx,
                &scanned,
                &mut candidates,
            );
            continue;
        }

        // Direct files of `root` are collected here on the spot; its
        // subdirectories are queued up to be walked (recursively, in full) in
        // parallel below.
        for entry in WalkDir::new(root)
            .follow_links(false)
            .min_depth(1)
            .max_depth(1)
        {
            if control.checkpoint() {
                return candidates;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(ScanEvent::Error(format!("Skipping entry: {e}")));
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                parallel_roots.push(entry.into_path());
            } else if entry.file_type().is_file()
                && let Some(f) = process_entry(&entry, config, tx, &scanned)
            {
                candidates.push(f);
            }
        }
    }

    if control.checkpoint() {
        return candidates;
    }

    let parallel_results: Vec<Vec<FileEntry>> = parallel_roots
        .par_iter()
        .map(|dir| {
            let mut local = Vec::new();
            walk_into(
                WalkDir::new(dir).follow_links(false),
                config,
                control,
                tx,
                &scanned,
                &mut local,
            );
            local
        })
        .collect();
    candidates.extend(parallel_results.into_iter().flatten());

    let _ = tx.send(ScanEvent::Progress {
        scanned: scanned.load(Ordering::Relaxed),
    });
    candidates
}

fn walk_into(
    walker: WalkDir,
    config: &ScanConfig,
    control: &JobControl,
    tx: &Sender<ScanEvent>,
    scanned: &AtomicUsize,
    out: &mut Vec<FileEntry>,
) {
    for entry in walker {
        if control.checkpoint() {
            return;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(ScanEvent::Error(format!("Skipping entry: {e}")));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(f) = process_entry(&entry, config, tx, scanned) {
            out.push(f);
        }
    }
}

/// Applies the size/extension filters to a single file entry and, if it
/// survives, returns the `FileEntry` to keep. Shared by both the direct-file
/// listing and the parallel subdirectory walks so the filtering logic (and
/// progress counting) never drifts between the two.
fn process_entry(
    entry: &DirEntry,
    config: &ScanConfig,
    tx: &Sender<ScanEvent>,
    scanned: &AtomicUsize,
) -> Option<FileEntry> {
    let metadata = match entry.metadata() {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(ScanEvent::Error(format!(
                "Skipping {}: {e}",
                entry.path().display()
            )));
            return None;
        }
    };

    let size = metadata.len();
    if size == 0 {
        return None;
    }
    if config.min_size.is_some_and(|min| size < min) {
        return None;
    }
    if config.max_size.is_some_and(|max| size > max) {
        return None;
    }

    let ext = entry
        .path()
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if !config.extensions.matches(ext.as_deref()) {
        return None;
    }

    // platform can't report mtime: can't apply "oldest wins" rule
    let modified = metadata.modified().ok()?;
    // Creation time is purely informational (shown in the UI); not every
    // filesystem tracks it, so fall back to `modified` rather than dropping
    // the file.
    let created = metadata.created().unwrap_or(modified);

    let n = scanned.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(PROGRESS_INTERVAL) {
        let _ = tx.send(ScanEvent::Progress { scanned: n });
    }

    Some(FileEntry {
        path: entry.path().to_path_buf(),
        size,
        created,
        modified,
    })
}
