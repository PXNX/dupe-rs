use crate::config::ScanConfig;
use crate::model::{FileEntry, MediaEntry, ScanEvent, SimilarGroup};
use crossbeam_channel::Sender;
use image::DynamicImage;
use rayon::prelude::*;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv", "flv", "mpg", "mpeg",
];

/// Side of the square the image is shrunk to before hashing; the hash itself
/// is one bit narrower (`HASH_SIDE - 1`) per row, since a difference hash
/// compares each pixel to its neighbour.
const HASH_SIDE: u32 = 9;

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(image::ImageFormat::from_extension)
        .is_some()
}

fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// 64-bit difference hash: shrink to a 9x8 grayscale grid and set bit `i` when
/// pixel `i` is brighter than the pixel to its right. Visually similar images
/// (recompressed, resized, mildly color-corrected) end up with a small
/// Hamming distance between their hashes even though their bytes differ
/// completely.
fn dhash(img: &DynamicImage) -> u64 {
    let small = img
        .resize_exact(HASH_SIDE, HASH_SIDE - 1, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash: u64 = 0;
    let mut bit = 0u32;
    for y in 0..small.height() {
        for x in 0..small.width() - 1 {
            if small.get_pixel(x, y).0[0] > small.get_pixel(x + 1, y).0[0] {
                hash |= 1 << bit;
            }
            bit += 1;
        }
    }
    hash
}

pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Decodes an image and returns its (width, height, perceptual hash).
fn hash_image(path: &Path) -> Option<(u32, u32, u64)> {
    let img = image::open(path).ok()?;
    let hash = dhash(&img);
    Some((img.width(), img.height(), hash))
}

/// Grabs one frame a second into the video via the `ffmpeg` binary on PATH
/// and hashes it the same way as a still image. Returns `None` (silently, the
/// caller reports it once) if `ffmpeg` isn't installed or the video can't be
/// read.
fn hash_video_frame(path: &Path) -> Option<(u32, u32, u64)> {
    let output = Command::new("ffmpeg")
        .args(["-y", "-ss", "00:00:01", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory(&output.stdout).ok()?;
    let hash = dhash(&img);
    Some((img.width(), img.height(), hash))
}

/// True once, the first time a video is encountered and `ffmpeg` turns out to
/// be missing, so the user gets one clear message instead of one per file.
fn ffmpeg_missing() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

struct Candidate {
    entry: MediaEntry,
    hash: u64,
}

/// Greedily clusters candidates by perceptual hash: each candidate joins the
/// first existing cluster whose representative (its first member) is within
/// `threshold` bits, or starts a new cluster otherwise. This is O(n *
/// clusters) rather than a full O(n^2) comparison, which is adequate for the
/// media libraries this tool targets without needing an index structure.
fn cluster_by_similarity(candidates: Vec<Candidate>, threshold: u32) -> Vec<Vec<Candidate>> {
    let mut clusters: Vec<Vec<Candidate>> = Vec::new();
    for candidate in candidates {
        let existing = clusters
            .iter_mut()
            .find(|cluster: &&mut Vec<Candidate>| {
                hamming_distance(cluster[0].hash, candidate.hash) <= threshold
            });
        match existing {
            Some(cluster) => cluster.push(candidate),
            None => clusters.push(vec![candidate]),
        }
    }
    clusters
}

/// Sorts a similar-media group in place so the "original" ends up at index 0:
/// highest resolution first, then oldest `modified`, then shortest filename.
fn sort_group_original(files: &mut [MediaEntry]) {
    files.sort_by(|a, b| {
        (b.width as u64 * b.height as u64)
            .cmp(&(a.width as u64 * a.height as u64))
            .then_with(|| a.modified.cmp(&b.modified))
            .then_with(|| {
                let a_len = a.path.file_name().map_or(0, |n| n.len());
                let b_len = b.path.file_name().map_or(0, |n| n.len());
                a_len.cmp(&b_len)
            })
    });
}

fn split_by_parent(files: Vec<MediaEntry>) -> Vec<Vec<MediaEntry>> {
    use std::collections::HashMap;
    use std::path::PathBuf;
    let mut map: HashMap<Option<PathBuf>, Vec<MediaEntry>> = HashMap::new();
    for f in files {
        let parent = f.path.parent().map(|p| p.to_path_buf());
        map.entry(parent).or_default().push(f);
    }
    map.into_values().filter(|v| v.len() > 1).collect()
}

/// Runs a `ScanMode::SimilarMedia` scan: walks and filters like the exact-hash
/// mode, narrows candidates to images/videos, perceptually hashes them in
/// parallel (one frame via `ffmpeg` for videos), then clusters by Hamming
/// distance. Video files are skipped with a single warning if `ffmpeg` isn't
/// on PATH; image comparison always works since it only needs the `image`
/// crate already used for thumbnails.
pub fn run_similarity_scan(config: ScanConfig, tx: Sender<ScanEvent>, cancel: Arc<AtomicBool>) {
    let start = Instant::now();
    let all_candidates = super::walk::walk_and_filter(&config, &cancel, &tx);

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(ScanEvent::Done {
            elapsed_ms: start.elapsed().as_millis(),
        });
        return;
    }

    let media: Vec<FileEntry> = all_candidates
        .into_iter()
        .filter(|f| is_image_path(&f.path) || is_video_path(&f.path))
        .collect();

    let has_videos = media.iter().any(|f| is_video_path(&f.path));
    if has_videos && ffmpeg_missing() {
        let _ = tx.send(ScanEvent::Error(
            "ffmpeg not found on PATH: video files will be skipped (install ffmpeg to compare videos)."
                .to_string(),
        ));
    }

    let total_bytes: u64 = media.iter().map(|f| f.size).sum();
    let _ = tx.send(ScanEvent::HashPhaseStarted { total_bytes });
    let bytes_done = AtomicU64::new(0);
    let last_reported = AtomicU64::new(0);
    let report_threshold = (total_bytes / 200).max(4 * 1024 * 1024);

    let candidates: Vec<Candidate> = media
        .into_par_iter()
        .filter_map(|f| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let decoded = if is_video_path(&f.path) {
                hash_video_frame(&f.path)
            } else {
                hash_image(&f.path)
            };

            let done = bytes_done.fetch_add(f.size, Ordering::Relaxed) + f.size;
            let prev = last_reported.load(Ordering::Relaxed);
            if done.saturating_sub(prev) >= report_threshold
                && last_reported
                    .compare_exchange(prev, done, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                let _ = tx.send(ScanEvent::HashProgress { bytes_done: done });
            }

            let (width, height, hash) = decoded?;
            Some(Candidate {
                entry: MediaEntry {
                    path: f.path,
                    size: f.size,
                    created: f.created,
                    modified: f.modified,
                    width,
                    height,
                },
                hash,
            })
        })
        .collect();

    if total_bytes > 0 {
        let _ = tx.send(ScanEvent::HashProgress {
            bytes_done: total_bytes,
        });
    }

    let clusters = cluster_by_similarity(candidates, config.similarity_threshold);
    for cluster in clusters {
        if cluster.len() <= 1 {
            continue;
        }
        let files: Vec<MediaEntry> = cluster.into_iter().map(|c| c.entry).collect();
        let subgroups = if config.same_folder_only {
            split_by_parent(files)
        } else {
            vec![files]
        };
        for mut subgroup in subgroups {
            sort_group_original(&mut subgroup);
            let _ = tx.send(ScanEvent::SimilarGroupFound(SimilarGroup { files: subgroup }));
        }
    }

    let _ = tx.send(ScanEvent::Done {
        elapsed_ms: start.elapsed().as_millis(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn media(name: &str, width: u32, height: u32, modified_offset_secs: u64) -> MediaEntry {
        MediaEntry {
            path: PathBuf::from(name),
            size: 100,
            created: SystemTime::UNIX_EPOCH,
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_offset_secs),
            width,
            height,
        }
    }

    #[test]
    fn hamming_distance_counts_differing_bits() {
        assert_eq!(hamming_distance(0b1010, 0b1010), 0);
        assert_eq!(hamming_distance(0b1010, 0b0010), 1);
        assert_eq!(hamming_distance(0, u64::MAX), 64);
    }

    #[test]
    fn cluster_by_similarity_groups_close_hashes_and_separates_distant_ones() {
        let candidates = vec![
            Candidate { entry: media("a.jpg", 100, 100, 0), hash: 0b0000_0000 },
            Candidate { entry: media("b.jpg", 200, 200, 0), hash: 0b0000_0011 }, // 2 bits from a
            Candidate { entry: media("c.jpg", 300, 300, 0), hash: 0b1111_1111 }, // far from a
        ];
        let clusters = cluster_by_similarity(candidates, 4);
        assert_eq!(clusters.len(), 2);
        let sizes: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        assert!(sizes.contains(&2));
        assert!(sizes.contains(&1));
    }

    #[test]
    fn sort_group_original_prefers_highest_resolution_then_oldest() {
        let mut files = vec![
            media("small.jpg", 100, 100, 5),
            media("big_but_newer.jpg", 400, 400, 10),
            media("big_older.jpg", 400, 400, 1),
        ];
        sort_group_original(&mut files);
        assert_eq!(files[0].path, PathBuf::from("big_older.jpg"));
    }
}
