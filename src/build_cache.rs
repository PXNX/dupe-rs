//! "Build caches": find regenerable build output and dependency folders
//! (`node_modules`, Python venvs, Rust/Maven `target`, Gradle `build`, ...)
//! below the chosen folders, so they can be reviewed and removed.
//!
//! Generic names like `target`, `build`, or `bin` only count next to the
//! project file that makes them build output (`Cargo.toml`, `build.gradle`,
//! `*.csproj`, ...), so an ordinary folder that happens to share the name is
//! never listed.

use crate::config::ScanConfig;
use crate::control::JobControl;
use crate::model::{CacheDir, ScanEvent};
use crossbeam_channel::Sender;
use rayon::prelude::*;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, SystemTime};
use walkdir::WalkDir;

const PROGRESS_INTERVAL: usize = 500;

/// The ecosystem a cache folder belongs to; each one can be switched off in
/// the settings panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CacheKind {
    Node,
    Python,
    Rust,
    Gradle,
    Maven,
    DotNet,
    Flutter,
    /// Any folder marked with a `CACHEDIR.TAG` (see
    /// <https://bford.info/cachedir/>), which by definition can be deleted.
    Tagged,
}

impl CacheKind {
    pub const ALL: [CacheKind; 8] = [
        CacheKind::Node,
        CacheKind::Python,
        CacheKind::Rust,
        CacheKind::Gradle,
        CacheKind::Maven,
        CacheKind::DotNet,
        CacheKind::Flutter,
        CacheKind::Tagged,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CacheKind::Node => "Node.js / web",
            CacheKind::Python => "Python",
            CacheKind::Rust => "Rust",
            CacheKind::Gradle => "Android / Gradle",
            CacheKind::Maven => "Java (Maven)",
            CacheKind::DotNet => ".NET",
            CacheKind::Flutter => "Flutter / Dart",
            CacheKind::Tagged => "Other (CACHEDIR.TAG)",
        }
    }

    /// What gets listed, for the settings panel's hover text.
    pub fn description(self) -> &'static str {
        match self {
            CacheKind::Node => {
                "node_modules, .next, .nuxt, .output, .svelte-kit, .turbo, .parcel-cache, \
                 .angular/cache, .vercel/output"
            }
            CacheKind::Python => {
                "Virtual environments (any folder with a pyvenv.cfg), __pycache__, \
                 .pytest_cache, .mypy_cache, .ruff_cache, .tox, .nox"
            }
            CacheKind::Rust => "target next to a Cargo.toml",
            CacheKind::Gradle => {
                "build, .gradle, .kotlin, .cxx, .externalNativeBuild next to a Gradle build file"
            }
            CacheKind::Maven => "target next to a pom.xml",
            CacheKind::DotNet => "bin and obj next to a .csproj/.fsproj/.vbproj",
            CacheKind::Flutter => ".dart_tool, and build next to a pubspec.yaml",
            CacheKind::Tagged => "Any folder containing a valid CACHEDIR.TAG",
        }
    }
}

/// What has to hold, besides the folder's name, for it to count as a cache.
enum Marker {
    /// The name alone is distinctive enough.
    Always,
    /// The containing folder holds one of these files.
    Sibling(&'static [&'static str]),
    /// The containing folder holds a file with one of these extensions.
    SiblingExt(&'static [&'static str]),
    /// The containing folder is named this.
    ParentNamed(&'static str),
}

struct Rule {
    /// Folder name, matched case-insensitively.
    name: &'static str,
    kind: CacheKind,
    marker: Marker,
}

const GRADLE_FILES: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
];

/// Checked in order; the first match wins.
const RULES: &[Rule] = &[
    Rule { name: "node_modules", kind: CacheKind::Node, marker: Marker::Always },
    Rule { name: ".next", kind: CacheKind::Node, marker: Marker::Sibling(&["package.json"]) },
    Rule { name: ".nuxt", kind: CacheKind::Node, marker: Marker::Sibling(&["package.json"]) },
    Rule { name: ".output", kind: CacheKind::Node, marker: Marker::Sibling(&["package.json"]) },
    Rule { name: ".svelte-kit", kind: CacheKind::Node, marker: Marker::Sibling(&["package.json"]) },
    Rule { name: ".turbo", kind: CacheKind::Node, marker: Marker::Always },
    Rule { name: ".parcel-cache", kind: CacheKind::Node, marker: Marker::Always },
    // `.vercel` and `.angular` also hold project settings; only their
    // output/cache subfolders are disposable.
    Rule { name: "output", kind: CacheKind::Node, marker: Marker::ParentNamed(".vercel") },
    Rule { name: "cache", kind: CacheKind::Node, marker: Marker::ParentNamed(".angular") },
    Rule { name: "__pycache__", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: ".pytest_cache", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: ".mypy_cache", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: ".ruff_cache", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: ".tox", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: ".nox", kind: CacheKind::Python, marker: Marker::Always },
    Rule { name: "target", kind: CacheKind::Rust, marker: Marker::Sibling(&["Cargo.toml"]) },
    Rule { name: "target", kind: CacheKind::Maven, marker: Marker::Sibling(&["pom.xml"]) },
    Rule { name: "build", kind: CacheKind::Gradle, marker: Marker::Sibling(GRADLE_FILES) },
    Rule { name: ".gradle", kind: CacheKind::Gradle, marker: Marker::Sibling(GRADLE_FILES) },
    Rule { name: ".kotlin", kind: CacheKind::Gradle, marker: Marker::Sibling(GRADLE_FILES) },
    Rule { name: ".cxx", kind: CacheKind::Gradle, marker: Marker::Sibling(GRADLE_FILES) },
    Rule {
        name: ".externalNativeBuild",
        kind: CacheKind::Gradle,
        marker: Marker::Sibling(GRADLE_FILES),
    },
    Rule {
        name: "bin",
        kind: CacheKind::DotNet,
        marker: Marker::SiblingExt(&["csproj", "fsproj", "vbproj"]),
    },
    Rule {
        name: "obj",
        kind: CacheKind::DotNet,
        marker: Marker::SiblingExt(&["csproj", "fsproj", "vbproj"]),
    },
    Rule { name: ".dart_tool", kind: CacheKind::Flutter, marker: Marker::Always },
    Rule { name: "build", kind: CacheKind::Flutter, marker: Marker::Sibling(&["pubspec.yaml"]) },
];

const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Folders never worth descending into while looking for caches.
const SKIP_DESCENT: &[&str] = &[".git", ".hg", ".svn"];

/// Which kind of cache `dir` is, if any, among the `enabled` kinds.
pub fn classify(dir: &Path, enabled: &[CacheKind]) -> Option<CacheKind> {
    let name = dir.file_name()?.to_str()?;
    let parent = dir.parent()?;
    for rule in RULES {
        if !enabled.contains(&rule.kind) || !rule.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let matched = match rule.marker {
            Marker::Always => true,
            Marker::Sibling(files) => files.iter().any(|f| parent.join(f).is_file()),
            Marker::SiblingExt(exts) => has_file_with_extension(parent, exts),
            Marker::ParentNamed(parent_name) => parent
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(parent_name)),
        };
        if matched {
            return Some(rule.kind);
        }
    }
    // Virtual environments go by any name (`venv`, `.venv`, `env`, ...), so
    // they're recognized by the `pyvenv.cfg` every one of them holds.
    if enabled.contains(&CacheKind::Python) && dir.join("pyvenv.cfg").is_file() {
        return Some(CacheKind::Python);
    }
    if enabled.contains(&CacheKind::Tagged) && has_cachedir_tag(dir) {
        return Some(CacheKind::Tagged);
    }
    None
}

fn has_file_with_extension(dir: &Path, exts: &[&str]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_type().is_ok_and(|t| t.is_file())
            && Path::new(&e.file_name())
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.iter().any(|want| want.eq_ignore_ascii_case(x)))
    })
}

fn has_cachedir_tag(dir: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(dir.join("CACHEDIR.TAG")) else {
        return false;
    };
    let mut head = [0u8; CACHEDIR_TAG_SIGNATURE.len()];
    file.read_exact(&mut head).is_ok() && head == CACHEDIR_TAG_SIGNATURE
}

/// Runs a `ScanMode::BuildCaches` scan: walks every configured folder for
/// cache folders (without descending into the ones it finds), then totals up
/// each one in parallel, reporting it as soon as it's sized.
pub fn run_scan(config: ScanConfig, tx: Sender<ScanEvent>, control: std::sync::Arc<JobControl>) {
    let start = Instant::now();
    let scanned = AtomicUsize::new(0);
    let found = find_cache_dirs(&config, &control, &tx, &scanned);

    found.par_iter().for_each(|(path, kind)| {
        if control.checkpoint() {
            return;
        }
        let entry = measure(path, *kind, &control, &tx, &scanned);
        if config.min_size.is_some_and(|min| entry.size < min)
            || config.max_size.is_some_and(|max| entry.size > max)
        {
            return;
        }
        let _ = tx.send(ScanEvent::CachesFound(vec![entry]));
    });

    let _ = tx.send(ScanEvent::Progress {
        scanned: scanned.load(Ordering::Relaxed),
    });
    let _ = tx.send(ScanEvent::Done {
        elapsed_ms: start.elapsed().as_millis(),
    });
}

fn find_cache_dirs(
    config: &ScanConfig,
    control: &JobControl,
    tx: &Sender<ScanEvent>,
    scanned: &AtomicUsize,
) -> Vec<(PathBuf, CacheKind)> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for root in &config.folders {
        let mut walker = WalkDir::new(root).follow_links(false).min_depth(1).into_iter();
        while let Some(entry) = walker.next() {
            if control.checkpoint() {
                return found;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(ScanEvent::Error(format!("Skipping entry: {e}")));
                    continue;
                }
            };
            if !entry.file_type().is_dir() {
                continue;
            }
            count(scanned, tx);
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| SKIP_DESCENT.iter().any(|s| s.eq_ignore_ascii_case(n)))
            {
                walker.skip_current_dir();
                continue;
            }
            if let Some(kind) = classify(entry.path(), &config.cache_kinds) {
                // Overlapping roots would otherwise list the same folder twice.
                if seen.insert(entry.path().to_path_buf()) {
                    found.push((entry.path().to_path_buf(), kind));
                }
                walker.skip_current_dir();
            }
        }
    }
    found
}

/// Totals up everything inside a cache folder.
fn measure(
    path: &Path,
    kind: CacheKind,
    control: &JobControl,
    tx: &Sender<ScanEvent>,
    scanned: &AtomicUsize,
) -> CacheDir {
    let mut size = 0;
    let mut file_count = 0;
    let mut modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in WalkDir::new(path).follow_links(false).min_depth(1).into_iter().flatten() {
        if control.checkpoint() {
            break;
        }
        count(scanned, tx);
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if let Ok(m) = meta.modified() {
            modified = modified.max(m);
        }
        if meta.is_file() {
            size += meta.len();
            file_count += 1;
        }
    }
    CacheDir {
        path: path.to_path_buf(),
        kind,
        size,
        file_count,
        modified,
    }
}

fn count(scanned: &AtomicUsize, tx: &Sender<ScanEvent>) {
    let n = scanned.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(PROGRESS_INTERVAL) {
        let _ = tx.send(ScanEvent::Progress { scanned: n });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn mkdir(path: &Path) {
        fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn generic_names_only_count_next_to_their_project_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        mkdir(&root.join("rusty/target"));
        fs::write(root.join("rusty/Cargo.toml"), "").unwrap();
        mkdir(&root.join("maven/target"));
        fs::write(root.join("maven/pom.xml"), "").unwrap();
        mkdir(&root.join("photos/target"));
        mkdir(&root.join("app/build"));
        fs::write(root.join("app/build.gradle.kts"), "").unwrap();
        mkdir(&root.join("docs/build"));
        mkdir(&root.join("cs/bin"));
        fs::write(root.join("cs/Tool.CSPROJ"), "").unwrap();
        mkdir(&root.join("scripts/bin"));

        let all = CacheKind::ALL;
        assert_eq!(classify(&root.join("rusty/target"), &all), Some(CacheKind::Rust));
        assert_eq!(classify(&root.join("maven/target"), &all), Some(CacheKind::Maven));
        assert_eq!(classify(&root.join("photos/target"), &all), None);
        assert_eq!(classify(&root.join("app/build"), &all), Some(CacheKind::Gradle));
        assert_eq!(classify(&root.join("docs/build"), &all), None);
        assert_eq!(classify(&root.join("cs/bin"), &all), Some(CacheKind::DotNet));
        assert_eq!(classify(&root.join("scripts/bin"), &all), None);
        assert_eq!(classify(&root.join("rusty/target"), &[CacheKind::Node]), None);
    }

    #[test]
    fn venvs_vercel_output_and_tagged_folders_are_recognized() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        mkdir(&root.join("py/my-env"));
        fs::write(root.join("py/my-env/pyvenv.cfg"), "home = x").unwrap();
        mkdir(&root.join("web/.vercel/output"));
        mkdir(&root.join("other/output"));
        mkdir(&root.join("tagged"));
        fs::write(
            root.join("tagged/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# a cache",
        )
        .unwrap();
        mkdir(&root.join("fake"));
        fs::write(root.join("fake/CACHEDIR.TAG"), b"not a real tag").unwrap();

        let all = CacheKind::ALL;
        assert_eq!(classify(&root.join("py/my-env"), &all), Some(CacheKind::Python));
        assert_eq!(classify(&root.join("web/.vercel/output"), &all), Some(CacheKind::Node));
        assert_eq!(classify(&root.join("web/.vercel"), &all), None);
        assert_eq!(classify(&root.join("other/output"), &all), None);
        assert_eq!(classify(&root.join("tagged"), &all), Some(CacheKind::Tagged));
        assert_eq!(classify(&root.join("fake"), &all), None);
    }

    #[test]
    fn scan_sizes_caches_and_does_not_list_nested_ones() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let modules = root.join("site/node_modules");
        mkdir(&modules.join("dep/node_modules/inner"));
        fs::write(modules.join("dep/index.js"), b"12345").unwrap();
        fs::write(modules.join("dep/node_modules/inner/a.js"), b"123").unwrap();
        fs::write(root.join("site/index.js"), b"keep").unwrap();

        let config = ScanConfig {
            folders: vec![root.to_path_buf()],
            mode: crate::config::ScanMode::BuildCaches,
            ..ScanConfig::default()
        };
        let (tx, rx) = unbounded();
        run_scan(config, tx, Arc::new(JobControl::default()));
        let found: Vec<CacheDir> = rx
            .try_iter()
            .filter_map(|e| match e {
                ScanEvent::CachesFound(c) => Some(c),
                _ => None,
            })
            .flatten()
            .collect();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, modules);
        assert_eq!(found[0].kind, CacheKind::Node);
        assert_eq!(found[0].size, 8);
        assert_eq!(found[0].file_count, 2);
    }
}
