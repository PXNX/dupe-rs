use crate::archive::split_archive_name;
use crate::control::JobControl;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// One file to move from a subfolder into the flattened folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMove {
    pub from: PathBuf,
    pub to: PathBuf,
    /// The file's name had to change to avoid a clash.
    pub renamed: bool,
}

/// Plans moving every file in `root`'s subfolders (at any depth) directly
/// into `root`. Clashing names get a Windows-style ` (2)`, ` (3)`, ... before
/// the extension, compared case-insensitively like NTFS, and never reuse the
/// name of anything already in `root` (files *or* folders). Volumes of a
/// split archive from the same folder are renamed together with the same
/// number, so the set still opens after the move. Returns `None` if
/// cancelled.
pub fn plan_flatten(root: &Path, control: &JobControl) -> Option<Vec<PlannedMove>> {
    let mut taken: HashSet<String> = std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    // Files in subfolders, grouped into rename units: a split-archive set
    // (same folder, same stem) moves as one unit, anything else alone.
    let mut units: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .min_depth(2)
        .sort_by_file_name();
    for entry in walker {
        if control.checkpoint() {
            return None;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let parent = path.parent().unwrap_or(root).to_path_buf();
        let key = match split_archive_name(&name) {
            Some((stem, _)) => (parent, format!("archive:{}", stem.to_lowercase())),
            None => (parent, format!("file:{name}")),
        };
        units.entry(key).or_default().push(path);
    }

    let mut moves = Vec::new();
    for files in units.into_values() {
        let names: Vec<(String, String)> = files
            .iter()
            .map(|p| split_name(&p.file_name().unwrap_or_default().to_string_lossy()))
            .collect();
        let target = |n: u32, stem: &str, suffix: &str| {
            if n == 1 {
                format!("{stem}{suffix}")
            } else {
                format!("{stem} ({n}){suffix}")
            }
        };
        let n = (1u32..)
            .find(|&n| {
                names
                    .iter()
                    .all(|(stem, suffix)| !taken.contains(&target(n, stem, suffix).to_lowercase()))
            })
            .expect("some number is free");
        for (from, (stem, suffix)) in files.into_iter().zip(&names) {
            let new_name = target(n, stem, suffix);
            taken.insert(new_name.to_lowercase());
            moves.push(PlannedMove {
                to: root.join(&new_name),
                from,
                renamed: n > 1,
            });
        }
    }
    Some(moves)
}

/// `(stem, suffix)` where the suffix is what a clash number goes in front
/// of: a split-archive volume suffix (`.part1.rar`, `.r00`), otherwise the
/// last extension (`.jpg`), or nothing.
fn split_name(name: &str) -> (String, String) {
    if let Some((stem, suffix)) = split_archive_name(name) {
        return (stem.to_string(), suffix.to_string());
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_string(), name[dot..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(moves: &[PlannedMove]) -> Vec<String> {
        moves
            .iter()
            .map(|m| m.to.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn renames_clashes_windows_style_and_avoids_existing_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("photo.jpg"), b"already here").unwrap();
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(root.join("b/deeper")).unwrap();
        fs::write(root.join("a/photo.jpg"), b"1").unwrap();
        fs::write(root.join("b/PHOTO.jpg"), b"2").unwrap();
        fs::write(root.join("b/deeper/notes"), b"3").unwrap();
        // A file named like an existing folder must not take its name.
        fs::write(root.join("b/deeper/a"), b"4").unwrap();

        let moves = plan_flatten(root, &JobControl::default()).unwrap();
        let mut got = names(&moves);
        got.sort();
        assert_eq!(got, ["PHOTO (3).jpg", "a (2)", "notes", "photo (2).jpg"]);
        assert!(moves.iter().all(|m| m.to.parent() == Some(root)));
        assert_eq!(moves.iter().filter(|m| m.renamed).count(), 3);
    }

    #[test]
    fn split_archive_sets_are_renamed_together() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("movie.part2.rar"), b"existing part").unwrap();
        fs::create_dir(root.join("dl")).unwrap();
        fs::write(root.join("dl/movie.part1.rar"), b"p1").unwrap();
        fs::write(root.join("dl/movie.part2.rar"), b"p2").unwrap();
        fs::write(root.join("dl/movie.part3.rar"), b"p3").unwrap();

        let moves = plan_flatten(root, &JobControl::default()).unwrap();
        assert_eq!(
            names(&moves),
            [
                "movie (2).part1.rar",
                "movie (2).part2.rar",
                "movie (2).part3.rar"
            ],
            "one clash renames the whole set with the same number"
        );
    }

    #[test]
    fn files_already_in_the_root_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("top.txt"), b"x").unwrap();
        assert!(
            plan_flatten(dir.path(), &JobControl::default())
                .unwrap()
                .is_empty()
        );
    }
}
