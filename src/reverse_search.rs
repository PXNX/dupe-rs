use crate::config::ScanConfig;
use crate::control::JobControl;
use crate::index_db::{IndexDb, IndexedFile, hex_encode};
use crate::scanner::indexer::{self, IndexEvent};
use crossbeam_channel::Receiver;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub enum IndexState {
    Idle,
    Running {
        rx: Receiver<IndexEvent>,
        control: Arc<JobControl>,
        scanned: usize,
        total: usize,
    },
}

/// State for the "reverse search" tab: build a persisted, drive-spanning
/// index of file hashes ahead of time (see `scanner::indexer`), then pick a
/// single file and find every other indexed copy of it — including on drives
/// that aren't attached right now, since matches are looked up by volume
/// label + relative path rather than a live filesystem path.
pub struct ReverseSearchState {
    pub db: IndexDb,
    db_path: PathBuf,
    pub index_folders: Vec<PathBuf>,
    pub index_state: IndexState,
    pub picked_file: Option<PathBuf>,
    pub results: Vec<IndexedFile>,
    pub status: Option<String>,
}

impl Default for ReverseSearchState {
    fn default() -> Self {
        Self::new(IndexDb::default_path())
    }
}

impl ReverseSearchState {
    /// Loads (or starts) the index at `db_path`. Exposed separately from
    /// `default()` so tests can point it at a temp file instead of the
    /// real, shared `%LOCALAPPDATA%` index.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db: IndexDb::load(&db_path),
            db_path,
            index_folders: Vec::new(),
            index_state: IndexState::Idle,
            picked_file: None,
            results: Vec::new(),
            status: None,
        }
    }

    pub fn is_indexing(&self) -> bool {
        matches!(self.index_state, IndexState::Running { .. })
    }

    pub fn start_indexing(&mut self) {
        if self.index_folders.is_empty() || self.is_indexing() {
            return;
        }
        let config = ScanConfig {
            folders: self.index_folders.clone(),
            ..ScanConfig::default()
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || {
            indexer::run_index_scan(config, tx, control_for_thread);
        });
        self.status = None;
        self.index_state = IndexState::Running {
            rx,
            control,
            scanned: 0,
            total: 0,
        };
    }

    pub fn cancel_indexing(&mut self) {
        if let IndexState::Running { control, .. } = &self.index_state {
            control.cancel();
        }
    }

    pub fn is_index_paused(&self) -> bool {
        matches!(&self.index_state, IndexState::Running { control, .. } if control.is_paused())
    }

    pub fn toggle_index_pause(&mut self) {
        if let IndexState::Running { control, .. } = &self.index_state {
            control.set_paused(!control.is_paused());
        }
    }

    /// Drains queued indexing events, merging a finished pass into the
    /// persisted DB and saving it. Returns whether anything changed.
    pub fn drain_index_events(&mut self) -> bool {
        let mut changed = false;
        let mut finished = None;
        if let IndexState::Running {
            rx, scanned, total, ..
        } = &mut self.index_state
        {
            for event in rx.try_iter().take(200) {
                changed = true;
                match event {
                    IndexEvent::Progress { scanned: s, total: t } => {
                        *scanned = s;
                        *total = t;
                    }
                    IndexEvent::Done { entries, elapsed_ms } => {
                        finished = Some((entries, elapsed_ms));
                    }
                }
            }
        }

        if let Some((entries, elapsed_ms)) = finished {
            let file_count = entries.len();
            let mut by_volume: HashMap<(String, String), Vec<(String, IndexedFile)>> = HashMap::new();
            for (hash_hex, file) in entries {
                let key = (file.drive_letter.clone(), file.volume_label.clone());
                by_volume.entry(key).or_default().push((hash_hex, file));
            }
            let drive_count = by_volume.len();
            for ((drive, label), volume_entries) in by_volume {
                self.db.reindex_volume(&drive, &label, volume_entries);
            }
            self.status = Some(match self.db.save(&self.db_path) {
                Ok(()) => format!(
                    "Indexed {file_count} file(s) across {drive_count} drive(s) in {}.",
                    crate::ui::format::format_duration_hms(elapsed_ms)
                ),
                Err(err) => format!("Indexed {file_count} file(s) but failed to save the index: {err}"),
            });
            self.index_state = IndexState::Idle;
            self.refresh_results();
        }
        changed
    }

    /// Sets the file to find matches for and looks them up immediately.
    /// Hashing is done inline (blocking) rather than on a background thread:
    /// a single file's hash is fast enough for this not to be worth the extra
    /// state machine a background variant would need.
    pub fn pick_file(&mut self, path: PathBuf) {
        self.picked_file = Some(path);
        self.refresh_results();
    }

    fn refresh_results(&mut self) {
        let Some(path) = self.picked_file.clone() else {
            self.results.clear();
            return;
        };
        let hash = match crate::scanner::full_hash(&path) {
            Ok(h) => h,
            Err(err) => {
                self.status = Some(format!("Couldn't read {}: {err}", path.display()));
                self.results.clear();
                return;
            }
        };
        let drive_letter = crate::volume::drive_letter_of(&path);
        let volume_label = crate::volume::volume_info(&drive_letter).label;
        let root = format!("{drive_letter}\\");
        let rel_path = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
        let probe = IndexedFile {
            volume_label,
            drive_letter,
            rel_path,
            size: 0,
            modified: std::time::SystemTime::UNIX_EPOCH,
        };
        let hash_hex = hex_encode(&hash);
        self.results = self.db.matches(&hash_hex, &probe).into_iter().cloned().collect();
    }
}
