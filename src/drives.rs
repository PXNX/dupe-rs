//! The Drives tab: every drive dupe-rs knows about, whether it's plugged in
//! right now or sitting in a drawer — when it was last seen, indexed and
//! health-checked, how full it is, how it compares to its twin (the drive
//! holding the same data), and its SMART health history.
//!
//! The registry is kept in its own small file next to the reverse-search
//! index rather than inside it, since it changes every time a drive is
//! plugged in and the index can be hundreds of megabytes.

use crate::index_db::{ContentDiff, IndexDb, VolumeUsage};
use crate::reverse_search::ReverseSearchState;
use crate::smart::{self, Assessment, DeviceIdentity, DriveReading, SmartAttribute};
use crate::volume::{self, VolumeInfo};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Health checks kept per drive; the oldest are dropped beyond this.
const MAX_HEALTH_SAMPLES: usize = 200;
/// How often the watcher looks for drives being plugged in or removed.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Age after which something is flagged as overdue on a drive's card.
pub const STALE_INDEX: Duration = Duration::from_secs(180 * 86_400);
pub const STALE_HEALTH: Duration = Duration::from_secs(90 * 86_400);
pub const STALE_SEEN: Duration = Duration::from_secs(180 * 86_400);

/// One SMART reading of a drive.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HealthSample {
    pub at: SystemTime,
    pub attributes: Vec<SmartAttribute>,
}

/// Everything remembered about one volume, keyed in the registry by
/// `volume::volume_key`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DriveRecord {
    pub label: String,
    pub serial: Option<u32>,
    /// The letter it was mounted as most recently.
    pub last_letter: String,
    /// The physical disk, as last reported (see `smart::DeviceIdentity`).
    #[serde(default)]
    pub device: Option<DeviceIdentity>,
    #[serde(default)]
    pub first_seen: Option<SystemTime>,
    /// When it was last plugged in or (if it was attached while dupe-rs ran)
    /// removed.
    #[serde(default)]
    pub last_seen: Option<SystemTime>,
    #[serde(default)]
    pub last_indexed: Option<SystemTime>,
    #[serde(default)]
    pub usage: Option<VolumeUsage>,
    /// Key of the drive meant to hold the same data.
    #[serde(default)]
    pub twin: Option<String>,
    /// Oldest first.
    #[serde(default)]
    pub health: Vec<HealthSample>,
}

impl DriveRecord {
    fn new(volume: &VolumeInfo) -> Self {
        Self {
            label: volume.label.clone(),
            serial: volume.serial,
            last_letter: volume.drive_letter.clone(),
            ..Default::default()
        }
    }

    /// The latest health check's verdict, compared with the one before it.
    pub fn assessment(&self) -> Option<Assessment> {
        let (latest, earlier) = self.health.split_last()?;
        Some(smart::assess(
            &latest.attributes,
            earlier.last().map(|s| s.attributes.as_slice()),
        ))
    }

    /// Fills in whatever `other` (an older record of the same drive) knows
    /// that this one doesn't.
    fn absorb(&mut self, other: DriveRecord) {
        self.device = self.device.take().or(other.device);
        self.first_seen = min_time(self.first_seen, other.first_seen);
        self.last_seen = self.last_seen.max(other.last_seen);
        self.last_indexed = self.last_indexed.max(other.last_indexed);
        if other.usage.map(|u| u.recorded_at) > self.usage.map(|u| u.recorded_at) {
            self.usage = other.usage;
        }
        self.twin = self.twin.take().or(other.twin);
        self.health.extend(other.health);
        self.health.sort_by_key(|s| s.at);
    }
}

fn min_time(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<SystemTime> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The persisted set of known drives.
#[derive(Default, Serialize, Deserialize)]
pub struct DriveRegistry {
    drives: BTreeMap<String, DriveRecord>,
}

impl DriveRegistry {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, bytes)
    }

    pub fn default_path() -> PathBuf {
        crate::index_db::data_dir().join("drives.json")
    }

    pub fn get(&self, key: &str) -> Option<&DriveRecord> {
        self.drives.get(key)
    }

    /// `(key, record)` pairs sorted by label.
    pub fn sorted(&self) -> Vec<(&String, &DriveRecord)> {
        let mut out: Vec<_> = self.drives.iter().collect();
        out.sort_by(|a, b| a.1.label.cmp(&b.1.label).then_with(|| a.0.cmp(b.0)));
        out
    }

    pub fn len(&self) -> usize {
        self.drives.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drives.is_empty()
    }

    /// The record for `volume`, created if it's new, with its label and
    /// letter brought up to date.
    pub fn track(&mut self, volume: &VolumeInfo) -> &mut DriveRecord {
        self.adopt_legacy(volume);
        let record = self
            .drives
            .entry(volume.key())
            .or_insert_with(|| DriveRecord::new(volume));
        record.label = volume.label.clone();
        record.serial = volume.serial;
        if !volume.drive_letter.is_empty() {
            record.last_letter = volume.drive_letter.clone();
        }
        record
    }

    /// Moves a record kept under the letter + label key used before serials
    /// were recorded to `volume`'s proper key, now that `volume` is mounted
    /// under that letter with that label (see `IndexDb::adopt_legacy`).
    pub fn adopt_legacy(&mut self, volume: &VolumeInfo) -> bool {
        if volume.serial.is_none() {
            return false;
        }
        let legacy_key = volume::volume_key(&volume.drive_letter, &volume.label, None);
        let Some(legacy) = self.drives.remove(&legacy_key) else {
            return false;
        };
        let key = volume.key();
        match self.drives.get_mut(&key) {
            Some(record) => record.absorb(legacy),
            None => {
                let mut record = legacy;
                record.serial = volume.serial;
                self.drives.insert(key.clone(), record);
            }
        }
        for record in self.drives.values_mut() {
            if record.twin.as_deref() == Some(legacy_key.as_str()) {
                record.twin = Some(key.clone());
            }
        }
        true
    }

    /// Notes that `volume` (already tracked or not) is attached right now.
    /// Returns whether a tracked drive's record changed.
    pub fn record_seen(
        &mut self,
        volume: &VolumeInfo,
        device: Option<&DeviceIdentity>,
        now: SystemTime,
    ) -> bool {
        let adopted = self.adopt_legacy(volume);
        if !self.drives.contains_key(&volume.key()) {
            return adopted;
        }
        let record = self.track(volume);
        record.first_seen.get_or_insert(now);
        record.last_seen = Some(now);
        // An IDENTIFY result from a health check names the disk itself; the
        // driver's descriptor may name the USB adapter, so it only fills in.
        if record.device.is_none() {
            record.device = device.cloned();
        }
        true
    }

    /// Notes that the tracked drive under `key` was just removed.
    pub fn record_removed(&mut self, key: &str, now: SystemTime) -> bool {
        match self.drives.get_mut(key) {
            Some(record) => {
                record.last_seen = Some(now);
                true
            }
            None => false,
        }
    }

    pub fn record_update(&mut self, update: &DriveUpdate) {
        let record = self.track(&update.volume);
        let at = update
            .usage
            .map(|u| u.recorded_at)
            .unwrap_or_else(SystemTime::now);
        record.first_seen.get_or_insert(at);
        record.last_seen = record.last_seen.max(Some(at));
        if let Some(usage) = update.usage {
            record.usage = Some(usage);
        }
        if update.indexed {
            record.last_indexed = Some(at);
        }
    }

    pub fn record_health(
        &mut self,
        volume: &VolumeInfo,
        report: smart::SmartReport,
        at: SystemTime,
    ) {
        let record = self.track(volume);
        if report.identity.is_some() {
            record.device = report.identity;
        }
        record.health.push(HealthSample {
            at,
            attributes: report.attributes,
        });
        let excess = record.health.len().saturating_sub(MAX_HEALTH_SAMPLES);
        record.health.drain(..excess);
    }

    /// Pairs `a` with `b` (or unpairs it if `b` is `None`), unpairing
    /// whatever either was paired with before.
    pub fn set_twin(&mut self, a: &str, b: Option<&str>) {
        let mut unpair = |key: &str| {
            let old = self.drives.get_mut(key).and_then(|r| r.twin.take());
            if let Some(old) = old
                && let Some(r) = self.drives.get_mut(&old)
            {
                r.twin = None;
            }
        };
        unpair(a);
        if let Some(b) = b {
            unpair(b);
            if a == b || !self.drives.contains_key(a) || !self.drives.contains_key(b) {
                return;
            }
            self.drives.get_mut(a).unwrap().twin = Some(b.to_string());
            self.drives.get_mut(b).unwrap().twin = Some(a.to_string());
        }
    }

    pub fn forget(&mut self, key: &str) {
        self.set_twin(key, None);
        self.drives.remove(key);
    }

    /// Makes sure every volume in the index has a record, carrying over the
    /// usage older versions kept in the index (`legacy_usage`, keyed the
    /// same way). Returns whether anything was added.
    pub fn sync_with_index(
        &mut self,
        db: &IndexDb,
        legacy_usage: &HashMap<String, VolumeUsage>,
    ) -> bool {
        let mut added = false;
        for v in db.volumes() {
            if self.drives.contains_key(&v.key) {
                continue;
            }
            let mut record = DriveRecord::new(&VolumeInfo {
                drive_letter: v.drive_letter.clone(),
                label: v.label.clone(),
                serial: v.serial,
            });
            record.usage = legacy_usage.get(&v.key).copied();
            self.drives.insert(v.key.clone(), record);
            added = true;
        }
        added
    }
}

/// What indexing or a drive-fill copy learned about a drive, handed to the
/// registry.
#[derive(Clone, Debug, PartialEq)]
pub struct DriveUpdate {
    pub volume: VolumeInfo,
    pub usage: Option<VolumeUsage>,
    /// Whether the whole drive was (re-)indexed, not just added to.
    pub indexed: bool,
}

/// A drive attached right now.
#[derive(Clone, Debug, PartialEq)]
pub struct MountedDrive {
    pub volume: VolumeInfo,
    pub device: Option<DeviceIdentity>,
}

/// Lists the attached local drives (the OS calls behind this are answered
/// from the driver, so a sleeping disk isn't spun up).
fn mounted_drives() -> Vec<MountedDrive> {
    volume::local_drive_letters()
        .iter()
        .filter_map(|letter| {
            Some(MountedDrive {
                volume: volume::try_volume_info(letter)?,
                device: smart::device_identity(letter),
            })
        })
        .collect()
}

/// Background thread that reports the attached drives whenever the set of
/// drive letters changes (or a refresh is asked for), and asks the UI to
/// repaint. Ends once the app drops its end of the channel.
fn watch_drives(tx: Sender<Vec<MountedDrive>>, refresh: Receiver<()>, ctx: egui::Context) {
    let mut last_letters = None;
    loop {
        let letters = volume::local_drive_letters();
        let forced = refresh.try_iter().count() > 0;
        if forced || last_letters.as_ref() != Some(&letters) {
            if tx.send(mounted_drives()).is_err() {
                return;
            }
            ctx.request_repaint();
            last_letters = Some(letters);
        }
        match refresh.recv_timeout(POLL_INTERVAL) {
            Ok(()) => last_letters = None,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
    }
}

struct Watcher {
    rx: Receiver<Vec<MountedDrive>>,
    refresh: Sender<()>,
}

/// A health check in flight: the volumes asked about (by letter) and where
/// the result arrives.
struct HealthJob {
    volumes: HashMap<String, VolumeInfo>,
    rx: Receiver<Result<Vec<DriveReading>, String>>,
}

pub struct DrivesState {
    pub registry: DriveRegistry,
    path: PathBuf,
    /// Attached drives, as last reported by the watcher (`None` until its
    /// first report).
    pub mounted: Option<Vec<MountedDrive>>,
    watcher: Option<Watcher>,
    health: Option<HealthJob>,
    /// Per drive key: why its last health check failed.
    pub health_errors: HashMap<String, String>,
    pub status: Option<String>,
    /// Key of the drive the "forget?" dialog is asking about.
    pub pending_forget: Option<String>,
    /// Twin comparisons, keyed by pair and valid for one index generation.
    diffs: HashMap<(String, String), (u64, ContentDiff)>,
}

impl DrivesState {
    /// Loads the registry at `path`, adding any drive the index knows that
    /// it doesn't (along with usage older versions kept in the index).
    pub fn new(path: PathBuf, db: &mut IndexDb) -> Self {
        let mut registry = DriveRegistry::load(&path);
        let legacy_usage = db.take_legacy_usage();
        if registry.sync_with_index(db, &legacy_usage) {
            let _ = registry.save(&path);
        }
        Self {
            registry,
            path,
            mounted: None,
            watcher: None,
            health: None,
            health_errors: HashMap::new(),
            status: None,
            pending_forget: None,
            diffs: HashMap::new(),
        }
    }

    /// Starts the background drive watcher, once.
    pub fn ensure_watching(&mut self, ctx: &egui::Context) {
        if self.watcher.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let (refresh, refresh_rx) = crossbeam_channel::unbounded();
        let ctx = ctx.clone();
        std::thread::spawn(move || watch_drives(tx, refresh_rx, ctx));
        self.watcher = Some(Watcher { rx, refresh });
    }

    /// Re-reads labels etc. of the attached drives, e.g. after one was
    /// renamed.
    pub fn refresh(&self) {
        if let Some(w) = &self.watcher {
            let _ = w.refresh.send(());
        }
    }

    pub fn save(&mut self) {
        if let Err(err) = self.registry.save(&self.path) {
            self.status = Some(format!("Couldn't save the drive list: {err}"));
        }
    }

    /// Where the drive under `key` is attached right now, if it is.
    pub fn mounted_letter(&self, key: &str) -> Option<&str> {
        self.mounted
            .iter()
            .flatten()
            .find(|m| m.volume.key() == key)
            .map(|m| m.volume.drive_letter.as_str())
    }

    /// Applies what the watcher, a health check, indexing and drive-fill
    /// reported since the last frame. Returns whether anything changed.
    pub fn poll(&mut self, reverse_search: &mut ReverseSearchState) -> bool {
        let mut changed = false;
        let mut dirty = false;

        for update in reverse_search.take_drive_updates() {
            self.registry.record_update(&update);
            dirty = true;
        }
        if self
            .registry
            .sync_with_index(&reverse_search.db, &HashMap::new())
        {
            dirty = true;
        }

        let latest = self.watcher.as_ref().and_then(|w| w.rx.try_iter().last());
        if let Some(now_mounted) = latest {
            let now = SystemTime::now();
            for m in &now_mounted {
                reverse_search.adopt_legacy(&m.volume);
                dirty |= self.registry.record_seen(&m.volume, m.device.as_ref(), now);
            }
            for gone in self.mounted.iter().flatten() {
                let key = gone.volume.key();
                if !now_mounted.iter().any(|m| m.volume.key() == key) {
                    dirty |= self.registry.record_removed(&key, now);
                }
            }
            self.mounted = Some(now_mounted);
            changed = true;
        }

        if let Some(job) = &self.health
            && let Ok(result) = job.rx.try_recv()
        {
            let job = self.health.take().unwrap();
            self.apply_health(job, result);
            dirty = true;
        }

        if dirty {
            self.save();
        }
        changed || dirty
    }

    fn apply_health(&mut self, job: HealthJob, result: Result<Vec<DriveReading>, String>) {
        let readings = match result {
            Ok(readings) => readings,
            Err(err) => {
                self.status = Some(err);
                return;
            }
        };
        let now = SystemTime::now();
        let (mut ok, mut failed) = (0, 0);
        for reading in readings {
            let Some(volume) = job.volumes.get(&reading.letter) else {
                continue;
            };
            let key = volume.key();
            match reading.result {
                Ok(report) => {
                    self.registry.record_health(volume, report, now);
                    self.health_errors.remove(&key);
                    ok += 1;
                }
                Err(err) => {
                    self.health_errors.insert(key, err);
                    failed += 1;
                }
            }
        }
        self.status = Some(match (ok, failed) {
            (_, 0) => format!("Checked the health of {ok} drive(s)."),
            (0, _) => format!("Couldn't read the health of {failed} drive(s); see their cards."),
            _ => format!("Checked {ok} drive(s); {failed} couldn't be read (see their cards)."),
        });
    }

    pub fn is_checking_health(&self) -> bool {
        self.health.is_some()
    }

    /// Whether a running health check includes the drive under `key`.
    pub fn is_checking(&self, key: &str) -> bool {
        self.health
            .as_ref()
            .is_some_and(|j| j.volumes.values().any(|v| v.key() == key))
    }

    /// Reads SMART data of the given attached drives in the background
    /// (prompting for administrator rights; see `smart`).
    pub fn check_health(&mut self, volumes: Vec<VolumeInfo>) {
        if self.health.is_some() || volumes.is_empty() {
            return;
        }
        let letters: Vec<String> = volumes.iter().map(|v| v.drive_letter.clone()).collect();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(smart::read_health(&letters));
        });
        self.status = None;
        self.health = Some(HealthJob {
            volumes: volumes
                .into_iter()
                .map(|v| (v.drive_letter.clone(), v))
                .collect(),
            rx,
        });
    }

    /// Every attached drive that's tracked.
    pub fn attached_tracked(&self) -> Vec<VolumeInfo> {
        self.mounted
            .iter()
            .flatten()
            .filter(|m| self.registry.get(&m.volume.key()).is_some())
            .map(|m| m.volume.clone())
            .collect()
    }

    /// Attached drives that aren't tracked yet.
    pub fn untracked(&self) -> Vec<&MountedDrive> {
        self.mounted
            .iter()
            .flatten()
            .filter(|m| self.registry.get(&m.volume.key()).is_none())
            .collect()
    }

    pub fn track(&mut self, drive: &MountedDrive) {
        let now = SystemTime::now();
        let record = self.registry.track(&drive.volume);
        record.first_seen.get_or_insert(now);
        record.last_seen = Some(now);
        record.device = record.device.take().or(drive.device.clone());
        if let Some(usage) = crate::scanner::indexer::volume_usage(&drive.volume.drive_letter) {
            record.usage = Some(usage);
        }
        self.save();
    }

    pub fn set_twin(&mut self, a: &str, b: Option<&str>) {
        self.registry.set_twin(a, b);
        self.save();
    }

    /// Drops the drive from the list and its files from the index.
    pub fn forget(&mut self, key: &str, reverse_search: &mut ReverseSearchState) {
        self.registry.forget(key);
        self.health_errors.remove(key);
        self.save();
        if let Err(err) = reverse_search.forget_volume(key) {
            self.status = Some(format!("Couldn't save the index: {err}"));
        }
    }

    /// How the drive under `key` and its twin differ, by indexed content.
    pub fn twin_diff(&mut self, key: &str, twin: &str, db: &IndexDb) -> ContentDiff {
        let pair = (key.to_string(), twin.to_string());
        match self.diffs.get(&pair) {
            Some((generation, diff)) if *generation == db.generation() => *diff,
            _ => {
                let diff = db.content_diff(key, twin);
                self.diffs.insert(pair, (db.generation(), diff));
                diff
            }
        }
    }
}

/// Why a drive needs a look, most important first: its health, how out of
/// date its index and health check are, and how long it's been unplugged.
/// `attached` drives aren't nagged about being unplugged.
pub fn attention(
    record: &DriveRecord,
    indexed: bool,
    attached: bool,
    now: SystemTime,
) -> Vec<String> {
    let age = |t: Option<SystemTime>| t.and_then(|t| now.duration_since(t).ok());
    let days = |d: Duration| d.as_secs() / 86_400;
    let mut out = Vec::new();

    match record.assessment() {
        Some(a) if a.status != smart::HealthStatus::Good => out.extend(a.notes),
        Some(_) => {}
        None => out.push("Health never checked".to_string()),
    }
    if let Some(d) = age(record.health.last().map(|s| s.at)).filter(|d| *d > STALE_HEALTH) {
        out.push(format!("Health last checked {} days ago", days(d)));
    }
    if !indexed {
        out.push("Not indexed yet".to_string());
    } else if let Some(d) = age(record.last_indexed).filter(|d| *d > STALE_INDEX) {
        out.push(format!("Last indexed {} days ago", days(d)));
    }
    if !attached && let Some(d) = age(record.last_seen).filter(|d| *d > STALE_SEEN) {
        out.push(format!(
            "Not plugged in for {} days; old drives can fail sitting unused",
            days(d)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_db::IndexedFile;
    use crate::smart::{REALLOCATED, SmartReport};

    fn vol(letter: &str, label: &str, serial: Option<u32>) -> VolumeInfo {
        VolumeInfo {
            drive_letter: letter.into(),
            label: label.into(),
            serial,
        }
    }

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn report(reallocated: u64) -> SmartReport {
        SmartReport {
            identity: Some(DeviceIdentity {
                model: "WDC WD40EFRX".into(),
                serial: "WD-123".into(),
                firmware: "82.00A82".into(),
                bus: "USB".into(),
            }),
            attributes: vec![SmartAttribute {
                id: REALLOCATED,
                prefail: true,
                current: 200,
                worst: 200,
                threshold: 140,
                raw: reallocated,
            }],
        }
    }

    #[test]
    fn seeing_an_untracked_drive_does_not_track_it() {
        let mut reg = DriveRegistry::default();
        assert!(!reg.record_seen(&vol("E:", "Stick", Some(1)), None, t(5)));
        assert!(reg.is_empty());
    }

    #[test]
    fn a_tracked_drive_is_found_again_under_another_letter() {
        let mut reg = DriveRegistry::default();
        reg.track(&vol("E:", "Backup A", Some(1)));

        assert!(reg.record_seen(&vol("G:", "Backup A", Some(1)), None, t(10)));

        let record = reg.get(&vol("G:", "Backup A", Some(1)).key()).unwrap();
        assert_eq!(record.last_letter, "G:");
        assert_eq!(record.last_seen, Some(t(10)));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn legacy_records_move_to_the_serial_key_and_keep_their_twin() {
        let mut reg = DriveRegistry::default();
        let legacy_a = vol("E:", "Backup A", None);
        let legacy_b = vol("F:", "Backup B", None);
        reg.track(&legacy_a).usage = Some(VolumeUsage {
            total: 10,
            free: 4,
            recorded_at: t(1),
        });
        reg.track(&legacy_b);
        reg.set_twin(&legacy_a.key(), Some(&legacy_b.key()));

        let a = vol("E:", "Backup A", Some(0xA));
        assert!(reg.record_seen(&a, None, t(20)));

        assert!(reg.get(&legacy_a.key()).is_none());
        let record = reg.get(&a.key()).unwrap();
        assert_eq!(record.usage.unwrap().free, 4);
        assert_eq!(record.twin.as_deref(), Some(legacy_b.key().as_str()));
        assert_eq!(
            reg.get(&legacy_b.key()).unwrap().twin.as_deref(),
            Some(a.key().as_str()),
            "the twin points at the new key too"
        );
    }

    #[test]
    fn twins_are_paired_both_ways_and_repairing_unpairs_the_old_partner() {
        let mut reg = DriveRegistry::default();
        let (a, b, c) = (
            vol("E:", "A", Some(1)).key(),
            vol("F:", "B", Some(2)).key(),
            vol("G:", "C", Some(3)).key(),
        );
        for (l, n, s) in [("E:", "A", 1), ("F:", "B", 2), ("G:", "C", 3)] {
            reg.track(&vol(l, n, Some(s)));
        }

        reg.set_twin(&a, Some(&b));
        assert_eq!(reg.get(&b).unwrap().twin.as_deref(), Some(a.as_str()));

        reg.set_twin(&a, Some(&c));
        assert_eq!(reg.get(&b).unwrap().twin, None);
        assert_eq!(reg.get(&c).unwrap().twin.as_deref(), Some(a.as_str()));

        reg.forget(&c);
        assert_eq!(reg.get(&a).unwrap().twin, None);
    }

    #[test]
    fn health_checks_build_a_history_and_compare_against_the_previous_one() {
        let mut reg = DriveRegistry::default();
        let v = vol("E:", "A", Some(1));
        reg.record_health(&v, report(0), t(1));
        assert_eq!(
            reg.get(&v.key()).unwrap().assessment().unwrap().status,
            smart::HealthStatus::Good
        );

        reg.record_health(&v, report(16), t(2));

        let record = reg.get(&v.key()).unwrap();
        assert_eq!(record.health.len(), 2);
        assert_eq!(record.device.as_ref().unwrap().serial, "WD-123");
        let a = record.assessment().unwrap();
        assert_eq!(a.status, smart::HealthStatus::Caution);
        assert!(a.notes[0].contains("up from 0"));
    }

    #[test]
    fn indexed_volumes_get_records_with_their_legacy_usage() {
        let mut db = IndexDb::default();
        db.upsert(vec![(
            "h".into(),
            IndexedFile {
                volume_label: "data".into(),
                drive_letter: "D:".into(),
                volume_serial: None,
                rel_path: "a.jpg".into(),
                size: 1,
                modified: t(0),
            },
        )]);
        let usage = VolumeUsage {
            total: 100,
            free: 50,
            recorded_at: t(3),
        };
        let mut reg = DriveRegistry::default();

        assert!(reg.sync_with_index(&db, &HashMap::from([("D:|data".to_string(), usage)])));
        assert_eq!(reg.get("D:|data").unwrap().usage, Some(usage));
        assert!(
            !reg.sync_with_index(&db, &HashMap::new()),
            "nothing new the second time"
        );
    }

    #[test]
    fn attention_flags_overdue_things_but_not_an_attached_drive_being_unplugged() {
        let now = t(400 * 86_400);
        let mut record = DriveRecord::new(&vol("E:", "A", Some(1)));
        record.last_seen = Some(t(0));
        record.last_indexed = Some(t(0));

        let notes = attention(&record, true, false, now);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].contains("never checked"));
        assert!(notes[1].contains("400 days"));
        assert!(notes[2].contains("Not plugged in"));

        assert_eq!(attention(&record, true, true, now).len(), 2);
        assert!(attention(&record, false, true, now).contains(&"Not indexed yet".to_string()));
    }

    #[test]
    fn registry_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drives.json");
        let mut reg = DriveRegistry::default();
        reg.record_health(&vol("E:", "A", Some(1)), report(3), t(9));
        reg.save(&path).unwrap();

        let loaded = DriveRegistry::load(&path);
        assert_eq!(loaded.sorted().len(), 1);
        assert_eq!(loaded.sorted()[0].1, reg.sorted()[0].1);
    }
}
