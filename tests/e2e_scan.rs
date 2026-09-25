//! End-to-end tests of the scan pipeline: real files on disk, through
//! `scanner::run_scan`, down to the `ScanEvent` stream a caller would see.
//! Exercises the config knobs (`exclude_subfolders`, `same_folder_only`,
//! extension/size filters, cancellation) as a black box, the way the app
//! itself drives the scanner.

use crossbeam_channel::unbounded;
use dupe_rs::config::{ExtensionFilter, ScanConfig};
use dupe_rs::control::JobControl;
use dupe_rs::model::{DupeGroup, ScanEvent};
use dupe_rs::scanner::run_scan;
use filetime::{FileTime, set_file_mtime};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

fn config_for(dir: &Path) -> ScanConfig {
    ScanConfig {
        folders: vec![dir.to_path_buf()],
        ..ScanConfig::default()
    }
}

fn age(path: &Path, secs_ago: u64) {
    let t = SystemTime::now() - Duration::from_secs(secs_ago);
    set_file_mtime(path, FileTime::from_system_time(t)).unwrap();
}

fn scan(config: ScanConfig) -> Vec<DupeGroup> {
    let (tx, rx) = unbounded();
    run_scan(config, tx, Arc::new(JobControl::default()));
    rx.try_iter()
        .filter_map(|e| match e {
            ScanEvent::GroupFound(g) => Some(g),
            _ => None,
        })
        .collect()
}

#[test]
fn finds_cross_folder_duplicates_by_default() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    let a = dir.path().join("a/photo.jpg");
    let b = dir.path().join("b/photo_copy.jpg");
    fs::write(&a, b"identical bytes across folders").unwrap();
    fs::write(&b, b"identical bytes across folders").unwrap();
    age(&a, 60);

    let groups = scan(config_for(dir.path()));

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].files.len(), 2);
    assert_eq!(groups[0].files[0].path, a);
}

#[test]
fn same_folder_only_drops_cross_folder_duplicates() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    fs::write(dir.path().join("a/photo.jpg"), b"identical bytes").unwrap();
    fs::write(dir.path().join("b/photo_copy.jpg"), b"identical bytes").unwrap();

    let mut config = config_for(dir.path());
    config.same_folder_only = true;
    let groups = scan(config);

    assert!(groups.is_empty(), "cross-folder copies must not be marked duplicate");
}

#[test]
fn same_folder_only_keeps_duplicates_sharing_a_folder() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("b")).unwrap();
    let a1 = dir.path().join("a/one.jpg");
    let a2 = dir.path().join("a/two.jpg");
    fs::write(&a1, b"same folder duplicate content").unwrap();
    fs::write(&a2, b"same folder duplicate content").unwrap();
    age(&a1, 60);
    // A same-content file in a different folder should stay excluded.
    fs::write(dir.path().join("b/three.jpg"), b"same folder duplicate content").unwrap();

    let mut config = config_for(dir.path());
    config.same_folder_only = true;
    let groups = scan(config);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].files.len(), 2);
    assert!(groups[0].files.iter().all(|f| f.path.starts_with(dir.path().join("a"))));
}

#[test]
fn exclude_subfolders_ignores_nested_files() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("top.txt"), b"duplicate payload").unwrap();
    fs::write(dir.path().join("nested/copy.txt"), b"duplicate payload").unwrap();

    let mut config = config_for(dir.path());
    config.exclude_subfolders = true;
    let groups = scan(config);

    assert!(groups.is_empty(), "the nested copy should never be seen");
}

#[test]
fn extension_include_filter_limits_matches() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"same content, different extension").unwrap();
    fs::write(dir.path().join("b.log"), b"same content, different extension").unwrap();
    fs::write(dir.path().join("c.txt"), b"same content, different extension").unwrap();

    let mut config = config_for(dir.path());
    config.extensions = ExtensionFilter::Include(vec!["txt".to_string()]);
    let groups = scan(config);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].files.len(), 2);
    assert!(groups[0].files.iter().all(|f| f.path.extension().unwrap() == "txt"));
}

#[test]
fn size_range_filter_excludes_files_outside_bounds() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("small_a.bin"), vec![1u8; 10]).unwrap();
    fs::write(dir.path().join("small_b.bin"), vec![1u8; 10]).unwrap();
    fs::write(dir.path().join("big_a.bin"), vec![2u8; 10_000]).unwrap();
    fs::write(dir.path().join("big_b.bin"), vec![2u8; 10_000]).unwrap();

    let mut config = config_for(dir.path());
    config.min_size = Some(1_000);
    let groups = scan(config);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].files[0].size, 10_000);
}

#[test]
fn same_size_different_extension_files_are_not_grouped() {
    // Extension is now part of the pre-hash bucket key (a speed optimization),
    // so identical content under different extensions no longer counts as a
    // duplicate. This documents that trade-off at the full-scan level.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.jpg"), b"identical bytes, different ext").unwrap();
    fs::write(dir.path().join("b.png"), b"identical bytes, different ext").unwrap();

    let groups = scan(config_for(dir.path()));

    assert!(groups.is_empty());
}

#[test]
fn subdirectories_are_walked_in_parallel_without_missing_files() {
    // Exercises the parallel-per-subdirectory walk: several sibling folders,
    // each with its own duplicate pair plus a unique file, must all be found.
    let dir = tempdir().unwrap();
    for artist in ["artist_a", "artist_b", "artist_c", "artist_d"] {
        let sub = dir.path().join(artist);
        fs::create_dir(&sub).unwrap();
        let track_bytes = format!("track bytes unique to {artist}");
        fs::write(sub.join("track.mp3"), &track_bytes).unwrap();
        fs::write(sub.join("track_copy.mp3"), &track_bytes).unwrap();
        fs::write(sub.join("other.mp3"), format!("unique to {artist}")).unwrap();
    }

    let groups = scan(config_for(dir.path()));

    assert_eq!(groups.len(), 4);
    assert!(groups.iter().all(|g| g.files.len() == 2));
}

#[test]
fn cancelling_before_scan_starts_yields_no_groups() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), b"content").unwrap();
    fs::write(dir.path().join("b.txt"), b"content").unwrap();

    let (tx, rx) = unbounded();
    run_scan(config_for(dir.path()), tx, Arc::new(JobControl::cancelled()));

    let events: Vec<ScanEvent> = rx.try_iter().collect();
    assert!(matches!(events.last(), Some(ScanEvent::Done { .. })));
    assert!(!events.iter().any(|e| matches!(e, ScanEvent::GroupFound(_))));
}

#[test]
fn rar_and_split_archive_volumes_are_skipped_unless_asked_for() {
    let dir = tempdir().unwrap();
    for sub in ["a", "b"] {
        let d = dir.path().join(sub);
        fs::create_dir(&d).unwrap();
        fs::write(d.join("movie.part1.rar"), b"volume one bytes").unwrap();
        fs::write(d.join("movie.part2.rar"), b"volume two bytes").unwrap();
        fs::write(d.join("old.r00"), b"old style volume").unwrap();
        fs::write(d.join("backup.7z.001"), b"seven zip split").unwrap();
        fs::write(d.join("photo.jpg"), b"an ordinary duplicate").unwrap();
    }

    let groups = scan(config_for(dir.path()));
    assert_eq!(groups.len(), 1, "only the ordinary file pairs up");
    assert!(groups[0].files[0].path.ends_with("photo.jpg"));

    let groups = scan(ScanConfig {
        skip_archives: false,
        ..config_for(dir.path())
    });
    assert_eq!(groups.len(), 5);
}
