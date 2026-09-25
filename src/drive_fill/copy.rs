use crate::control::JobControl;
use crate::index_db::{IndexedFile, VolumeUsage, hex_encode};
use crate::volume;
use crossbeam_channel::Sender;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const COPY_BUF_SIZE: usize = 1024 * 1024;
/// Bytes between `Progress` events, so a fast copy doesn't flood the channel.
const PROGRESS_STEP: u64 = 8 * 1024 * 1024;

pub enum CopyEvent {
    /// About to copy this source file.
    FileStarted {
        path: PathBuf,
    },
    /// Cumulative bytes copied so far across the whole job.
    Progress {
        bytes_done: u64,
    },
    /// A file landed on the target; ready to add to the reverse-search index.
    FileCopied {
        hash_hex: String,
        file: IndexedFile,
    },
    Error(String),
    /// The job ended. `aborted` explains an early stop other than the user
    /// cancelling (e.g. the target filled up).
    Done {
        aborted: Option<String>,
        /// The target volume's usage afterwards, measured here rather than
        /// on the UI thread (a sleeping drive can take seconds to answer).
        usage: Option<(String, String, VolumeUsage)>,
    },
}

/// Copies each `(source folder, destination folder)` pair file by file,
/// hashing every file with blake3 in the same pass that writes it (the same
/// hash the reverse-search indexer computes, so no second read is needed)
/// and reporting it via `CopyEvent::FileCopied`. Modification and creation
/// times are carried over. A file interrupted by cancellation or an error is
/// removed rather than left half-written.
pub fn copy_folders(jobs: Vec<(PathBuf, PathBuf)>, tx: Sender<CopyEvent>, control: &JobControl) {
    let volume = jobs
        .first()
        .map(|(_, dest)| volume::volume_info(&volume::drive_letter_of(dest)));
    let aborted = copy_all(jobs, &tx, control, volume.as_ref());
    let usage = volume.and_then(|vol| {
        let usage = crate::scanner::indexer::volume_usage(&vol.drive_letter)?;
        Some((vol.drive_letter, vol.label, usage))
    });
    let _ = tx.send(CopyEvent::Done { aborted, usage });
}

/// The copy loop; returns why it stopped early, if it wasn't cancelled.
fn copy_all(
    jobs: Vec<(PathBuf, PathBuf)>,
    tx: &Sender<CopyEvent>,
    control: &JobControl,
    volume: Option<&volume::VolumeInfo>,
) -> Option<String> {
    let mut bytes_done = 0u64;
    let mut last_reported = 0u64;

    for (source, dest) in jobs {
        for entry in WalkDir::new(&source).follow_links(false) {
            if control.checkpoint() {
                return None;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    let _ = tx.send(CopyEvent::Error(format!("Skipping entry: {err}")));
                    continue;
                }
            };
            let rel = entry.path().strip_prefix(&source).unwrap_or(entry.path());
            let target = dest.join(rel);

            if entry.file_type().is_dir() {
                if let Err(err) = std::fs::create_dir_all(&target) {
                    let _ = tx.send(CopyEvent::Error(format!(
                        "Couldn't create {}: {err}",
                        target.display()
                    )));
                }
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }

            let _ = tx.send(CopyEvent::FileStarted {
                path: entry.path().to_path_buf(),
            });
            let result = copy_file_hashed(entry.path(), &target, control, |n| {
                bytes_done += n;
                if bytes_done - last_reported >= PROGRESS_STEP {
                    last_reported = bytes_done;
                    let _ = tx.send(CopyEvent::Progress { bytes_done });
                }
            });
            match result {
                Ok(Some((hash, size, modified))) => {
                    let _ = tx.send(CopyEvent::Progress { bytes_done });
                    if let Some(vol) = volume {
                        let root = format!("{}\\", vol.drive_letter);
                        let _ = tx.send(CopyEvent::FileCopied {
                            hash_hex: hex_encode(&hash),
                            file: IndexedFile {
                                volume_label: vol.label.clone(),
                                drive_letter: vol.drive_letter.clone(),
                                rel_path: target
                                    .strip_prefix(&root)
                                    .unwrap_or(&target)
                                    .to_path_buf(),
                                size,
                                modified,
                            },
                        });
                    }
                }
                // Cancelled mid-file.
                Ok(None) => {
                    return None;
                }
                Err(err) if err.kind() == io::ErrorKind::StorageFull => {
                    return Some(format!(
                            "The target ran out of space while copying {}.",
                            entry.path().display()
                        ));
                }
                Err(err) => {
                    let _ = tx.send(CopyEvent::Error(format!(
                        "Couldn't copy {}: {err}",
                        entry.path().display()
                    )));
                }
            }
        }
    }
    None
}

/// Copies `src` to a new file at `dst` (never overwriting), returning its
/// blake3 hash, size, and modification time, or `None` if cancelled
/// part-way. Any partial `dst` is deleted on failure or cancellation.
fn copy_file_hashed(
    src: &Path,
    dst: &Path,
    control: &JobControl,
    mut on_bytes: impl FnMut(u64),
) -> io::Result<Option<([u8; 32], u64, std::time::SystemTime)>> {
    let mut input = File::open(src)?;
    let meta = input.metadata()?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(dst)?;

    let result = (|| {
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; COPY_BUF_SIZE];
        let mut size = 0u64;
        loop {
            if control.checkpoint() {
                return Ok(None);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            size += n as u64;
            on_bytes(n as u64);
        }
        output.flush()?;
        let modified = meta.modified()?;
        let mut times = std::fs::FileTimes::new().set_modified(modified);
        #[cfg(windows)]
        if let Ok(created) = meta.created() {
            use std::os::windows::fs::FileTimesExt;
            times = times.set_created(created);
        }
        output.set_times(times)?;
        Ok(Some((*hasher.finalize().as_bytes(), size, modified)))
    })();

    if !matches!(result, Ok(Some(_))) {
        drop(output);
        let _ = std::fs::remove_file(dst);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn copies_a_tree_and_reports_hashes_matching_a_full_read() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("album/disc1")).unwrap();
        fs::create_dir(src.path().join("album/empty")).unwrap();
        fs::write(src.path().join("album/cover.jpg"), b"cover bytes").unwrap();
        fs::write(
            src.path().join("album/disc1/01.flac"),
            vec![7u8; 3 * 1024 * 1024],
        )
        .unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        copy_folders(
            vec![(src.path().join("album"), dst.path().join("album"))],
            tx,
            &JobControl::default(),
        );
        let events: Vec<CopyEvent> = rx.try_iter().collect();

        assert!(matches!(
            events.last(),
            Some(CopyEvent::Done { aborted: None, .. })
        ));
        assert!(dst.path().join("album/empty").is_dir());
        let copied = dst.path().join("album/disc1/01.flac");
        assert_eq!(fs::read(&copied).unwrap(), vec![7u8; 3 * 1024 * 1024]);
        assert_eq!(
            fs::metadata(&copied).unwrap().modified().unwrap(),
            fs::metadata(src.path().join("album/disc1/01.flac"))
                .unwrap()
                .modified()
                .unwrap()
        );

        let reported: Vec<(String, IndexedFile)> = events
            .into_iter()
            .filter_map(|e| match e {
                CopyEvent::FileCopied { hash_hex, file } => Some((hash_hex, file)),
                _ => None,
            })
            .collect();
        assert_eq!(reported.len(), 2);
        for (hash_hex, file) in reported {
            let on_disk = crate::scanner::full_hash(&file.absolute_path()).unwrap();
            assert_eq!(hash_hex, hex_encode(&on_disk));
        }
    }

    #[test]
    fn a_cancelled_copy_leaves_no_partial_file() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("f")).unwrap();
        fs::write(src.path().join("f/a.bin"), b"data").unwrap();

        let (tx, _rx) = crossbeam_channel::unbounded();
        copy_folders(
            vec![(src.path().join("f"), dst.path().join("f"))],
            tx,
            &JobControl::cancelled(),
        );
        assert!(!dst.path().join("f/a.bin").exists());
    }
}
