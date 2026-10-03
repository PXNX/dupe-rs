//! "Disk usage": total up how much space every folder below a chosen folder
//! takes, at any depth, shown as a size-sorted tree.

use crate::control::JobControl;
use crossbeam_channel::{Receiver, Sender};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

/// How often the walker reports how far it has got.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// One folder of the measured tree. Totals include everything below it.
#[derive(Clone, Debug)]
pub struct DirNode {
    pub path: PathBuf,
    pub name: String,
    pub parent: Option<usize>,
    pub depth: usize,
    /// Subfolders, largest first.
    pub children: Vec<usize>,
    /// Sum of file sizes at any depth (what Explorer shows as "Size").
    pub size: u64,
    pub file_count: u64,
    /// Subfolders at any depth.
    pub dir_count: u64,
    /// Files sitting directly in this folder.
    pub own_size: u64,
    pub own_file_count: u64,
}

/// Every folder below the chosen one; index 0 is the chosen folder itself,
/// and parents always come before their children.
#[derive(Clone, Debug, Default)]
pub struct DirTree {
    pub nodes: Vec<DirNode>,
    /// Entries that couldn't be read (e.g. access denied) and so aren't
    /// counted.
    pub unreadable: usize,
}

impl DirTree {
    pub fn root(&self) -> Option<&DirNode> {
        self.nodes.first()
    }
}

pub enum UsageEvent {
    Progress {
        files: u64,
        bytes: u64,
        current: PathBuf,
    },
    /// `None` if the walk was cancelled.
    Done(Option<DirTree>),
}

/// Walks `root` and totals up every folder in it. Symlinks and junctions
/// aren't followed, so nothing is counted twice through them.
pub fn measure_tree(root: &Path, tx: &Sender<UsageEvent>, control: &JobControl) -> Option<DirTree> {
    let mut tree = DirTree::default();
    // Index of the folder at each depth along the current walk path.
    let mut stack: Vec<usize> = Vec::new();
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut last_progress = Instant::now();

    for entry in WalkDir::new(root).follow_links(false) {
        if control.checkpoint() {
            return None;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                tree.unreadable += 1;
                continue;
            }
        };
        let depth = entry.depth();
        stack.truncate(depth);
        let parent = stack.last().copied();
        let file_type = entry.file_type();
        if file_type.is_dir() {
            let name = if depth == 0 {
                entry.path().display().to_string()
            } else {
                entry.file_name().to_string_lossy().into_owned()
            };
            let idx = tree.nodes.len();
            tree.nodes.push(DirNode {
                path: entry.path().to_path_buf(),
                name,
                parent,
                depth,
                children: Vec::new(),
                size: 0,
                file_count: 0,
                dir_count: 0,
                own_size: 0,
                own_file_count: 0,
            });
            if let Some(p) = parent {
                tree.nodes[p].children.push(idx);
            }
            stack.push(idx);
        } else if file_type.is_file()
            && let Some(p) = parent
        {
            match entry.metadata() {
                Ok(meta) => {
                    let node = &mut tree.nodes[p];
                    node.own_size += meta.len();
                    node.own_file_count += 1;
                    files += 1;
                    bytes += meta.len();
                }
                Err(_) => tree.unreadable += 1,
            }
        }

        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            let _ = tx.send(UsageEvent::Progress {
                files,
                bytes,
                current: entry.path().to_path_buf(),
            });
        }
    }

    for node in &mut tree.nodes {
        node.size = node.own_size;
        node.file_count = node.own_file_count;
    }
    // Children always come after their parent, so walking backwards folds
    // each folder's totals into its parent once they're complete.
    for i in (1..tree.nodes.len()).rev() {
        let (size, file_count, dir_count) = {
            let n = &tree.nodes[i];
            (n.size, n.file_count, n.dir_count)
        };
        if let Some(p) = tree.nodes[i].parent {
            let parent = &mut tree.nodes[p];
            parent.size += size;
            parent.file_count += file_count;
            parent.dir_count += dir_count + 1;
        }
    }
    for i in 0..tree.nodes.len() {
        let mut children = std::mem::take(&mut tree.nodes[i].children);
        children.sort_by(|&a, &b| {
            let (a, b) = (&tree.nodes[a], &tree.nodes[b]);
            b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))
        });
        tree.nodes[i].children = children;
    }
    Some(tree)
}

/// One line of the tree view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Dir(usize),
    /// The files sitting directly in this folder, shown under its expanded
    /// subfolders so the sizes listed add up to the folder's total.
    Files(usize),
}

struct UsageJob {
    rx: Receiver<UsageEvent>,
    control: Arc<JobControl>,
    started: Instant,
}

#[derive(Default)]
pub struct DiskUsageState {
    pub root: Option<PathBuf>,
    pub tree: Option<DirTree>,
    job: Option<UsageJob>,
    /// Files and bytes counted so far, and where the walk currently is.
    pub progress: (u64, u64, Option<PathBuf>),
    expanded: HashSet<usize>,
    /// The visible rows, rebuilt whenever the tree or `expanded` changes.
    rows: Vec<Row>,
    pub status: Option<String>,
}

impl DiskUsageState {
    pub fn is_scanning(&self) -> bool {
        self.job.is_some()
    }

    pub fn needs_polling(&self) -> bool {
        self.is_scanning()
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root = Some(root);
        self.rescan();
    }

    /// Measures the chosen folder (again) in the background.
    pub fn rescan(&mut self) {
        self.cancel();
        let Some(root) = self.root.clone() else {
            return;
        };
        self.tree = None;
        self.expanded.clear();
        self.rows.clear();
        self.status = None;
        self.progress = (0, 0, None);
        let (tx, rx) = crossbeam_channel::unbounded();
        let control = Arc::new(JobControl::default());
        let control_for_thread = control.clone();
        std::thread::spawn(move || {
            let tree = measure_tree(&root, &tx, &control_for_thread);
            let _ = tx.send(UsageEvent::Done(tree));
        });
        self.job = Some(UsageJob {
            rx,
            control,
            started: Instant::now(),
        });
    }

    pub fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.control.cancel();
            self.status = Some("Cancelled.".into());
        }
    }

    pub fn drain_events(&mut self) -> bool {
        let mut changed = false;
        let mut done = None;
        if let Some(job) = &self.job {
            for event in job.rx.try_iter() {
                changed = true;
                match event {
                    UsageEvent::Progress {
                        files,
                        bytes,
                        current,
                    } => self.progress = (files, bytes, Some(current)),
                    UsageEvent::Done(tree) => done = Some(tree),
                }
            }
        }
        if let Some(tree) = done
            && let Some(job) = self.job.take()
        {
            self.status = Some(match &tree {
                None => "Cancelled.".into(),
                Some(tree) => match tree.root() {
                    None => "Couldn't read the folder.".into(),
                    Some(root) => {
                        let unreadable = if tree.unreadable > 0 {
                            format!(" {} item(s) couldn't be read.", tree.unreadable)
                        } else {
                            String::new()
                        };
                        format!(
                            "{} file(s) in {} folder(s), measured in {:.1}s.{unreadable}",
                            root.file_count,
                            root.dir_count + 1,
                            job.started.elapsed().as_secs_f64()
                        )
                    }
                },
            });
            self.tree = tree.filter(|t| !t.nodes.is_empty());
            if self.tree.is_some() {
                self.expanded.insert(0);
            }
            self.rebuild_rows();
        }
        changed
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn is_expanded(&self, idx: usize) -> bool {
        self.expanded.contains(&idx)
    }

    pub fn toggle(&mut self, idx: usize) {
        if !self.expanded.remove(&idx) {
            self.expanded.insert(idx);
        }
        self.rebuild_rows();
    }

    pub fn expand_all(&mut self) {
        if let Some(tree) = &self.tree {
            self.expanded = (0..tree.nodes.len())
                .filter(|&i| !tree.nodes[i].children.is_empty())
                .collect();
        }
        self.rebuild_rows();
    }

    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        if self.tree.is_some() {
            self.expanded.insert(0);
        }
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(tree) = &self.tree else {
            return;
        };
        // Depth-first, pushing children in reverse so the largest pops first.
        let mut stack = vec![Row::Dir(0)];
        while let Some(row) = stack.pop() {
            self.rows.push(row);
            let Row::Dir(idx) = row else { continue };
            let node = &tree.nodes[idx];
            if !self.expanded.contains(&idx) || node.children.is_empty() {
                continue;
            }
            if node.own_file_count > 0 {
                stack.push(Row::Files(idx));
            }
            stack.extend(node.children.iter().rev().map(|&c| Row::Dir(c)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn measure(root: &Path) -> DirTree {
        let (tx, _rx) = crossbeam_channel::unbounded();
        measure_tree(root, &tx, &JobControl::default()).unwrap()
    }

    #[test]
    fn totals_every_folder_and_sorts_children_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("small/deep")).unwrap();
        fs::create_dir(root.join("big")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        fs::write(root.join("loose.bin"), vec![0u8; 5]).unwrap();
        fs::write(root.join("small/a.bin"), vec![0u8; 10]).unwrap();
        fs::write(root.join("small/deep/b.bin"), vec![0u8; 20]).unwrap();
        fs::write(root.join("big/c.bin"), vec![0u8; 100]).unwrap();

        let tree = measure(root);
        let top = &tree.nodes[0];
        assert_eq!(top.size, 135);
        assert_eq!(top.file_count, 4);
        assert_eq!(top.dir_count, 4);
        assert_eq!((top.own_size, top.own_file_count), (5, 1));

        let names: Vec<_> = top
            .children
            .iter()
            .map(|&c| tree.nodes[c].name.as_str())
            .collect();
        assert_eq!(names, ["big", "small", "empty"]);

        let small = &tree.nodes[top.children[1]];
        assert_eq!((small.size, small.own_size, small.dir_count), (30, 10, 1));
        let deep = &tree.nodes[small.children[0]];
        assert_eq!((deep.name.as_str(), deep.size, deep.depth), ("deep", 20, 2));
        assert_eq!(tree.unreadable, 0);
    }

    #[test]
    fn rows_follow_expansion_and_list_loose_files_last() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("top.bin"), b"x").unwrap();
        fs::write(root.join("a/b/f.bin"), b"y").unwrap();

        let mut state = DiskUsageState::default();
        state.set_root(root.to_path_buf());
        let start = Instant::now();
        while state.needs_polling() {
            state.drain_events();
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        let a = state.tree.as_ref().unwrap().nodes[0].children[0];
        assert_eq!(state.rows(), [Row::Dir(0), Row::Dir(a), Row::Files(0)]);

        state.expand_all();
        assert_eq!(state.rows().len(), 4);
        state.collapse_all();
        assert_eq!(state.rows().len(), 3);
        state.toggle(0);
        assert_eq!(state.rows(), [Row::Dir(0)]);
    }

    #[test]
    fn a_cancelled_walk_returns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = crossbeam_channel::unbounded();
        assert!(measure_tree(dir.path(), &tx, &JobControl::cancelled()).is_none());
    }
}
