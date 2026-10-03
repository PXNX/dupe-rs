//! "Metadata Backup": record every file's name, location, size, created and
//! modified dates, and photo EXIF data (date taken, GPS, camera) in a JSON
//! backup, then later compare a backup with the files as they are now and
//! put back whatever got lost: names and locations, dates, date taken, GPS.

pub mod exif;
pub mod restore;
pub mod snapshot;

use crate::control::{ActiveClock, JobControl, estimate_remaining};
use crossbeam_channel::Receiver;
use restore::{Comparison, RestoreOptions, RestoreReport};
use snapshot::Snapshot;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Files processed so far, shared with the worker thread.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Backup,
    Compare,
    Restore,
}

enum JobResult {
    BackedUp(Result<(PathBuf, usize), String>),
    Compared(Result<Comparison, String>),
    Restored {
        report: RestoreReport,
        safety_backup: Result<PathBuf, String>,
    },
}

pub struct MetadataJob {
    pub kind: JobKind,
    rx: Receiver<Option<JobResult>>,
    control: Arc<JobControl>,
    progress: Arc<Progress>,
    clock: ActiveClock,
}

impl MetadataJob {
    pub fn done(&self) -> usize {
        self.progress.done.load(Ordering::Relaxed)
    }

    /// 0 while the files are still being listed.
    pub fn total(&self) -> usize {
        self.progress.total.load(Ordering::Relaxed)
    }

    pub fn fraction(&self) -> f32 {
        match self.total() {
            0 => 0.0,
            total => self.done() as f32 / total as f32,
        }
    }

    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(
            self.done() as u64,
            self.total() as u64,
            self.clock.elapsed(),
        )
    }

    pub fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    pub fn toggle_pause(&mut self) {
        let paused = !self.control.is_paused();
        self.control.set_paused(paused);
        if paused {
            self.clock.pause();
        } else {
            self.clock.resume();
        }
    }

    pub fn cancel(&mut self) {
        self.control.cancel();
        self.clock.resume();
    }

    pub fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
    }
}

pub struct MetadataBackupState {
    /// The folder "Back up" records.
    pub root: Option<PathBuf>,
    /// Where backups are saved (and listed from).
    pub backups_dir: PathBuf,
    /// Saved backups, newest first. Refreshed after each job.
    pub backups: Vec<PathBuf>,
    /// The backup "Compare" reads.
    pub selected_backup: Option<PathBuf>,
    pub comparison: Option<Comparison>,
    pub options: RestoreOptions,
    pub job: Option<MetadataJob>,
    pub status: Option<String>,
    pub errors: Vec<String>,
}

impl Default for MetadataBackupState {
    fn default() -> Self {
        Self::with_backups_dir(default_backups_dir())
    }
}

/// `%LOCALAPPDATA%\dupe-rs\metadata-backups`.
pub fn default_backups_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("dupe-rs").join("metadata-backups")
}

impl MetadataBackupState {
    pub fn with_backups_dir(backups_dir: PathBuf) -> Self {
        let backups = snapshot::list_backups(&backups_dir);
        Self {
            root: None,
            selected_backup: backups.first().cloned(),
            backups,
            backups_dir,
            comparison: None,
            options: RestoreOptions {
                locations: true,
                times: true,
                exif: true,
            },
            job: None,
            status: None,
            errors: Vec::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn set_root(&mut self, root: PathBuf) {
        if !self.is_running() {
            self.root = Some(root);
            self.status = None;
            self.errors.clear();
        }
    }

    pub fn select_backup(&mut self, path: PathBuf) {
        if !self.is_running() && self.selected_backup.as_ref() != Some(&path) {
            self.selected_backup = Some(path);
            self.comparison = None;
            self.status = None;
            self.errors.clear();
        }
    }

    fn spawn(
        &mut self,
        kind: JobKind,
        work: impl FnOnce(&JobControl, &Progress) -> Option<JobResult> + Send + 'static,
    ) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let control = Arc::new(JobControl::default());
        let progress = Arc::new(Progress::default());
        let (c, p) = (control.clone(), progress.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work(&c, &p));
        });
        self.status = None;
        self.errors.clear();
        self.job = Some(MetadataJob {
            kind,
            rx,
            control,
            progress,
            clock: ActiveClock::start(),
        });
    }

    /// Records the chosen folder's files into a new backup.
    pub fn back_up(&mut self) {
        let Some(root) = self.root.clone().filter(|_| !self.is_running()) else {
            return;
        };
        let dir = self.backups_dir.clone();
        self.spawn(JobKind::Backup, move |control, progress| {
            let files = snapshot::capture(&root, control, progress)?;
            let count = files.len();
            let saved = Snapshot::new(root, files).save_in(&dir, "");
            Some(JobResult::BackedUp(saved.map(|path| (path, count))))
        });
    }

    /// Compares the selected backup with the files in its folder now.
    pub fn compare(&mut self) {
        let Some(backup) = self.selected_backup.clone().filter(|_| !self.is_running()) else {
            return;
        };
        self.comparison = None;
        self.spawn(JobKind::Compare, move |control, progress| {
            let result = (|| {
                let snapshot = Snapshot::load(&backup)?;
                if !snapshot.root.is_dir() {
                    return Err(format!(
                        "The backed-up folder {} doesn't exist any more",
                        snapshot.root.display()
                    ));
                }
                let current =
                    snapshot::capture(&snapshot.root, control, progress).ok_or_else(String::new)?;
                Ok(restore::compare(snapshot, current))
            })();
            match result {
                Err(e) if e.is_empty() => None,
                result => Some(JobResult::Compared(result)),
            }
        });
    }

    pub fn restore_steps(&self) -> usize {
        self.comparison
            .as_ref()
            .map_or(0, |c| restore::restore_steps(&c.changes, self.options))
    }

    /// Puts back the chosen kinds of change from the last comparison, after
    /// saving the files' current state as a backup of its own.
    pub fn restore(&mut self) {
        if self.is_running() || self.restore_steps() == 0 {
            return;
        }
        let Some(comparison) = self.comparison.take() else {
            return;
        };
        let options = self.options;
        let dir = self.backups_dir.clone();
        self.spawn(JobKind::Restore, move |control, progress| {
            let Comparison {
                root,
                changes,
                current,
                ..
            } = comparison;
            let safety_backup =
                Snapshot::new(root.clone(), current).save_in(&dir, "before restore");
            if safety_backup.is_err() {
                return Some(JobResult::Restored {
                    report: RestoreReport::default(),
                    safety_backup,
                });
            }
            progress
                .total
                .store(restore::restore_steps(&changes, options), Ordering::Relaxed);
            let report = restore::restore(&root, &changes, options, control, progress);
            Some(JobResult::Restored {
                report,
                safety_backup,
            })
        });
    }

    pub fn drain_events(&mut self) -> bool {
        let Some(job) = &self.job else {
            return false;
        };
        let Ok(result) = job.rx.try_recv() else {
            return false;
        };
        let job = self.job.take().expect("checked above");
        let cancelled = if job.is_cancelled() {
            "Cancelled. "
        } else {
            ""
        };
        let mut compare_again = false;
        match result {
            None => self.status = Some("Cancelled.".to_owned()),
            Some(JobResult::BackedUp(Ok((path, count)))) => {
                self.status = Some(format!("Backed up {count} file(s) to {}.", path.display()));
                self.selected_backup = Some(path);
                self.comparison = None;
            }
            Some(JobResult::Compared(Ok(comparison))) => self.comparison = Some(comparison),
            Some(JobResult::BackedUp(Err(e)) | JobResult::Compared(Err(e))) => self.errors.push(e),
            Some(JobResult::Restored {
                report,
                safety_backup,
            }) => match safety_backup {
                Err(e) => self.errors.push(format!("Nothing was restored. {e}")),
                Ok(path) => {
                    self.status = Some(format!(
                        "{cancelled}Moved {} file(s) back, restored dates of {} and EXIF data of {}. \
                         The state before restoring was saved as {}.",
                        report.moved,
                        report.times,
                        report.exif,
                        file_name(&path)
                    ));
                    self.errors = report.errors;
                    compare_again = true;
                }
            },
        }
        self.backups = snapshot::list_backups(&self.backups_dir);
        if compare_again {
            // Show what (if anything) still differs.
            let status = self.status.take();
            let errors = std::mem::take(&mut self.errors);
            self.compare();
            self.status = status;
            self.errors = errors;
        }
        true
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::exif::{ExifInfo, Gps};
    use super::*;
    use crate::metadata_backup::restore::set_times;
    use std::fs;
    use std::time::{Instant, SystemTime};

    fn settle(state: &mut MetadataBackupState) {
        let start = Instant::now();
        while state.is_running() {
            state.drain_events();
            assert!(start.elapsed() < Duration::from_secs(20), "job timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(state.errors.is_empty(), "{:?}", state.errors);
    }

    fn times(path: &Path) -> (SystemTime, SystemTime) {
        let meta = fs::metadata(path).unwrap();
        (meta.created().unwrap(), meta.modified().unwrap())
    }

    #[test]
    fn restores_names_locations_dates_and_gps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("photos");
        fs::create_dir_all(root.join("2019")).unwrap();
        let photo = root.join("2019/beach.jpg");
        exif::tests::write_plain_jpeg(&photo);
        let place = ExifInfo {
            date_taken: Some("2019:07:14 18:03:22".to_owned()),
            gps: Some(Gps {
                latitude: 43.7,
                longitude: 7.26,
                altitude: None,
            }),
            camera: None,
        };
        exif::write(&photo, &place).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_563_120_000);
        set_times(&photo, Some(old), Some(old)).unwrap();
        // Two files that will swap names.
        fs::write(root.join("a.txt"), b"first file").unwrap();
        fs::write(root.join("b.txt"), b"second file").unwrap();
        let (a_created, a_modified) = times(&root.join("a.txt"));

        let mut state = MetadataBackupState::with_backups_dir(dir.path().join("backups"));
        state.set_root(root.clone());
        state.back_up();
        settle(&mut state);
        assert_eq!(state.backups.len(), 1);

        // Mess things up: strip the photo's EXIF and move it, change its
        // dates, swap the two text files' names.
        fs::remove_file(&photo).unwrap();
        let moved_photo = root.join("IMG_0001.jpg");
        exif::tests::write_plain_jpeg(&moved_photo);
        fs::rename(root.join("a.txt"), root.join("tmp")).unwrap();
        fs::rename(root.join("b.txt"), root.join("a.txt")).unwrap();
        fs::rename(root.join("tmp"), root.join("b.txt")).unwrap();
        let later = SystemTime::now() - Duration::from_secs(3600);
        set_times(&root.join("b.txt"), Some(later), Some(later)).unwrap();

        state.compare();
        settle(&mut state);
        let comparison = state.comparison.as_ref().unwrap();
        // The re-written photo has new contents, so it isn't recognised.
        assert_eq!(
            comparison.missing,
            vec![Path::new("2019").join("beach.jpg")]
        );
        assert_eq!(comparison.moved_count(), 2);
        assert_eq!(comparison.times_count(), 1);

        // Put the photo back where it was: now it's found by path and only
        // its EXIF and dates differ.
        fs::rename(&moved_photo, &photo).unwrap();
        state.compare();
        settle(&mut state);
        let comparison = state.comparison.as_ref().unwrap();
        assert!(comparison.missing.is_empty());
        assert_eq!(comparison.exif_count(), 1);

        state.restore();
        settle(&mut state);
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"first file");
        assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"second file");
        let (created, modified) = times(&root.join("a.txt"));
        assert_eq!(modified, a_modified);
        if cfg!(windows) {
            assert_eq!(created, a_created);
        }
        let got = exif::read(&photo).unwrap();
        assert_eq!(got.date_taken, place.date_taken);
        assert!(got.gps.unwrap().same_place(&place.gps.unwrap()));
        assert_eq!(times(&photo).1, old);

        // Restoring compared again: nothing left to do, and the state
        // before restoring was kept as a second backup.
        let comparison = state.comparison.as_ref().unwrap();
        assert!(comparison.changes.is_empty(), "{:?}", comparison.changes);
        assert_eq!(state.backups.len(), 2);
        assert!(
            state
                .status
                .as_ref()
                .unwrap()
                .contains("Moved 2 file(s) back")
        );
    }

    #[test]
    fn never_overwrites_a_file_in_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("docs");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("report.txt"), b"the report").unwrap();

        let mut state = MetadataBackupState::with_backups_dir(dir.path().join("backups"));
        state.set_root(root.clone());
        state.back_up();
        settle(&mut state);

        fs::rename(root.join("report.txt"), root.join("renamed.txt")).unwrap();
        state.compare();
        settle(&mut state);
        fs::write(root.join("report.txt"), b"a new file").unwrap();
        state.restore();
        let start = Instant::now();
        while state.is_running() || state.comparison.is_none() {
            state.drain_events();
            assert!(start.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.errors.len(), 1, "{:?}", state.errors);
        assert_eq!(fs::read(root.join("report.txt")).unwrap(), b"a new file");
        assert_eq!(fs::read(root.join("renamed.txt")).unwrap(), b"the report");
    }
}
