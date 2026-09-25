//! "Drive fill": copy whole top-level folders of a source directory onto a
//! target drive, choosing the subset that uses the target's free space as
//! fully as possible, and record every copied file in the reverse-search
//! index so it can later be found on whichever drive it ended up on.

pub mod copy;
pub mod measure;
pub mod plan;

use crate::app::SortDirection;
use crate::control::{ActiveClock, JobControl, estimate_remaining};
use crate::index_db::IndexedFile;
use crate::reverse_search::ReverseSearchState;
use crate::volume::{DiskSpace, disk_space};
use copy::CopyEvent;
use crossbeam_channel::Receiver;
use measure::{MeasureEvent, RootFolder};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const MB: u64 = 1024 * 1024;

/// Where a top-level source folder stands in the current plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderStatus {
    /// Will be copied.
    Planned,
    /// Left out: it doesn't fit alongside the folders picked instead.
    DoesNotFit,
    /// Unticked by the user.
    Excluded,
    /// A folder with the same name already exists in the target.
    ExistsInTarget,
}

impl FolderStatus {
    pub fn label(self) -> &'static str {
        match self {
            FolderStatus::Planned => "Will copy",
            FolderStatus::DoesNotFit => "Doesn't fit",
            FolderStatus::Excluded => "Excluded",
            FolderStatus::ExistsInTarget => "Already in target",
        }
    }
}

/// Sortable columns of the folder overview table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderSortColumn {
    Name,
    Size,
    Files,
    Modified,
    Created,
    Status,
}

#[derive(Default)]
pub struct FillPlan {
    /// Per entry of `DriveFillState::folders`.
    pub statuses: Vec<FolderStatus>,
    /// Sum of file sizes of the planned folders.
    pub planned_bytes: u64,
    /// Estimated on-disk footprint of the planned folders.
    pub planned_footprint: u64,
    /// Space the plan was allowed to use (free space minus the reserve).
    pub capacity: u64,
}

impl FillPlan {
    pub fn planned_count(&self) -> usize {
        self.statuses
            .iter()
            .filter(|s| **s == FolderStatus::Planned)
            .count()
    }
}

pub enum MeasureState {
    Idle,
    Running {
        rx: Receiver<MeasureEvent>,
        control: Arc<JobControl>,
        done: usize,
        total: usize,
    },
}

/// A running copy onto the target.
pub struct CopyJob {
    rx: Receiver<CopyEvent>,
    control: Arc<JobControl>,
    clock: ActiveClock,
    pub total_bytes: u64,
    pub bytes_done: u64,
    pub files_copied: usize,
    pub errors: Vec<String>,
    pub current: Option<PathBuf>,
    entries: Vec<(String, IndexedFile)>,
}

impl CopyJob {
    pub fn bytes_per_sec(&self) -> Option<f64> {
        let secs = self.clock.elapsed().as_secs_f64();
        (self.bytes_done > 0 && secs > 0.0).then(|| self.bytes_done as f64 / secs)
    }

    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(self.bytes_done, self.total_bytes, self.clock.elapsed())
    }

    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            1.0
        } else {
            self.bytes_done as f32 / self.total_bytes as f32
        }
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

pub struct DriveFillState {
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
    /// Space to leave free on the target, in MiB.
    pub reserve_mb: u64,
    pub folders: Vec<RootFolder>,
    /// Folders (by source path) the user unticked.
    pub excluded: HashSet<PathBuf>,
    pub measure: MeasureState,
    pub copy: Option<CopyJob>,
    pub space: Option<DiskSpace>,
    /// Names of the folders already in the target, read alongside `space`.
    existing_in_target: HashSet<String>,
    /// A background read of the target's free space and folder names, so a
    /// sleeping or slow target drive never stalls the UI.
    space_rx: Option<Receiver<(Option<DiskSpace>, HashSet<String>)>>,
    pub plan: FillPlan,
    pub status: Option<String>,
    /// Overview table sort; `None` keeps the default largest-first order.
    pub sort: Option<(FolderSortColumn, SortDirection)>,
}

impl Default for DriveFillState {
    fn default() -> Self {
        Self {
            source: None,
            target: None,
            reserve_mb: 256,
            folders: Vec::new(),
            excluded: HashSet::new(),
            measure: MeasureState::Idle,
            copy: None,
            space: None,
            existing_in_target: HashSet::new(),
            space_rx: None,
            plan: FillPlan::default(),
            status: None,
            sort: None,
        }
    }
}

impl DriveFillState {
    pub fn is_measuring(&self) -> bool {
        matches!(self.measure, MeasureState::Running { .. })
    }

    pub fn is_copying(&self) -> bool {
        self.copy.is_some()
    }

    pub fn is_busy(&self) -> bool {
        self.is_measuring() || self.is_copying()
    }

    pub fn is_reading_target(&self) -> bool {
        self.space_rx.is_some()
    }

    /// Whether `drain_events` has anything to wait for.
    pub fn needs_polling(&self) -> bool {
        self.is_busy() || self.is_reading_target()
    }

    /// Picks the folder whose subfolders get distributed, and starts sizing
    /// them up in the background.
    pub fn set_source(&mut self, source: PathBuf) {
        if self.is_busy() {
            return;
        }
        self.source = Some(source.clone());
        self.folders.clear();
        self.excluded.clear();
        self.status = None;
        self.replan();

        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || {
            measure::measure_root_folders(source, tx, &control_for_thread);
        });
        self.measure = MeasureState::Running {
            rx,
            control,
            done: 0,
            total: 0,
        };
    }

    pub fn cancel_measure(&mut self) {
        if let MeasureState::Running { control, .. } = &self.measure {
            control.cancel();
        }
    }

    pub fn set_target(&mut self, target: PathBuf) {
        if self.is_copying() {
            return;
        }
        self.target = Some(target);
        self.refresh_space();
    }

    /// Re-reads the target's free space and existing folders in the
    /// background; the plan is rebuilt once they arrive.
    pub fn refresh_space(&mut self) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let space = disk_space(&target);
            let existing = std::fs::read_dir(&target)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            let _ = tx.send((space, existing));
        });
        self.space_rx = Some(rx);
    }

    pub fn set_excluded(&mut self, index: usize, excluded: bool) {
        let Some(folder) = self.folders.get(index) else {
            return;
        };
        if excluded {
            self.excluded.insert(folder.path.clone());
        } else {
            self.excluded.remove(&folder.path);
        }
        self.replan();
    }

    /// Indices into `folders` in the overview table's current sort order.
    pub fn sorted_indices(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.folders.len()).collect();
        let Some((column, direction)) = self.sort else {
            return order;
        };
        let status_rank = |i: usize| {
            self.plan
                .statuses
                .get(i)
                .map_or(u8::MAX, |s| match s {
                    FolderStatus::Planned => 0,
                    FolderStatus::DoesNotFit => 1,
                    FolderStatus::ExistsInTarget => 2,
                    FolderStatus::Excluded => 3,
                })
        };
        order.sort_by(|&a, &b| {
            let (fa, fb) = (&self.folders[a], &self.folders[b]);
            let ord = match column {
                FolderSortColumn::Name => fa.name.to_lowercase().cmp(&fb.name.to_lowercase()),
                FolderSortColumn::Size => fa.size.cmp(&fb.size),
                FolderSortColumn::Files => fa.file_count.cmp(&fb.file_count),
                FolderSortColumn::Modified => fa.modified.cmp(&fb.modified),
                FolderSortColumn::Created => fa.created.cmp(&fb.created),
                FolderSortColumn::Status => status_rank(a).cmp(&status_rank(b)),
            };
            match direction {
                SortDirection::Asc => ord,
                SortDirection::Desc => ord.reverse(),
            }
        });
        order
    }

    /// Recomputes which folders to copy. Cheap enough to run on every input
    /// change (see `plan::MAX_DP_CELLS`), but not every frame.
    pub fn replan(&mut self) {
        let cluster = self.space.map_or(4096, |s| s.cluster);
        let capacity = self
            .space
            .map_or(0, |s| s.free.saturating_sub(self.reserve_mb * MB));

        let mut statuses = vec![FolderStatus::DoesNotFit; self.folders.len()];
        let mut candidates = Vec::new();
        for (i, folder) in self.folders.iter().enumerate() {
            if self.excluded.contains(&folder.path) {
                statuses[i] = FolderStatus::Excluded;
            } else if self.existing_in_target.contains(&folder.name) {
                statuses[i] = FolderStatus::ExistsInTarget;
            } else {
                candidates.push(i);
            }
        }

        let weights: Vec<u64> = candidates
            .iter()
            .map(|&i| self.folders[i].footprint(cluster))
            .collect();
        let mut plan = FillPlan {
            capacity,
            ..FillPlan::default()
        };
        if self.target.is_some() {
            for k in plan::choose_best_fit(&weights, capacity) {
                let i = candidates[k];
                statuses[i] = FolderStatus::Planned;
                plan.planned_bytes += self.folders[i].size;
                plan.planned_footprint += weights[k];
            }
        }
        plan.statuses = statuses;
        self.plan = plan;
    }

    /// Starts copying every planned folder into the target.
    pub fn start_copy(&mut self) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if self.is_busy() || self.plan.planned_count() == 0 {
            return;
        }
        let jobs: Vec<(PathBuf, PathBuf)> = self
            .folders
            .iter()
            .zip(&self.plan.statuses)
            .filter(|(_, s)| **s == FolderStatus::Planned)
            .map(|(f, _)| (f.path.clone(), target.join(&f.name)))
            .collect();

        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || copy::copy_folders(jobs, tx, &control_for_thread));
        self.status = None;
        self.copy = Some(CopyJob {
            rx,
            control,
            clock: ActiveClock::start(),
            total_bytes: self.plan.planned_bytes,
            bytes_done: 0,
            files_copied: 0,
            errors: Vec::new(),
            current: None,
            entries: Vec::new(),
        });
    }

    /// Drains measuring and copying events. A finished copy's files are
    /// merged into `reverse_search`'s index (and saved) right away.
    pub fn drain_events(&mut self, reverse_search: &mut ReverseSearchState) -> bool {
        let mut changed = false;

        if let Some(rx) = &self.space_rx
            && let Ok((space, existing)) = rx.try_recv()
        {
            self.space = space;
            self.existing_in_target = existing;
            self.space_rx = None;
            self.replan();
            changed = true;
        }

        let mut measured = None;
        if let MeasureState::Running {
            rx, done, total, ..
        } = &mut self.measure
        {
            for event in rx.try_iter().take(500) {
                changed = true;
                match event {
                    MeasureEvent::Progress { done: d, total: t } => {
                        *done = d;
                        *total = t;
                    }
                    MeasureEvent::Error(msg) => self.status = Some(msg),
                    MeasureEvent::Done(folders) => measured = Some(folders),
                }
            }
        }
        if let Some(mut folders) = measured {
            folders.sort_by_key(|f| std::cmp::Reverse(f.size));
            self.folders = folders;
            self.measure = MeasureState::Idle;
            self.replan();
        }

        let mut finished = None;
        if let Some(job) = &mut self.copy {
            for event in job.rx.try_iter().take(2000) {
                changed = true;
                match event {
                    CopyEvent::FileStarted { path } => job.current = Some(path),
                    CopyEvent::Progress { bytes_done } => job.bytes_done = bytes_done,
                    CopyEvent::FileCopied { hash_hex, file } => {
                        job.files_copied += 1;
                        job.entries.push((hash_hex, file));
                    }
                    CopyEvent::Error(msg) => job.errors.push(msg),
                    CopyEvent::Done { aborted, usage } => finished = Some((aborted, usage)),
                }
            }
        }
        if let Some((aborted, usage)) = finished
            && let Some(job) = self.copy.take()
        {
            self.finish_copy(job, aborted, usage, reverse_search);
        }
        changed
    }

    fn finish_copy(
        &mut self,
        job: CopyJob,
        aborted: Option<String>,
        usage: Option<(String, String, crate::index_db::VolumeUsage)>,
        reverse_search: &mut ReverseSearchState,
    ) {
        let cancelled = job.is_cancelled();
        let copied = job.files_copied;
        let errors = job.errors.len();
        let indexed = reverse_search.add_to_index(job.entries, usage);

        let mut msg = match (&aborted, cancelled) {
            (Some(reason), _) => format!("Stopped: {reason} Copied {copied} file(s)"),
            (None, true) => format!("Cancelled. Copied {copied} file(s)"),
            (None, false) => format!("Copied {copied} file(s)"),
        };
        match indexed {
            Ok(()) => msg.push_str(" and added them to the reverse-search index."),
            Err(err) => msg.push_str(&format!(
                ", but saving the reverse-search index failed: {err}"
            )),
        }
        if errors > 0 {
            msg.push_str(&format!(" {errors} error(s); first: {}", job.errors[0]));
        }
        self.status = Some(msg);
        self.refresh_space();
    }
}
