//! "Flatten": move every file out of a folder's subfolders into the folder
//! itself, renaming name clashes (see `plan::plan_flatten`), optionally
//! removing the emptied subfolders, with a one-step undo.

pub mod plan;

use crate::control::{ActiveClock, JobControl, estimate_remaining};
use crossbeam_channel::{Receiver, Sender};
use plan::PlannedMove;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use walkdir::WalkDir;

pub enum MoveEvent {
    Started { from: PathBuf },
    Moved { from: PathBuf, to: PathBuf },
    Failed(String),
    Finished { removed_dirs: usize },
}

/// Moves each `(from, to)` pair by renaming (the flattened folder and its
/// subfolders share a volume, so no data is copied). An existing `to` is
/// never overwritten. With `create_parents`, missing destination folders are
/// created (needed by undo). Afterwards, empty folders under
/// `remove_empty_under` are deleted, deepest first.
pub fn run_moves(
    moves: Vec<(PathBuf, PathBuf)>,
    create_parents: bool,
    remove_empty_under: Option<PathBuf>,
    tx: Sender<MoveEvent>,
    control: &JobControl,
) {
    for (from, to) in moves {
        if control.checkpoint() {
            break;
        }
        let _ = tx.send(MoveEvent::Started { from: from.clone() });
        let result = if to.exists() {
            Err(format!("{} already exists", to.display()))
        } else {
            (|| {
                if create_parents && let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&from, &to)
            })()
            .map_err(|e| format!("Couldn't move {}: {e}", from.display()))
        };
        let _ = tx.send(match result {
            Ok(()) => MoveEvent::Moved { from, to },
            Err(msg) => MoveEvent::Failed(msg),
        });
    }

    let mut removed_dirs = 0;
    if let Some(root) = remove_empty_under
        && !control.is_cancelled()
    {
        let dirs = WalkDir::new(&root)
            .min_depth(1)
            .contents_first(true)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_dir());
        for dir in dirs {
            // Only succeeds for folders that are actually empty.
            if std::fs::remove_dir(dir.path()).is_ok() {
                removed_dirs += 1;
            }
        }
    }
    let _ = tx.send(MoveEvent::Finished { removed_dirs });
}

pub struct MoveJob {
    rx: Receiver<MoveEvent>,
    control: Arc<JobControl>,
    clock: ActiveClock,
    pub undo: bool,
    pub total: usize,
    pub done: usize,
    pub current: Option<PathBuf>,
    /// Moves that went through, in order, for undo.
    completed: Vec<(PathBuf, PathBuf)>,
}

impl MoveJob {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            self.done as f32 / self.total as f32
        }
    }

    pub fn items_per_sec(&self) -> Option<f64> {
        let secs = self.clock.elapsed().as_secs_f64();
        (self.done > 0 && secs > 0.0).then(|| self.done as f64 / secs)
    }

    pub fn eta(&self) -> Option<Duration> {
        estimate_remaining(self.done as u64, self.total as u64, self.clock.elapsed())
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

pub struct FlattenState {
    pub root: Option<PathBuf>,
    pub remove_empty_dirs: bool,
    /// Preview of what "Move" will do; recomputed whenever the folder
    /// changes or a run finishes.
    pub plan: Vec<PlannedMove>,
    planning: Option<(Receiver<Option<Vec<PlannedMove>>>, Arc<JobControl>)>,
    pub job: Option<MoveJob>,
    /// The moves the last completed run made, which "Undo" reverses.
    pub last_run: Vec<(PathBuf, PathBuf)>,
    pub errors: Vec<String>,
    pub status: Option<String>,
}

impl Default for FlattenState {
    fn default() -> Self {
        Self {
            root: None,
            remove_empty_dirs: true,
            plan: Vec::new(),
            planning: None,
            job: None,
            last_run: Vec::new(),
            errors: Vec::new(),
            status: None,
        }
    }
}

impl FlattenState {
    pub fn is_planning(&self) -> bool {
        self.planning.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn needs_polling(&self) -> bool {
        self.is_planning() || self.is_running()
    }

    pub fn set_root(&mut self, root: PathBuf) {
        if self.is_running() {
            return;
        }
        if self.root.as_ref() != Some(&root) {
            self.last_run.clear();
        }
        self.root = Some(root);
        self.status = None;
        self.errors.clear();
        self.replan();
    }

    /// Recomputes the preview in the background.
    pub fn replan(&mut self) {
        if let Some((_, control)) = self.planning.take() {
            control.cancel();
        }
        self.plan.clear();
        let Some(root) = self.root.clone() else {
            return;
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || {
            let _ = tx.send(plan::plan_flatten(&root, &control_for_thread));
        });
        self.planning = Some((rx, control));
    }

    pub fn renamed_count(&self) -> usize {
        self.plan.iter().filter(|m| m.renamed).count()
    }

    /// Carries out the previewed moves.
    pub fn start(&mut self) {
        if self.is_running() || self.is_planning() || self.plan.is_empty() {
            return;
        }
        let moves = self
            .plan
            .iter()
            .map(|m| (m.from.clone(), m.to.clone()))
            .collect();
        let cleanup = self.remove_empty_dirs.then(|| self.root.clone()).flatten();
        self.spawn(moves, false, cleanup, false);
    }

    /// Puts every file the last run moved back where it came from.
    pub fn undo(&mut self) {
        if self.is_running() || self.last_run.is_empty() {
            return;
        }
        let moves = self
            .last_run
            .iter()
            .rev()
            .map(|(from, to)| (to.clone(), from.clone()))
            .collect();
        self.spawn(moves, true, None, true);
    }

    fn spawn(
        &mut self,
        moves: Vec<(PathBuf, PathBuf)>,
        create_parents: bool,
        cleanup: Option<PathBuf>,
        undo: bool,
    ) {
        let total = moves.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || {
            run_moves(moves, create_parents, cleanup, tx, &control_for_thread);
        });
        self.errors.clear();
        self.status = None;
        self.job = Some(MoveJob {
            rx,
            control,
            clock: ActiveClock::start(),
            undo,
            total,
            done: 0,
            current: None,
            completed: Vec::new(),
        });
    }

    pub fn drain_events(&mut self) -> bool {
        let mut changed = false;
        if let Some((rx, _)) = &self.planning
            && let Ok(result) = rx.try_recv()
        {
            self.plan = result.unwrap_or_default();
            self.planning = None;
            changed = true;
        }

        let mut finished = None;
        if let Some(job) = &mut self.job {
            for event in job.rx.try_iter().take(5000) {
                changed = true;
                match event {
                    MoveEvent::Started { from } => job.current = Some(from),
                    MoveEvent::Moved { from, to } => {
                        job.done += 1;
                        job.completed.push((from, to));
                    }
                    MoveEvent::Failed(msg) => {
                        job.done += 1;
                        self.errors.push(msg);
                    }
                    MoveEvent::Finished { removed_dirs } => finished = Some(removed_dirs),
                }
            }
        }
        if let Some(removed_dirs) = finished
            && let Some(job) = self.job.take()
        {
            let moved = job.completed.len();
            let cancelled = if job.is_cancelled() {
                "Cancelled. "
            } else {
                ""
            };
            let failed = if self.errors.is_empty() {
                String::new()
            } else {
                format!(" {} failed.", self.errors.len())
            };
            self.status = Some(if job.undo {
                self.last_run.clear();
                format!("{cancelled}Undo moved {moved} file(s) back.{failed}")
            } else {
                self.last_run = job.completed;
                format!(
                    "{cancelled}Moved {moved} file(s) and removed {removed_dirs} empty folder(s).{failed}"
                )
            });
            self.replan();
        }
        changed
    }
}

/// `path` relative to `root`, for display.
pub fn relative<'a>(path: &'a Path, root: &Path) -> std::borrow::Cow<'a, str> {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    fn settle(state: &mut FlattenState) {
        let start = Instant::now();
        while state.needs_polling() {
            state.drain_events();
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn flattens_removes_empty_folders_and_undo_restores_everything() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/one.txt"), b"1").unwrap();
        fs::write(root.join("a/b/one.txt"), b"2").unwrap();
        fs::write(root.join("top.txt"), b"t").unwrap();

        let mut state = FlattenState::default();
        state.set_root(root.to_path_buf());
        settle(&mut state);
        assert_eq!(state.plan.len(), 2);
        assert_eq!(state.renamed_count(), 1);

        state.start();
        settle(&mut state);
        assert_eq!(fs::read(root.join("one.txt")).unwrap(), b"1");
        assert_eq!(fs::read(root.join("one (2).txt")).unwrap(), b"2");
        assert!(!root.join("a").exists(), "emptied folders are removed");
        assert!(state.plan.is_empty(), "nothing left to flatten");

        state.undo();
        settle(&mut state);
        assert_eq!(fs::read(root.join("a/one.txt")).unwrap(), b"1");
        assert_eq!(fs::read(root.join("a/b/one.txt")).unwrap(), b"2");
        assert!(!root.join("one.txt").exists());
        assert!(root.join("top.txt").exists());
    }

    #[test]
    fn never_overwrites_a_file_that_appeared_after_planning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/x.txt"), b"mine").unwrap();

        let mut state = FlattenState::default();
        state.set_root(root.to_path_buf());
        settle(&mut state);
        fs::write(root.join("x.txt"), b"appeared meanwhile").unwrap();
        state.start();
        settle(&mut state);

        assert_eq!(fs::read(root.join("x.txt")).unwrap(), b"appeared meanwhile");
        assert_eq!(fs::read(root.join("sub/x.txt")).unwrap(), b"mine");
        assert_eq!(state.errors.len(), 1);
    }
}
