use crate::config::{ExtensionFilter, ScanConfig, SizeUnit, parse_extension_list};
use crate::model::{DupeGroup, ScanEvent};
use crate::scanner;
use crate::selection::compute_visible_entries;
use crate::ui::thumbnails::ThumbnailCache;
use crossbeam_channel::Receiver;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

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

pub struct DeleteConfirmState {
    pub paths: Vec<PathBuf>,
    pub total_size: u64,
}

pub struct DupeApp {
    pub config: ScanConfig,
    pub only_show_duplicates: bool,
    pub view_mode: ViewMode,
    pub groups: Vec<DupeGroup>,
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

    pub delete_confirm: Option<DeleteConfirmState>,
}

impl Default for DupeApp {
    fn default() -> Self {
        Self {
            config: ScanConfig::default(),
            only_show_duplicates: false,
            view_mode: ViewMode::Table,
            groups: Vec::new(),
            selection: HashSet::new(),
            scan_state: ScanState::Idle,
            status_message: None,
            thumbnail_cache: ThumbnailCache::new(),
            min_size_text: String::new(),
            max_size_text: String::new(),
            size_unit: SizeUnit::MB,
            extension_mode: ExtensionMode::All,
            extension_text: String::new(),
            delete_confirm: None,
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
        let visible = compute_visible_entries(&self.groups, self.only_show_duplicates);
        self.selection = visible.into_iter().map(|f| f.path.clone()).collect();
    }

    pub fn delete_selected(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
        let total_size: u64 = compute_visible_entries(&self.groups, false)
            .into_iter()
            .filter(|f| self.selection.contains(&f.path))
            .map(|f| f.size)
            .sum();
        self.delete_confirm = Some(DeleteConfirmState { paths, total_size });
    }

    pub fn confirm_delete(&mut self) {
        let Some(confirm) = self.delete_confirm.take() else {
            return;
        };

        // Re-verify existence right before deleting: a file hashed successfully
        // during the scan may have vanished or been modified since.
        let mut deleted = HashSet::new();
        let mut skipped = 0usize;
        for path in &confirm.paths {
            if !path.exists() {
                skipped += 1;
                continue;
            }
            match trash::delete(path) {
                Ok(()) => {
                    deleted.insert(path.clone());
                }
                Err(_) => skipped += 1,
            }
        }

        for group in &mut self.groups {
            group.files.retain(|f| !deleted.contains(&f.path));
        }
        self.groups.retain(|g| g.files.len() > 1);
        self.selection.retain(|p| !deleted.contains(p));

        self.status_message = Some(if skipped == 0 {
            format!("Moved {} file(s) to the trash.", deleted.len())
        } else {
            format!(
                "Moved {} file(s) to the trash; skipped {} (missing or failed).",
                deleted.len(),
                skipped
            )
        });
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

        crate::ui::settings_panel::show(self, ui);
        crate::ui::status_bar::show(self, ui);
        crate::ui::delete_confirm::show(self, &ctx);

        egui::CentralPanel::default().show(ui, |ui| match self.view_mode {
            ViewMode::Table => crate::ui::results_table::show(self, ui),
            ViewMode::Grid => crate::ui::results_grid::show(self, ui),
        });
    }
}
