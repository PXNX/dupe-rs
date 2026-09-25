use super::{hash, walk};
use crate::config::ScanConfig;
use crate::index_db::{IndexedFile, VolumeUsage, hex_encode};
use crate::model::ScanEvent;
use crate::volume;
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use crate::control::JobControl;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub enum IndexEvent {
    Progress { scanned: usize, total: usize },
    Done {
        entries: Vec<(String, IndexedFile)>,
        elapsed_ms: u128,
        /// How full each indexed drive is, measured at the end of the pass.
        usage: Vec<(String, String, VolumeUsage)>,
    },
}

/// Walks every configured folder and fully hashes *every* surviving file —
/// unlike the duplicate scan (`scanner::run_scan`), which only pays for a
/// full-file hash once a file already shares a size (and then a partial
/// hash) with something else in the same scan. Building a reverse-search
/// index needs every file's hash up front, since the point is to find
/// matches against files that may not even be part of the current scan.
pub fn run_index_scan(config: ScanConfig, tx: Sender<IndexEvent>, control: Arc<JobControl>) {
    let start = Instant::now();

    // `walk_and_filter` reports errors via `ScanEvent`; the indexer doesn't
    // surface those individually, so its end of this channel is just drained
    // and discarded rather than wired up to anything.
    let (walk_tx, _walk_rx) = crossbeam_channel::unbounded::<ScanEvent>();
    let files = walk::walk_and_filter(&config, &control, &walk_tx);

    if control.checkpoint() {
        let _ = tx.send(IndexEvent::Done {
            entries: Vec::new(),
            elapsed_ms: start.elapsed().as_millis(),
            usage: Vec::new(),
        });
        return;
    }

    // Volume label lookups are OS calls; cache one per drive letter up front
    // (there's usually only one or two) rather than paying for it per file.
    let mut volumes = HashMap::new();
    for f in &files {
        let drive = volume::drive_letter_of(&f.path);
        volumes
            .entry(drive.clone())
            .or_insert_with(|| volume::volume_info(&drive));
    }

    let total = files.len();
    let scanned = AtomicUsize::new(0);

    let entries: Vec<(String, IndexedFile)> = files
        .into_par_iter()
        .filter_map(|f| {
            if control.checkpoint() {
                return None;
            }
            let file_hash = hash::full_hash(&f.path).ok()?;
            let drive_letter = volume::drive_letter_of(&f.path);
            let vol = volumes.get(&drive_letter)?;
            let root = format!("{drive_letter}\\");
            let rel_path = f
                .path
                .strip_prefix(&root)
                .unwrap_or(&f.path)
                .to_path_buf();

            let n = scanned.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(200) {
                let _ = tx.send(IndexEvent::Progress { scanned: n, total });
            }

            Some((
                hex_encode(&file_hash),
                IndexedFile {
                    volume_label: vol.label.clone(),
                    drive_letter,
                    rel_path,
                    size: f.size,
                    modified: f.modified,
                },
            ))
        })
        .collect();

    let usage = volumes
        .iter()
        .filter_map(|(drive, vol)| Some((drive.clone(), vol.label.clone(), volume_usage(drive)?)))
        .collect();
    let _ = tx.send(IndexEvent::Done {
        entries,
        elapsed_ms: start.elapsed().as_millis(),
        usage,
    });
}

/// Current total/free space of `drive_letter` (e.g. `"D:"`), if queryable.
pub fn volume_usage(drive_letter: &str) -> Option<VolumeUsage> {
    if drive_letter.is_empty() {
        return None;
    }
    let space = volume::disk_space(std::path::Path::new(&format!("{drive_letter}\\")))?;
    Some(VolumeUsage {
        total: space.total,
        free: space.free,
        recorded_at: std::time::SystemTime::now(),
    })
}
