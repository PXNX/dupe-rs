use crate::model::{DupeGroup, SimilarGroup};

/// Aggregate numbers derived from the current duplicate groups, shown in the
/// statistics section so the user can see scan results at a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub group_count: usize,
    pub total_file_count: usize,
    pub duplicate_file_count: usize,
    pub total_bytes: u64,
    pub wasted_bytes: u64,
}

/// `group.files[0]` is always the "original"; every other file in the group is
/// a reclaimable duplicate, so wasted space is each duplicate's own size.
pub fn compute(groups: &[DupeGroup]) -> ScanStats {
    let mut stats = ScanStats::default();
    for group in groups {
        stats.group_count += 1;
        stats.total_file_count += group.files.len();
        for file in &group.files {
            stats.total_bytes += file.size;
        }
        for file in &group.files[1..] {
            stats.duplicate_file_count += 1;
            stats.wasted_bytes += file.size;
        }
    }
    stats
}

/// Same as `compute`, for `ScanMode::SimilarMedia` results.
pub fn compute_similar(groups: &[SimilarGroup]) -> ScanStats {
    let mut stats = ScanStats::default();
    for group in groups {
        stats.group_count += 1;
        stats.total_file_count += group.files.len();
        for file in &group.files {
            stats.total_bytes += file.size;
        }
        for file in &group.files[1..] {
            stats.duplicate_file_count += 1;
            stats.wasted_bytes += file.size;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileEntry;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn file(name: &str, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(name),
            size,
            created: SystemTime::UNIX_EPOCH,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn computes_wasted_space_as_sum_of_non_original_files() {
        let groups = vec![
            DupeGroup {
                hash: [0; 32],
                files: vec![file("a", 100), file("b", 100), file("c", 100)],
            },
            DupeGroup {
                hash: [1; 32],
                files: vec![file("d", 50), file("e", 50)],
            },
        ];
        let stats = compute(&groups);
        assert_eq!(stats.group_count, 2);
        assert_eq!(stats.total_file_count, 5);
        assert_eq!(stats.duplicate_file_count, 3);
        assert_eq!(stats.total_bytes, 400);
        assert_eq!(stats.wasted_bytes, 250);
    }

    #[test]
    fn empty_groups_produce_zeroed_stats() {
        assert_eq!(compute(&[]), ScanStats::default());
    }
}
