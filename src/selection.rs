use crate::model::{DupeGroup, FileEntry, MediaEntry, SimilarGroup};
use crate::namematch;

/// Single source of truth for which files are currently "visible" given the
/// `only_show_duplicates` filter (which hides each group's original at index 0).
/// Shared by row rendering, Ctrl+A, and the status bar's "Shown" total so they
/// never disagree with each other.
pub fn compute_visible_entries<'a>(
    groups: impl IntoIterator<Item = &'a DupeGroup>,
    only_show_duplicates: bool,
) -> Vec<&'a FileEntry> {
    groups
        .into_iter()
        .flat_map(|g| {
            if only_show_duplicates {
                g.files[1..].iter()
            } else {
                g.files[..].iter()
            }
        })
        .collect()
}

/// Same idea as `compute_visible_entries`, for `ScanMode::SimilarMedia`
/// results (`SimilarGroup`/`MediaEntry` instead of `DupeGroup`/`FileEntry`).
pub fn compute_visible_media_entries<'a>(
    groups: impl IntoIterator<Item = &'a SimilarGroup>,
    only_show_duplicates: bool,
) -> Vec<&'a MediaEntry> {
    groups
        .into_iter()
        .flat_map(|g| {
            if only_show_duplicates {
                g.files[1..].iter()
            } else {
                g.files[..].iter()
            }
        })
        .collect()
}

fn stem_of(entry: &FileEntry) -> String {
    entry
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// True if any duplicate in the group has a filename that looks like an
/// OS/user-generated copy of the original's filename (e.g. `"photo (2).jpg"`,
/// `"photo - Kopie.jpg"` next to `"photo.jpg"`).
pub fn group_looks_like_name_copy(group: &DupeGroup) -> bool {
    let original_stem = stem_of(&group.files[0]);
    group.files[1..]
        .iter()
        .any(|f| namematch::looks_like_copy(&original_stem, &stem_of(f)))
}

/// Narrows `groups` down to those where `group_looks_like_name_copy` holds,
/// when `enabled`; otherwise returns every group unfiltered. Used by both the
/// table and grid views so the "copy-named only" toggle behaves identically.
pub fn filter_by_name_pattern(groups: &[DupeGroup], enabled: bool) -> Vec<&DupeGroup> {
    if !enabled {
        return groups.iter().collect();
    }
    groups.iter().filter(|g| group_looks_like_name_copy(g)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn file(name: &str) -> FileEntry {
        FileEntry {
            path: PathBuf::from(name),
            size: 1,
            created: SystemTime::UNIX_EPOCH,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn group_with_copy_named_duplicate_is_flagged() {
        let group = DupeGroup {
            hash: [0; 32],
            files: vec![file("photo.jpg"), file("photo (2).jpg")],
        };
        assert!(group_looks_like_name_copy(&group));
    }

    #[test]
    fn group_with_unrelated_names_is_not_flagged() {
        let group = DupeGroup {
            hash: [0; 32],
            files: vec![file("invoice.pdf"), file("receipt.pdf")],
        };
        assert!(!group_looks_like_name_copy(&group));
    }

    #[test]
    fn filter_by_name_pattern_keeps_only_matching_groups_when_enabled() {
        let groups = vec![
            DupeGroup {
                hash: [0; 32],
                files: vec![file("photo.jpg"), file("photo (2).jpg")],
            },
            DupeGroup {
                hash: [1; 32],
                files: vec![file("invoice.pdf"), file("receipt.pdf")],
            },
        ];
        assert_eq!(filter_by_name_pattern(&groups, false).len(), 2);
        assert_eq!(filter_by_name_pattern(&groups, true).len(), 1);
    }
}
