use crate::config::ScanConfig;
use crate::control::JobControl;
use crate::index_db::{IndexDb, IndexedFile, VolumeUsage, hex_encode};
use crate::scanner::indexer::{self, IndexEvent};
use crossbeam_channel::Receiver;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Results of the background work behind a reverse-search lookup. Both steps
/// touch the disk (hashing the picked file, probing the drives matches live
/// on), which can stall for seconds on a sleeping or slow drive, so neither
/// runs on the UI thread.
enum LookupEvent {
    Hashed { hash_hex: String, probe: IndexedFile },
    Failed(String),
    /// Per result: whether its drive is attached and the file is there.
    Attached(Vec<bool>),
}

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
    /// Parallel to `results`; `None` while still being checked.
    pub results_attached: Vec<Option<bool>>,
    pub status: Option<String>,
    lookup_rx: Option<Receiver<LookupEvent>>,
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
            results_attached: Vec::new(),
            status: None,
            lookup_rx: None,
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
            // Archives are worth finding again too; the skip only exists to
            // keep duplicate *deletion* from breaking split sets.
            skip_archives: false,
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
                    IndexEvent::Done {
                        entries,
                        elapsed_ms,
                        usage,
                    } => {
                        finished = Some((entries, elapsed_ms, usage));
                    }
                }
            }
        }

        if let Some((entries, elapsed_ms, usage)) = finished {
            for (drive, label, u) in usage {
                self.db.record_usage(&drive, &label, u);
            }
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

    /// Adds files hashed elsewhere (e.g. by a drive-fill copy) to the index,
    /// along with the usage of the drive they landed on (measured by that
    /// worker), and saves it.
    pub fn add_to_index(
        &mut self,
        entries: Vec<(String, IndexedFile)>,
        usage: Option<(String, String, VolumeUsage)>,
    ) -> std::io::Result<()> {
        if let Some((drive, label, u)) = usage {
            self.db.record_usage(&drive, &label, u);
        }
        if entries.is_empty() {
            return self.db.save(&self.db_path);
        }
        self.db.upsert(entries);
        let saved = self.db.save(&self.db_path);
        self.refresh_results();
        saved
    }

    /// Sets the file to find matches for and starts looking them up in the
    /// background (see `LookupEvent`).
    pub fn pick_file(&mut self, path: PathBuf) {
        self.picked_file = Some(path);
        self.refresh_results();
    }

    pub fn is_looking_up(&self) -> bool {
        self.lookup_rx.is_some()
    }

    /// Re-runs the lookup for the picked file, e.g. after the index changed.
    fn refresh_results(&mut self) {
        self.results.clear();
        self.results_attached.clear();
        let Some(path) = self.picked_file.clone() else {
            self.lookup_rx = None;
            return;
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let event = match crate::scanner::full_hash(&path) {
                Ok(hash) => {
                    let drive_letter = crate::volume::drive_letter_of(&path);
                    let volume_label = crate::volume::volume_info(&drive_letter).label;
                    let root = format!("{drive_letter}\\");
                    let rel_path = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
                    LookupEvent::Hashed {
                        hash_hex: hex_encode(&hash),
                        probe: IndexedFile {
                            volume_label,
                            drive_letter,
                            rel_path,
                            size: 0,
                            modified: std::time::SystemTime::UNIX_EPOCH,
                        },
                    }
                }
                Err(err) => LookupEvent::Failed(format!("Couldn't read {}: {err}", path.display())),
            };
            let _ = tx.send(event);
        });
        self.lookup_rx = Some(rx);
    }

    /// Applies finished lookup steps. Returns whether anything changed.
    pub fn drain_lookup(&mut self) -> bool {
        let Some(rx) = &self.lookup_rx else {
            return false;
        };
        let Ok(event) = rx.try_recv() else {
            return false;
        };
        match event {
            LookupEvent::Failed(msg) => {
                self.status = Some(msg);
                self.lookup_rx = None;
            }
            LookupEvent::Hashed { hash_hex, probe } => {
                self.results = self.db.matches(&hash_hex, &probe).into_iter().cloned().collect();
                self.results_attached = vec![None; self.results.len()];
                if self.results.is_empty() {
                    self.lookup_rx = None;
                } else {
                    let files = self.results.clone();
                    let (tx, rx) = crossbeam_channel::unbounded();
                    std::thread::spawn(move || {
                        let _ = tx.send(LookupEvent::Attached(check_attached(&files)));
                    });
                    self.lookup_rx = Some(rx);
                }
            }
            LookupEvent::Attached(attached) => {
                self.results_attached = attached.into_iter().map(Some).collect();
                self.lookup_rx = None;
            }
        }
        true
    }
}

/// Whether each file's volume is mounted under its recorded letter and the
/// file is there. Not just `path.exists()`: a different physical drive can
/// end up mounted under the same letter (e.g. swapping what's plugged into
/// D:), and its files could coincidentally share a relative path with
/// something indexed from the original volume. Volume labels are looked up
/// once per letter.
fn check_attached(files: &[IndexedFile]) -> Vec<bool> {
    let mut labels: HashMap<String, String> = HashMap::new();
    files
        .iter()
        .map(|f| {
            let label = labels
                .entry(f.drive_letter.clone())
                .or_insert_with(|| crate::volume::volume_info(&f.drive_letter).label);
            *label == f.volume_label && f.absolute_path().exists()
        })
        .collect()
}
