use crate::config::ScanConfig;
use crate::model::{FileEntry, ScanEvent};
use crossbeam_channel::Sender;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

const PROGRESS_INTERVAL: usize = 200;

/// Walks every configured root, applies the size/extension filters, and returns
/// the surviving files. Zero-byte files are always skipped (never meaningfully
/// dedupable by content); permission errors and unreadable metadata are logged
/// via `ScanEvent::Error` and skipped rather than aborting the whole scan.
pub fn walk_and_filter(
    config: &ScanConfig,
    cancel: &Arc<AtomicBool>,
    tx: &Sender<ScanEvent>,
) -> Vec<FileEntry> {
    let mut candidates = Vec::new();
    let mut scanned = 0usize;

    for root in &config.folders {
        let mut walker = WalkDir::new(root).follow_links(false);
        if config.exclude_subfolders {
            walker = walker.max_depth(1);
        }

        for entry in walker {
            if cancel.load(Ordering::Relaxed) {
                return candidates;
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

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx.send(ScanEvent::Error(format!(
                        "Skipping {}: {e}",
                        entry.path().display()
                    )));
                    continue;
                }
            };

            let size = metadata.len();
            if size == 0 {
                continue;
            }
            if config.min_size.is_some_and(|min| size < min) {
                continue;
            }
            if config.max_size.is_some_and(|max| size > max) {
                continue;
            }

            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());
            if !config.extensions.matches(ext.as_deref()) {
                continue;
            }

            let modified = match metadata.modified() {
                Ok(m) => m,
                Err(_) => continue, // platform can't report mtime: can't apply "oldest wins" rule
            };

            candidates.push(FileEntry {
                path: entry.into_path(),
                size,
                modified,
            });

            scanned += 1;
            if scanned.is_multiple_of(PROGRESS_INTERVAL) {
                let _ = tx.send(ScanEvent::Progress { scanned });
            }
        }
    }

    let _ = tx.send(ScanEvent::Progress { scanned });
    candidates
}
