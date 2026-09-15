use crate::model::{DupeGroup, FileEntry};

/// Single source of truth for which files are currently "visible" given the
/// `only_show_duplicates` filter (which hides each group's original at index 0).
/// Shared by row rendering, Ctrl+A, and the status bar's "Shown" total so they
/// never disagree with each other.
pub fn compute_visible_entries(
    groups: &[DupeGroup],
    only_show_duplicates: bool,
) -> Vec<&FileEntry> {
    groups
        .iter()
        .flat_map(|g| {
            if only_show_duplicates {
                g.files[1..].iter()
            } else {
                g.files[..].iter()
            }
        })
        .collect()
}
