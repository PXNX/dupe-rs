use crate::config::{ExtensionFilter, ScanConfig, ScanMode, SizeUnit, parse_extension_list};
use crate::model::{DeleteEvent, DupeGroup, FileEntry, MediaEntry, ScanEvent, SimilarGroup};
use crate::reverse_search::ReverseSearchState;
use crate::scanner;
use crate::selection::{compute_visible_entries, compute_visible_media_entries};
use crate::sound::{self, Sound};
use crate::taskbar::{Taskbar, TaskbarProgress};
use crate::ui::thumbnails::ThumbnailCache;
use crate::view_cache::ExactRowsCache;
use crossbeam_channel::Receiver;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Estimates time left from linear progress so far: `done` out of `total`
/// units after `elapsed`. `None` until there's a rate to go on, or once done.
pub fn estimate_remaining(done: u64, total: u64, elapsed: Duration) -> Option<Duration> {
    if done == 0 || done >= total {
        return None;
    }
    let rate = done as f64 / elapsed.as_secs_f64();
    if !rate.is_finite() || rate <= 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64((total - done) as f64 / rate))
}

/// Byte-level progress of the full-file hashing pass, the slow, I/O-bound
/// part of a scan on large trees. Used to show a GB-scanned readout and ETA.
pub struct HashProgress {
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub started_at: Instant,
}

impl HashProgress {
    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(self.done_bytes, self.total_bytes, self.started_at.elapsed())
    }
}

pub enum ScanState {
    Idle,
    Running {
        rx: Receiver<ScanEvent>,
        cancel: Arc<AtomicBool>,
        scanned: usize,
        hash_progress: Option<HashProgress>,
    },
    Done {
        elapsed_ms: u128,
    },
}

/// A background delete in flight, fed by `DeleteEvent`s from its worker
/// thread.
pub struct DeleteJob {
    rx: Receiver<DeleteEvent>,
    pub total: usize,
    pub done: usize,
    pub deleted_paths: HashSet<PathBuf>,
    pub skipped: usize,
    /// Whether files are removed outright rather than moved to the trash.
    pub permanent: bool,
    pub started_at: Instant,
    /// The file the worker is on right now, for the progress readout.
    pub current: Option<PathBuf>,
}

impl DeleteJob {
    /// Files processed per second so far, or `None` before the first one.
    pub fn items_per_sec(&self) -> Option<f64> {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        (self.done > 0 && elapsed > 0.0).then(|| self.done as f64 / elapsed)
    }

    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(self.done as u64, self.total as u64, self.started_at.elapsed())
    }
}

pub enum DeleteState {
    Idle,
    Running(DeleteJob),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionMode {
    All,
    Include,
    Exclude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Table,
    Grid,
}

/// Top-level section the window is showing: the regular duplicate scan, or
/// the reverse-search tab (pick one file, find its matches in a persisted,
/// previously-built index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppTab {
    Scan,
    ReverseSearch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Filename,
    Path,
    Size,
    Created,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Which file within a duplicate group a bulk-selection action should target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectCriterion {
    Oldest,
    Newest,
    ShortestPath,
    LongestPath,
}

impl SelectCriterion {
    pub const ALL: [SelectCriterion; 4] = [
        SelectCriterion::Oldest,
        SelectCriterion::Newest,
        SelectCriterion::ShortestPath,
        SelectCriterion::LongestPath,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SelectCriterion::Oldest => "Oldest",
            SelectCriterion::Newest => "Newest",
            SelectCriterion::ShortestPath => "Shortest path",
            SelectCriterion::LongestPath => "Longest path",
        }
    }
}

pub struct DeleteConfirmState {
    pub paths: Vec<PathBuf>,
    pub total_size: u64,
    /// Bound to the dialog's "skip the Recycle Bin" checkbox. Always starts
    /// unticked so the default stays the recoverable trash move.
    pub permanent: bool,
}

/// Shared by `FileEntry` and `MediaEntry` so `select_by_criterion` can pick a
/// target file by path length or modification time without duplicating that
/// logic once per group type.
trait TimedPath {
    fn path(&self) -> &Path;
    fn modified(&self) -> SystemTime;
}

impl TimedPath for FileEntry {
    fn path(&self) -> &Path {
        &self.path
    }
    fn modified(&self) -> SystemTime {
        self.modified
    }
}

impl TimedPath for MediaEntry {
    fn path(&self) -> &Path {
        &self.path
    }
    fn modified(&self) -> SystemTime {
        self.modified
    }
}

fn apply_criterion_selection<T: TimedPath>(
    files: &[T],
    criterion: SelectCriterion,
    invert: bool,
    selection: &mut HashSet<PathBuf>,
) {
    let target_idx = match criterion {
        SelectCriterion::Oldest => files.iter().enumerate().min_by_key(|(_, f)| f.modified()),
        SelectCriterion::Newest => files.iter().enumerate().max_by_key(|(_, f)| f.modified()),
        SelectCriterion::ShortestPath => files
            .iter()
            .enumerate()
            .min_by_key(|(_, f)| f.path().as_os_str().len()),
        SelectCriterion::LongestPath => files
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| f.path().as_os_str().len()),
    }
    .map(|(i, _)| i);
    let Some(target_idx) = target_idx else {
        return;
    };
    for (i, file) in files.iter().enumerate() {
        if (i == target_idx) != invert {
            selection.insert(file.path().to_path_buf());
        }
    }
}

pub struct DupeApp {
    pub config: ScanConfig,
    pub only_show_duplicates: bool,
    pub only_show_name_copies: bool,
    pub view_mode: ViewMode,
    pub groups: Vec<DupeGroup>,
    pub similar_groups: Vec<SimilarGroup>,
    /// Bumped every time `groups` is mutated (new group found, groups pruned
    /// after a delete, scan restarted). `exact_rows_cache` is keyed on this
    /// so it only rebuilds when the underlying data actually changed, rather
    /// than every frame.
    pub groups_generation: u64,
    pub exact_rows_cache: ExactRowsCache,
    /// The `ScanMode` the currently-displayed results came from; set when a
    /// scan starts so mid-scan mode changes never mismatch results and views.
    pub active_mode: ScanMode,
    pub selection: HashSet<PathBuf>,
    pub scan_state: ScanState,
    pub status_message: Option<String>,
    pub thumbnail_cache: ThumbnailCache,

    // Settings-panel transient UI state (raw text kept separately so users can
    // type freely; parsed into `config` only when a scan starts).
    pub min_size_text: String,
    pub max_size_text: String,
    pub size_unit: SizeUnit,
    pub extension_mode: ExtensionMode,
    pub extension_text: String,

    pub sort: Option<(SortColumn, SortDirection)>,
    pub select_criterion: SelectCriterion,
    pub select_invert: bool,

    pub delete_confirm: Option<DeleteConfirmState>,
    pub delete_state: DeleteState,

    pub tab: AppTab,
    pub reverse_search: ReverseSearchState,

    pub taskbar: Taskbar,
    /// Whether to play a chime when a scan or delete finishes.
    pub play_sounds: bool,
}

impl Default for DupeApp {
    fn default() -> Self {
        Self {
            config: ScanConfig::default(),
            only_show_duplicates: false,
            only_show_name_copies: false,
            view_mode: ViewMode::Table,
            groups: Vec::new(),
            similar_groups: Vec::new(),
            groups_generation: 0,
            exact_rows_cache: ExactRowsCache::default(),
            active_mode: ScanMode::ExactContent,
            selection: HashSet::new(),
            scan_state: ScanState::Idle,
            status_message: None,
            thumbnail_cache: ThumbnailCache::new(),
            min_size_text: String::new(),
            max_size_text: String::new(),
            size_unit: SizeUnit::MB,
            extension_mode: ExtensionMode::All,
            extension_text: String::new(),
            sort: None,
            select_criterion: SelectCriterion::Oldest,
            select_invert: false,
            delete_confirm: None,
            delete_state: DeleteState::Idle,

            tab: AppTab::Scan,
            reverse_search: ReverseSearchState::default(),

            taskbar: Taskbar::default(),
            play_sounds: true,
        }
    }
}

impl DupeApp {
    pub fn is_scanning(&self) -> bool {
        matches!(self.scan_state, ScanState::Running { .. })
    }

    pub fn start_scan(&mut self) {
        self.config.min_size = self.size_unit.parse_to_bytes(&self.min_size_text);
        self.config.max_size = self.size_unit.parse_to_bytes(&self.max_size_text);
        self.config.extensions = match self.extension_mode {
            ExtensionMode::All => ExtensionFilter::All,
            ExtensionMode::Include => {
                ExtensionFilter::Include(parse_extension_list(&self.extension_text))
            }
            ExtensionMode::Exclude => {
                ExtensionFilter::Exclude(parse_extension_list(&self.extension_text))
            }
        };

        self.groups.clear();
        self.similar_groups.clear();
        self.groups_generation += 1;
        self.active_mode = self.config.mode;
        self.selection.clear();
        self.status_message = None;

        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let config = self.config.clone();
        let cancel_for_thread = cancel.clone();
        std::thread::spawn(move || {
            scanner::run_scan(config, tx, cancel_for_thread);
        });
        self.scan_state = ScanState::Running {
            rx,
            cancel,
            scanned: 0,
            hash_progress: None,
        };
    }

    pub fn cancel_scan(&mut self) {
        if let ScanState::Running { cancel, .. } = &self.scan_state {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Drains queued scan events (bounded per frame so a huge tree can't stall
    /// the UI thread), returning whether anything changed.
    fn drain_scan_events(&mut self) -> bool {
        let mut changed = false;
        let mut groups_changed = false;
        let mut new_state = None;
        let mut cancelled = false;
        if let ScanState::Running {
            rx,
            cancel,
            scanned,
            hash_progress,
        } = &mut self.scan_state
        {
            for event in rx.try_iter().take(200) {
                changed = true;
                match event {
                    ScanEvent::Progress { scanned: s } => *scanned = s,
                    ScanEvent::HashPhaseStarted { total_bytes } => {
                        *hash_progress = Some(HashProgress {
                            total_bytes,
                            done_bytes: 0,
                            started_at: Instant::now(),
                        });
                    }
                    ScanEvent::HashProgress { bytes_done } => {
                        if let Some(progress) = hash_progress {
                            progress.done_bytes = bytes_done;
                        }
                    }
                    ScanEvent::GroupFound(group) => {
                        self.groups.push(group);
                        groups_changed = true;
                    }
                    ScanEvent::SimilarGroupFound(group) => self.similar_groups.push(group),
                    ScanEvent::Done { elapsed_ms } => {
                        cancelled = cancel.load(Ordering::Relaxed);
                        new_state = Some(ScanState::Done { elapsed_ms })
                    }
                    ScanEvent::Error(msg) => self.status_message = Some(msg),
                }
            }
        }
        if groups_changed {
            self.groups_generation += 1;
        }
        if let Some(state) = new_state {
            self.scan_state = state;
            if self.play_sounds && !cancelled {
                sound::play(Sound::ScanFinished);
            }
        }
        changed
    }

    /// Rebuilds `exact_rows_cache` if it's stale; a cheap no-op otherwise.
    /// Called once per frame, before anything that reads the cache.
    pub fn refresh_exact_rows_cache(&mut self) {
        self.exact_rows_cache.refresh(
            &self.groups,
            self.groups_generation,
            self.only_show_name_copies,
            self.only_show_duplicates,
            self.sort,
        );
    }

    pub fn select_ctrl_a(&mut self) {
        self.selection = match self.active_mode {
            ScanMode::ExactContent => {
                self.exact_rows_cache.rows.iter().map(|r| r.path.clone()).collect()
            }
            ScanMode::SimilarMedia => compute_visible_media_entries(
                &self.similar_groups,
                self.only_show_duplicates,
            )
            .into_iter()
            .map(|f| f.path.clone())
            .collect(),
        };
    }

    /// For each group, finds the file matching `criterion` and adds it to the
    /// selection — or, with `invert`, adds every *other* file in the group
    /// instead (e.g. "select everything except the oldest", to keep the
    /// oldest and delete the rest).
    pub fn select_by_criterion(&mut self, criterion: SelectCriterion, invert: bool) {
        match self.active_mode {
            ScanMode::ExactContent => {
                for group in &self.groups {
                    apply_criterion_selection(&group.files, criterion, invert, &mut self.selection);
                }
            }
            ScanMode::SimilarMedia => {
                for group in &self.similar_groups {
                    apply_criterion_selection(&group.files, criterion, invert, &mut self.selection);
                }
            }
        }
    }

    pub fn delete_selected(&mut self) {
        if self.selection.is_empty() || self.is_deleting() {
            return;
        }
        let paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
        let total_size: u64 = match self.active_mode {
            ScanMode::ExactContent => compute_visible_entries(&self.groups, false)
                .into_iter()
                .filter(|f| self.selection.contains(&f.path))
                .map(|f| f.size)
                .sum(),
            ScanMode::SimilarMedia => compute_visible_media_entries(&self.similar_groups, false)
                .into_iter()
                .filter(|f| self.selection.contains(&f.path))
                .map(|f| f.size)
                .sum(),
        };
        self.delete_confirm = Some(DeleteConfirmState {
            paths,
            total_size,
            permanent: false,
        });
    }

    pub fn is_deleting(&self) -> bool {
        matches!(self.delete_state, DeleteState::Running(_))
    }

    /// Moves the confirmed selection to the trash (or, if the dialog's
    /// permanent checkbox was ticked, removes it outright) on a background
    /// thread, re-verifying each file still exists first since it may have
    /// vanished or changed since the scan, and reporting progress so a large
    /// selection doesn't leave the UI looking frozen.
    pub fn confirm_delete(&mut self) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        let total = confirm.paths.len();
        let permanent = confirm.permanent;

        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for path in confirm.paths {
                let _ = tx.send(DeleteEvent::FileStarted { path: path.clone() });
                let deleted = path.exists()
                    && if permanent {
                        std::fs::remove_file(&path).is_ok()
                    } else {
                        trash::delete(&path).is_ok()
                    };
                let _ = tx.send(DeleteEvent::FileDone { path, deleted });
            }
            let _ = tx.send(DeleteEvent::Finished);
        });

        self.delete_state = DeleteState::Running(DeleteJob {
            rx,
            total,
            done: 0,
            deleted_paths: HashSet::new(),
            skipped: 0,
            permanent,
            started_at: Instant::now(),
            current: None,
        });
    }

    /// Drains queued delete events (mirrors `drain_scan_events`), pruning
    /// deleted files from the results and selection once the background
    /// thread reports it's finished.
    fn drain_delete_events(&mut self) -> bool {
        let mut changed = false;
        let mut finished = false;
        if let DeleteState::Running(job) = &mut self.delete_state {
            // Higher cap than the scan's: these events are trivially cheap to
            // apply, and a permanent delete can churn through thousands of
            // files a second.
            for event in job.rx.try_iter().take(2000) {
                changed = true;
                match event {
                    DeleteEvent::FileStarted { path } => job.current = Some(path),
                    DeleteEvent::FileDone { path, deleted } => {
                        job.done += 1;
                        if deleted {
                            job.deleted_paths.insert(path);
                        } else {
                            job.skipped += 1;
                        }
                    }
                    DeleteEvent::Finished => finished = true,
                }
            }
        }

        if finished
            && let DeleteState::Running(DeleteJob {
                deleted_paths,
                skipped,
                permanent,
                ..
            }) = std::mem::replace(&mut self.delete_state, DeleteState::Idle)
        {
            for group in &mut self.groups {
                group.files.retain(|f| !deleted_paths.contains(&f.path));
            }
            self.groups.retain(|g| g.files.len() > 1);
            self.groups_generation += 1;
            for group in &mut self.similar_groups {
                group.files.retain(|f| !deleted_paths.contains(&f.path));
            }
            self.similar_groups.retain(|g| g.files.len() > 1);
            self.selection.retain(|p| !deleted_paths.contains(p));

            if self.play_sounds {
                sound::play(Sound::DeleteFinished);
            }

            let action = if permanent {
                "Permanently deleted"
            } else {
                "Moved"
            };
            let destination = if permanent { "" } else { " to the trash" };
            self.status_message = Some(if skipped == 0 {
                format!("{action} {} file(s){destination}.", deleted_paths.len())
            } else {
                format!(
                    "{action} {} file(s){destination}; skipped {} (missing or failed).",
                    deleted_paths.len(),
                    skipped
                )
            });
        }

        changed
    }

    /// What the taskbar button should show for whatever background work is
    /// running. A delete takes precedence over a scan (it's the one the user
    /// is most likely waiting on), and a scan over reverse-search indexing.
    pub fn taskbar_progress(&self) -> TaskbarProgress {
        if let DeleteState::Running(job) = &self.delete_state {
            return if job.total == 0 {
                TaskbarProgress::Indeterminate
            } else {
                TaskbarProgress::Normal(job.done as f32 / job.total as f32)
            };
        }
        if let ScanState::Running { hash_progress, .. } = &self.scan_state {
            return match hash_progress {
                Some(p) if p.total_bytes > 0 => {
                    TaskbarProgress::Normal(p.done_bytes as f32 / p.total_bytes as f32)
                }
                _ => TaskbarProgress::Indeterminate,
            };
        }
        if let crate::reverse_search::IndexState::Running { scanned, total, .. } =
            &self.reverse_search.index_state
        {
            return if *total == 0 {
                TaskbarProgress::Indeterminate
            } else {
                TaskbarProgress::Normal(*scanned as f32 / *total as f32)
            };
        }
        TaskbarProgress::None
    }
}

impl eframe::App for DupeApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.is_scanning() {
            let changed = self.drain_scan_events();
            if changed {
                ctx.request_repaint();
            }
            if self.is_scanning() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        if self.is_deleting() {
            let changed = self.drain_delete_events();
            if changed {
                ctx.request_repaint();
            }
            if self.is_deleting() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        if self.active_mode == ScanMode::ExactContent {
            self.refresh_exact_rows_cache();
        }

        if self.reverse_search.is_indexing() {
            let changed = self.reverse_search.drain_index_events();
            if changed {
                ctx.request_repaint();
            }
            if self.reverse_search.is_indexing() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        self.taskbar.set(frame, self.taskbar_progress());

        crate::ui::tab_bar::show(self, ui);

        if self.tab == AppTab::Scan {
            let wants_keyboard = ctx.egui_wants_keyboard_input();
            let ctrl_a = !wants_keyboard
                && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A));
            if ctrl_a {
                self.select_ctrl_a();
            }
            let escape = !wants_keyboard && ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if escape {
                self.selection.clear();
            }
            let delete_key = !wants_keyboard && ctx.input(|i| i.key_pressed(egui::Key::Delete));
            if delete_key {
                self.delete_selected();
            }

            crate::ui::settings_panel::show(self, ui);
            crate::ui::status_bar::show(self, ui);
            crate::ui::delete_confirm::show(self, &ctx);

            egui::CentralPanel::default().show(ui, |ui| match self.active_mode {
                ScanMode::SimilarMedia => crate::ui::results_table_similar::show(self, ui),
                ScanMode::ExactContent => match self.view_mode {
                    ViewMode::Table => crate::ui::results_table::show(self, ui),
                    ViewMode::Grid => crate::ui::results_grid::show(self, ui),
                },
            });
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                crate::ui::reverse_search_panel::show(self, ui);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_remaining_time_from_the_rate_so_far() {
        let eta = estimate_remaining(25, 100, Duration::from_secs(10)).unwrap();
        assert_eq!(eta.as_secs(), 30);
    }

    #[test]
    fn no_estimate_before_progress_or_once_complete() {
        assert!(estimate_remaining(0, 100, Duration::from_secs(5)).is_none());
        assert!(estimate_remaining(100, 100, Duration::from_secs(5)).is_none());
        assert!(estimate_remaining(5, 100, Duration::ZERO).is_none());
    }
}
