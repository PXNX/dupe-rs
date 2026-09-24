use crate::app::{SortColumn, SortDirection};
use crate::model::DupeGroup;
use crate::selection::group_looks_like_name_copy;
use std::path::PathBuf;
use std::time::SystemTime;

/// One flattened, display-ready row for the exact-duplicates table and grid
/// views: pre-filtered (name-copy filter), pre-sorted, and with
/// `only_show_duplicates` already applied.
///
/// Both views, plus the status bar's totals, used to redo this filter/sort
/// work from scratch on every single frame (including every frame of a
/// scroll or hover), which made browsing large scan results laggy — a scan
/// spanning a large drive can produce hundreds of thousands of rows, and
/// resorting all of them 60 times a second is not free. `ExactRowsCache`
/// rebuilds this list only when something it actually depends on changes.
#[derive(Clone)]
pub struct RowInfo {
    pub path: PathBuf,
    pub size: u64,
    pub created: SystemTime,
    pub modified: SystemTime,
    pub hash: [u8; 32],
    pub group_idx: usize,
    pub is_original: bool,
    pub is_group_start: bool,
    pub is_last_in_group: bool,
    pub is_largest_size: bool,
    pub is_oldest_created: bool,
    pub is_oldest_modified: bool,
}

#[derive(PartialEq, Clone, Copy)]
struct CacheKey {
    generation: u64,
    name_filter: bool,
    only_dup: bool,
    sort: Option<(SortColumn, SortDirection)>,
}

#[derive(Default)]
pub struct ExactRowsCache {
    key: Option<CacheKey>,
    pub rows: Vec<RowInfo>,
}

impl ExactRowsCache {
    /// Rebuilds `rows` if `groups` or any of the view settings changed since
    /// the last call (tracked via `generation`, which the caller bumps
    /// whenever `groups` is mutated); otherwise this is a cheap no-op.
    pub fn refresh(
        &mut self,
        groups: &[DupeGroup],
        generation: u64,
        name_filter: bool,
        only_dup: bool,
        sort: Option<(SortColumn, SortDirection)>,
    ) {
        let key = CacheKey {
            generation,
            name_filter,
            only_dup,
            sort,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.rows = build_rows(groups, name_filter, only_dup, sort);
    }
}

fn build_rows(
    groups: &[DupeGroup],
    name_filter: bool,
    only_dup: bool,
    sort: Option<(SortColumn, SortDirection)>,
) -> Vec<RowInfo> {
    let mut indices: Vec<usize> = groups
        .iter()
        .enumerate()
        .filter(|(_, g)| !name_filter || group_looks_like_name_copy(g))
        .map(|(i, _)| i)
        .collect();

    if let Some((column, direction)) = sort {
        indices.sort_by(|&a, &b| {
            let a = &groups[a].files[0];
            let b = &groups[b].files[0];
            let ord = match column {
                SortColumn::Filename => {
                    let a_name = a
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let b_name = b
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    a_name.cmp(&b_name)
                }
                SortColumn::Path => a.path.cmp(&b.path),
                SortColumn::Size => a.size.cmp(&b.size),
                SortColumn::Created => a.created.cmp(&b.created),
                SortColumn::Modified => a.modified.cmp(&b.modified),
            };
            if direction == SortDirection::Desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }

    let skip = if only_dup { 1 } else { 0 };
    let mut rows = Vec::new();
    for &group_idx in &indices {
        let group = &groups[group_idx];
        let winner = |key: fn(&crate::model::FileEntry) -> _| {
            group
                .files
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| key(f))
                .map(|(i, _)| i)
                .unwrap_or(0)
        };
        let largest_size = group
            .files
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| f.size)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let oldest_created = winner(|f| f.created);
        let oldest_modified = winner(|f| f.modified);
        let last_idx = group.files.len().saturating_sub(1);

        for (file_idx, file) in group.files.iter().enumerate().skip(skip) {
            rows.push(RowInfo {
                path: file.path.clone(),
                size: file.size,
                created: file.created,
                modified: file.modified,
                hash: group.hash,
                group_idx,
                is_original: file_idx == 0,
                is_group_start: file_idx == skip,
                is_last_in_group: file_idx == last_idx,
                is_largest_size: file_idx == largest_size,
                is_oldest_created: file_idx == oldest_created,
                is_oldest_modified: file_idx == oldest_modified,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn file(name: &str, size: u64, modified_offset_secs: u64) -> crate::model::FileEntry {
        crate::model::FileEntry {
            path: PathBuf::from(name),
            size,
            created: SystemTime::UNIX_EPOCH + Duration::from_secs(modified_offset_secs),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(modified_offset_secs),
        }
    }

    #[test]
    fn skips_originals_when_only_dup_is_set() {
        let groups = vec![DupeGroup {
            hash: [0; 32],
            files: vec![file("a.txt", 10, 0), file("b.txt", 10, 1)],
        }];
        let rows = build_rows(&groups, false, true, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, PathBuf::from("b.txt"));
    }

    #[test]
    fn marks_last_file_in_each_group() {
        let groups = vec![DupeGroup {
            hash: [0; 32],
            files: vec![file("a.txt", 10, 0), file("b.txt", 10, 1), file("c.txt", 10, 2)],
        }];
        let rows = build_rows(&groups, false, false, None);
        assert!(!rows[0].is_last_in_group);
        assert!(!rows[1].is_last_in_group);
        assert!(rows[2].is_last_in_group);
    }

    #[test]
    fn sorts_groups_by_size_using_the_original_file() {
        let groups = vec![
            DupeGroup {
                hash: [0; 32],
                files: vec![file("big.txt", 100, 0), file("big2.txt", 100, 1)],
            },
            DupeGroup {
                hash: [1; 32],
                files: vec![file("small.txt", 10, 0), file("small2.txt", 10, 1)],
            },
        ];
        let rows = build_rows(&groups, false, false, Some((SortColumn::Size, SortDirection::Asc)));
        assert_eq!(rows[0].path, PathBuf::from("small.txt"));
        assert_eq!(rows[2].path, PathBuf::from("big.txt"));
    }

    #[test]
    fn filters_by_name_pattern_when_enabled() {
        let groups = vec![
            DupeGroup {
                hash: [0; 32],
                files: vec![file("photo.jpg", 10, 0), file("photo (2).jpg", 10, 1)],
            },
            DupeGroup {
                hash: [1; 32],
                files: vec![file("invoice.pdf", 10, 0), file("receipt.pdf", 10, 1)],
            },
        ];
        let rows = build_rows(&groups, true, false, None);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.group_idx == 0));
    }
}
