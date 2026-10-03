use crate::config::ScanConfig;
use crate::control::JobControl;
use crate::drives::DriveUpdate;
use crate::index_db::{IndexDb, IndexedFile, VolumeUsage, hex_encode};
use crate::scanner::indexer::{self, IndexEvent};
use crate::volume::VolumeInfo;
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
    /// Per result: where it is right now, if its drive is attached.
    Attached(Vec<Presence>),
}

/// Whether a search result can be opened right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Presence {
    Checking,
    /// Its drive isn't attached, or the file isn't there anymore.
    Missing,
    /// Its drive is attached (possibly under a different letter than it was
    /// indexed under) and the file is at this path.
    At(PathBuf),
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
/// (label + serial) + relative path rather than a live filesystem path.
pub struct ReverseSearchState {
    pub db: IndexDb,
    db_path: PathBuf,
    pub index_folders: Vec<PathBuf>,
    pub index_state: IndexState,
    pub picked_file: Option<PathBuf>,
    pub results: Vec<IndexedFile>,
    /// Parallel to `results`.
    pub results_presence: Vec<Presence>,
    pub status: Option<String>,
    lookup_rx: Option<Receiver<LookupEvent>>,
    /// What indexing and drive-fill learned about drives, for the drive
    /// registry to pick up (see `drives::DrivesState::poll`).
    drive_updates: Vec<DriveUpdate>,
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
            results_presence: Vec::new(),
            status: None,
            lookup_rx: None,
            drive_updates: Vec::new(),
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
            let file_count = entries.len();
            let mut by_volume: HashMap<String, Vec<(String, IndexedFile)>> = HashMap::new();
            for (hash_hex, file) in entries {
                by_volume.entry(file.volume_key()).or_default().push((hash_hex, file));
            }
            let drive_count = by_volume.len();
            for (volume, u) in usage {
                let volume_entries = by_volume.remove(&volume.key()).unwrap_or_default();
                self.db.reindex_volume(&volume, volume_entries);
                self.drive_updates.push(DriveUpdate {
                    volume,
                    usage: Some(u),
                    indexed: true,
                });
            }
            // Volumes whose usage couldn't be measured.
            for volume_entries in by_volume.into_values() {
                let f = &volume_entries[0].1;
                let volume = VolumeInfo {
                    drive_letter: f.drive_letter.clone(),
                    label: f.volume_label.clone(),
                    serial: f.volume_serial,
                };
                self.db.reindex_volume(&volume, volume_entries);
                self.drive_updates.push(DriveUpdate {
                    volume,
                    usage: None,
                    indexed: true,
                });
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
        usage: Option<(VolumeInfo, VolumeUsage)>,
    ) -> std::io::Result<()> {
        if let Some((volume, u)) = usage {
            self.drive_updates.push(DriveUpdate {
                volume,
                usage: Some(u),
                indexed: false,
            });
        }
        if entries.is_empty() {
            return self.db.save(&self.db_path);
        }
        self.db.upsert(entries);
        let saved = self.db.save(&self.db_path);
        self.refresh_results();
        saved
    }

    /// Drive updates collected since the last call.
    pub fn take_drive_updates(&mut self) -> Vec<DriveUpdate> {
        std::mem::take(&mut self.drive_updates)
    }

    /// Gives entries indexed before serials were recorded the identity of
    /// `volume` (see `IndexDb::adopt_legacy`), saving if any changed.
    pub fn adopt_legacy(&mut self, volume: &VolumeInfo) {
        if self.db.adopt_legacy(volume)
            && let Err(err) = self.db.save(&self.db_path)
        {
            self.status = Some(format!("Couldn't save the index: {err}"));
        }
    }

    /// Removes every entry of the volume stored under `key` and saves.
    pub fn forget_volume(&mut self, key: &str) -> std::io::Result<()> {
        self.db.forget_volume(key);
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
        self.results_presence.clear();
        let Some(path) = self.picked_file.clone() else {
            self.lookup_rx = None;
            return;
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            let event = match crate::scanner::full_hash(&path) {
                Ok(hash) => {
                    let drive_letter = crate::volume::drive_letter_of(&path);
                    let volume = crate::volume::volume_info(&drive_letter);
                    let root = format!("{drive_letter}\\");
                    let rel_path = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
                    LookupEvent::Hashed {
                        hash_hex: hex_encode(&hash),
                        probe: IndexedFile {
                            volume_label: volume.label,
                            drive_letter,
                            volume_serial: volume.serial,
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
                self.results_presence = vec![Presence::Checking; self.results.len()];
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
            LookupEvent::Attached(presence) => {
                self.results_presence = presence;
                self.lookup_rx = None;
            }
        }
        true
    }
}

/// Where each file is right now. Not just `path.exists()` on the recorded
/// path: USB drives come back under whatever letter is free, and a different
/// drive can end up under the recorded letter whose files could
/// coincidentally share a relative path with something indexed from the
/// original volume. So each file's volume is looked for among the attached
/// ones by identity (label + serial), looked up once per letter.
fn check_attached(files: &[IndexedFile]) -> Vec<Presence> {
    let mounted: HashMap<String, String> = crate::volume::local_drive_letters()
        .iter()
        .filter_map(|letter| crate::volume::try_volume_info(letter))
        .flat_map(|v| {
            // Entries indexed before serials were recorded are keyed by
            // letter + label, so offer that form too.
            let legacy = crate::volume::volume_key(&v.drive_letter, &v.label, None);
            [(v.key(), v.drive_letter.clone()), (legacy, v.drive_letter)]
        })
        .collect();
    files
        .iter()
        .map(|f| match mounted.get(&f.volume_key()) {
            Some(letter) => {
                let path = f.absolute_path_on(letter);
                if path.exists() {
                    Presence::At(path)
                } else {
                    Presence::Missing
                }
            }
            None => Presence::Missing,
        })
        .collect()
}
