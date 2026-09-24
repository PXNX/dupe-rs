use crate::config::{ExtensionFilter, ScanConfig, ScanMode, SizeUnit, parse_extension_list};
use crate::model::{DeleteEvent, DupeGroup, FileEntry, MediaEntry, ScanEvent, SimilarGroup};
use crate::scanner;
use crate::selection::{compute_visible_entries, compute_visible_media_entries};
use crate::ui::thumbnails::ThumbnailCache;
use crossbeam_channel::Receiver;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

/// Byte-level progress of the full-file hashing pass, the slow, I/O-bound
/// part of a scan on large trees. Used to show a GB-scanned readout and ETA.
pub struct HashProgress {
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub started_at: Instant,
}

impl HashProgress {
    pub fn eta(&self) -> Option<std::time::Duration> {
        if self.done_bytes == 0 || self.done_bytes >= self.total_bytes {
            return None;
        }
        let elapsed = self.started_at.elapsed().as_secs_f64();
        let rate = self.done_bytes as f64 / elapsed;
        if rate <= 0.0 {
            return None;
        }
        let remaining = (self.total_bytes - self.done_bytes) as f64;
        Some(std::time::Duration::from_secs_f64(remaining / rate))
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

pub enum DeleteState {
    Idle,
    Running {
        rx: Receiver<DeleteEvent>,
        total: usize,
        done: usize,
        deleted_paths: HashSet<PathBuf>,
        skipped: usize,
    },
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
        let mut new_state = None;
        if let ScanState::Running {
            rx,
            scanned,
            hash_progress,
            ..
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
                    ScanEvent::GroupFound(group) => self.groups.push(group),
                    ScanEvent::SimilarGroupFound(group) => self.similar_groups.push(group),
                    ScanEvent::Done { elapsed_ms } => {
                        new_state = Some(ScanState::Done { elapsed_ms })
                    }
                    ScanEvent::Error(msg) => self.status_message = Some(msg),
                }
            }
        }
        if let Some(state) = new_state {
            self.scan_state = state;
        }
        changed
    }

    pub fn select_ctrl_a(&mut self) {
        self.selection = match self.active_mode {
            ScanMode::ExactContent => {
                let filtered =
                    crate::selection::filter_by_name_pattern(&self.groups, self.only_show_name_copies);
                compute_visible_entries(filtered, self.only_show_duplicates)
                    .into_iter()
                    .map(|f| f.path.clone())
                    .collect()
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
        self.delete_confirm = Some(DeleteConfirmState { paths, total_size });
    }

    pub fn is_deleting(&self) -> bool {
        matches!(self.delete_state, DeleteState::Running { .. })
    }

    /// Moves the confirmed selection to the trash on a background thread
    /// (re-verifying each file still exists first, since it may have vanished
    /// or changed since the scan), reporting progress so a large selection
    /// doesn't leave the UI looking frozen.
    pub fn confirm_delete(&mut self) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };
        let total = confirm.paths.len();

        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for path in confirm.paths {
                let deleted = path.exists() && trash::delete(&path).is_ok();
                let _ = tx.send(DeleteEvent::FileDone { path, deleted });
            }
            let _ = tx.send(DeleteEvent::Finished);
        });

        self.delete_state = DeleteState::Running {
            rx,
            total,
            done: 0,
            deleted_paths: HashSet::new(),
            skipped: 0,
        };
    }

    /// Drains queued delete events (mirrors `drain_scan_events`), pruning
    /// deleted files from the results and selection once the background
    /// thread reports it's finished.
    fn drain_delete_events(&mut self) -> bool {
        let mut changed = false;
        let mut finished = false;
        if let DeleteState::Running {
            rx,
            done,
            deleted_paths,
            skipped,
            ..
        } = &mut self.delete_state
        {
            for event in rx.try_iter().take(200) {
                changed = true;
                match event {
                    DeleteEvent::FileDone { path, deleted } => {
                        *done += 1;
                        if deleted {
                            deleted_paths.insert(path);
                        } else {
                            *skipped += 1;
                        }
                    }
                    DeleteEvent::Finished => finished = true,
                }
            }
        }

        if finished
            && let DeleteState::Running {
                deleted_paths,
                skipped,
                ..
            } = std::mem::replace(&mut self.delete_state, DeleteState::Idle)
        {
            for group in &mut self.groups {
                group.files.retain(|f| !deleted_paths.contains(&f.path));
            }
            self.groups.retain(|g| g.files.len() > 1);
            for group in &mut self.similar_groups {
                group.files.retain(|f| !deleted_paths.contains(&f.path));
            }
            self.similar_groups.retain(|g| g.files.len() > 1);
            self.selection.retain(|p| !deleted_paths.contains(p));

            self.status_message = Some(if skipped == 0 {
                format!("Moved {} file(s) to the trash.", deleted_paths.len())
            } else {
                format!(
                    "Moved {} file(s) to the trash; skipped {} (missing or failed).",
                    deleted_paths.len(),
                    skipped
                )
            });
        }

        changed
    }
}

impl eframe::App for DupeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if self.is_scanning() {
            let changed = self.drain_scan_events();
            if changed {
                ctx.request_repaint();
            }
            if self.is_scanning() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        if self.is_deleting() {
            let changed = self.drain_delete_events();
            if changed {
                ctx.request_repaint();
            }
            if self.is_deleting() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        let wants_keyboard = ctx.egui_wants_keyboard_input();
        let ctrl_a =
            !wants_keyboard && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A));
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
    }
}
