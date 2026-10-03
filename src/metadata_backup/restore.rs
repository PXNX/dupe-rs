//! Comparing a backup with the files as they are now, and putting back what
//! changed: names and locations, created/modified dates, and EXIF date
//! taken and GPS position.

use super::Progress;
use super::exif::{self, ExifInfo};
use super::snapshot::{FileRecord, Snapshot};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::SystemTime;

/// File-system timestamps closer than this count as unchanged: FAT drives
/// store modified times in 2-second steps.
const TIME_TOLERANCE_MS: i64 = 2000;

/// One backed-up file that's still around but no longer as backed up.
#[derive(Clone, Debug)]
pub struct FileChange {
    pub backup: FileRecord,
    /// Where the file is now, relative to the root.
    pub current_path: PathBuf,
    pub current_created: Option<DateTime<Utc>>,
    pub current_modified: Option<DateTime<Utc>>,
    pub created_changed: bool,
    pub modified_changed: bool,
    /// The backup has a date taken / GPS position the file has lost or
    /// that was changed since, and the file's format can take it back.
    pub exif_changed: bool,
}

impl FileChange {
    pub fn moved(&self) -> bool {
        self.current_path != self.backup.path
    }

    pub fn times_changed(&self) -> bool {
        self.created_changed || self.modified_changed
    }
}

pub struct Comparison {
    pub root: PathBuf,
    pub taken_at: DateTime<Utc>,
    pub backed_up: usize,
    pub unchanged: usize,
    pub changes: Vec<FileChange>,
    /// Backed-up files that couldn't be found any more.
    pub missing: Vec<PathBuf>,
    /// The files as they are now, saved as a backup of its own before a
    /// restore changes anything.
    pub current: Vec<FileRecord>,
}

impl Comparison {
    pub fn moved_count(&self) -> usize {
        self.changes.iter().filter(|c| c.moved()).count()
    }

    pub fn times_count(&self) -> usize {
        self.changes.iter().filter(|c| c.times_changed()).count()
    }

    pub fn exif_count(&self) -> usize {
        self.changes.iter().filter(|c| c.exif_changed).count()
    }
}

fn times_differ(backup: Option<DateTime<Utc>>, current: Option<DateTime<Utc>>) -> bool {
    match (backup, current) {
        (Some(b), Some(c)) => (b - c).num_milliseconds().abs() > TIME_TOLERANCE_MS,
        _ => false,
    }
}

fn exif_differs(backup: Option<&ExifInfo>, current: Option<&ExifInfo>) -> bool {
    let Some(backup) = backup else {
        return false;
    };
    let date_lost = backup.date_taken.is_some()
        && backup.date_taken != current.and_then(|c| c.date_taken.clone());
    let gps_lost = backup.gps.is_some_and(|b| {
        !current
            .and_then(|c| c.gps)
            .is_some_and(|c| b.same_place(&c))
    });
    date_lost || gps_lost
}

/// Matches each backed-up file to a file that's there now: one with the same
/// contents fingerprint (found even after a rename or move, as long as the
/// fingerprint is unique on both sides), else the file at the same path.
pub fn compare(snapshot: Snapshot, current: Vec<FileRecord>) -> Comparison {
    let mut backup_ids: HashMap<(u64, &str), usize> = HashMap::new();
    for f in &snapshot.files {
        if let Some(id) = f.identity() {
            *backup_ids.entry(id).or_default() += 1;
        }
    }
    let mut current_ids: HashMap<(u64, &str), Vec<usize>> = HashMap::new();
    for (i, f) in current.iter().enumerate() {
        if let Some(id) = f.identity() {
            current_ids.entry(id).or_default().push(i);
        }
    }
    let by_path: HashMap<&Path, usize> = current
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_path(), i))
        .collect();

    let mut matched: Vec<Option<usize>> = vec![None; snapshot.files.len()];
    let mut claimed = HashSet::new();
    for (b, f) in snapshot.files.iter().enumerate() {
        if let Some(id) = f.identity()
            && backup_ids[&id] == 1
            && let Some([c]) = current_ids.get(&id).map(Vec::as_slice)
        {
            matched[b] = Some(*c);
            claimed.insert(*c);
        }
    }
    for (b, f) in snapshot.files.iter().enumerate() {
        if matched[b].is_none()
            && let Some(&c) = by_path.get(f.path.as_path())
            && claimed.insert(c)
        {
            matched[b] = Some(c);
        }
    }

    let mut changes = Vec::new();
    let mut missing = Vec::new();
    let mut unchanged = 0;
    for (b, backup) in snapshot.files.iter().enumerate() {
        let Some(c) = matched[b] else {
            missing.push(backup.path.clone());
            continue;
        };
        let now = &current[c];
        let change = FileChange {
            backup: backup.clone(),
            current_path: now.path.clone(),
            current_created: now.created,
            current_modified: now.modified,
            created_changed: times_differ(backup.created, now.created),
            modified_changed: times_differ(backup.modified, now.modified),
            exif_changed: exif_differs(backup.exif.as_ref(), now.exif.as_ref())
                && exif::is_writable(&now.path),
        };
        if change.moved() || change.times_changed() || change.exif_changed {
            changes.push(change);
        } else {
            unchanged += 1;
        }
    }

    Comparison {
        root: snapshot.root,
        taken_at: snapshot.taken_at,
        backed_up: snapshot.files.len(),
        unchanged,
        changes,
        missing,
        current,
    }
}

/// Which kinds of change a restore puts back.
#[derive(Clone, Copy, Debug)]
pub struct RestoreOptions {
    pub locations: bool,
    pub times: bool,
    pub exif: bool,
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub moved: usize,
    pub times: usize,
    pub exif: usize,
    pub errors: Vec<String>,
}

/// How many steps `restore` will report progress for.
pub fn restore_steps(changes: &[FileChange], options: RestoreOptions) -> usize {
    changes
        .iter()
        .map(|c| {
            usize::from(options.exif && c.exif_changed)
                + usize::from(options.locations && c.moved())
                + usize::from(options.times && c.times_changed())
        })
        .sum()
}

/// Puts the chosen kinds of change back. EXIF goes first (rewriting a file
/// changes its dates), then names and locations, then dates. A file is never
/// overwritten: a move whose original spot is taken is skipped.
pub fn restore(
    root: &Path,
    changes: &[FileChange],
    options: RestoreOptions,
    control: &crate::control::JobControl,
    progress: &Progress,
) -> RestoreReport {
    let mut report = RestoreReport::default();
    let step = || progress.done.fetch_add(1, Ordering::Relaxed);
    let mut paths: Vec<PathBuf> = changes.iter().map(|c| root.join(&c.current_path)).collect();

    if options.exif {
        for (change, path) in changes.iter().zip(&paths) {
            if !change.exif_changed {
                continue;
            }
            if control.checkpoint() {
                return report;
            }
            let wanted = change.backup.exif.clone().unwrap_or_default();
            let kept_times = file_times(path);
            match exif::write(path, &wanted) {
                Ok(()) => {
                    report.exif += 1;
                    // Rewriting the file mustn't change its dates.
                    if let Some((created, modified)) = kept_times {
                        let _ = set_times(path, created, modified);
                    }
                }
                Err(e) => report.errors.push(format!("{}: {e}", path.display())),
            }
            step();
        }
    }

    if options.locations {
        // Every file goes to a temporary name in its destination folder
        // first, so files that swapped names can each get theirs back.
        // Renaming into a name that was just vacated makes NTFS hand the
        // file the old one's creation date ("file tunneling"), so each
        // file's dates are put back after its final rename.
        let mut staged = Vec::new();
        for (i, change) in changes.iter().enumerate() {
            if !change.moved() {
                continue;
            }
            if control.checkpoint() {
                break;
            }
            let to = root.join(&change.backup.path);
            let tmp = to.with_file_name(format!(".dupe-rs-restore-{i}.tmp"));
            let kept_times = file_times(&paths[i]);
            let result = to
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::rename(&paths[i], &tmp));
            match result {
                Ok(()) => staged.push((i, tmp, to, kept_times)),
                Err(e) => {
                    report
                        .errors
                        .push(format!("Couldn't move {}: {e}", paths[i].display()));
                    step();
                }
            }
        }
        // Always finished, even when cancelled: no file is left behind
        // under a temporary name.
        for (i, tmp, to, kept_times) in staged {
            if to.exists() {
                report.errors.push(format!(
                    "Didn't move {} back: {} already exists",
                    paths[i].display(),
                    to.display()
                ));
                let _ = fs::rename(&tmp, &paths[i]);
            } else {
                match fs::rename(&tmp, &to) {
                    Ok(()) => {
                        report.moved += 1;
                        paths[i] = to;
                    }
                    Err(e) => {
                        report
                            .errors
                            .push(format!("Couldn't move {}: {e}", paths[i].display()));
                        let _ = fs::rename(&tmp, &paths[i]);
                    }
                }
            }
            if let Some((created, modified)) = kept_times {
                let _ = set_times(&paths[i], created, modified);
            }
            step();
        }
    }

    if options.times {
        for (change, path) in changes.iter().zip(&paths) {
            if !change.times_changed() {
                continue;
            }
            if control.checkpoint() {
                return report;
            }
            let created = change.backup.created.map(SystemTime::from);
            let modified = change.backup.modified.map(SystemTime::from);
            match set_times(path, created, modified) {
                Ok(()) => report.times += 1,
                Err(e) => report
                    .errors
                    .push(format!("Couldn't set the dates of {}: {e}", path.display())),
            }
            step();
        }
    }
    report
}

fn file_times(path: &Path) -> Option<(Option<SystemTime>, Option<SystemTime>)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.created().ok(), meta.modified().ok()))
}

/// Sets a file's modified and (on Windows) created time. Works on read-only
/// files too: it only asks for the right to change attributes.
pub fn set_times(
    path: &Path,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        options.access_mode(FILE_WRITE_ATTRIBUTES);
    }
    #[cfg(not(windows))]
    options.write(true);
    let file = options.open(path)?;

    let mut times = fs::FileTimes::new();
    if let Some(modified) = modified {
        times = times.set_modified(modified);
    }
    #[cfg(windows)]
    if let Some(created) = created {
        use std::os::windows::fs::FileTimesExt;
        times = times.set_created(created);
    }
    #[cfg(not(windows))]
    let _ = created;
    file.set_times(times)
}
