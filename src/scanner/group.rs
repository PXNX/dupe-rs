use crate::model::FileEntry;
use std::collections::HashMap;
use std::path::PathBuf;

/// Buckets files by size *and* lowercase extension as a cheap pre-filter before
/// any hashing happens. Two files of the same size but different extensions
/// are treated as certainly-not-duplicates and never even partial-hashed
/// against each other, which meaningfully shrinks the hash-pass workload on
/// trees with lots of same-sized-but-different-type files (thumbnails, log
/// rotations, etc.) — at the cost of no longer detecting a duplicate that was
/// renamed to a different extension.
pub fn group_by_size(files: Vec<FileEntry>) -> HashMap<(u64, Option<String>), Vec<FileEntry>> {
    let mut map: HashMap<(u64, Option<String>), Vec<FileEntry>> = HashMap::new();
    for f in files {
        let ext = f
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        map.entry((f.size, ext)).or_default().push(f);
    }
    map
}

/// Splits an already-confirmed duplicate group by parent directory, for the
/// "only mark files in the same folder" mode. Singleton subgroups (the file's
/// only copy left after splitting) are dropped since they're no longer dupes.
pub fn split_by_parent(files: Vec<FileEntry>) -> Vec<Vec<FileEntry>> {
    let mut map: HashMap<Option<PathBuf>, Vec<FileEntry>> = HashMap::new();
    for f in files {
        let parent = f.path.parent().map(|p| p.to_path_buf());
        map.entry(parent).or_default().push(f);
    }
    map.into_values().filter(|v| v.len() > 1).collect()
}

/// Sorts a duplicate group in place so the "original" ends up at index 0:
/// oldest `modified` time first, then shortest filename as a tiebreaker.
pub fn sort_group_original(files: &mut [FileEntry]) {
    files.sort_by(|a, b| {
        a.modified.cmp(&b.modified).then_with(|| {
            let a_len = a.path.file_name().map_or(0, |n| n.len());
            let b_len = b.path.file_name().map_or(0, |n| n.len());
            a_len.cmp(&b_len)
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn entry(name: &str, modified_offset_secs: u64) -> FileEntry {
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(modified_offset_secs);
        FileEntry {
            path: PathBuf::from(name),
            size: 100,
            created: modified,
            modified,
        }
    }

    #[test]
    fn group_by_size_buckets_by_size() {
        let mut small = entry("a", 0);
        small.size = 50;
        let files = vec![small, entry("b", 0), entry("c", 1)];
        let groups = group_by_size(files);
        assert_eq!(groups.get(&(50, None)).unwrap().len(), 1);
        assert_eq!(groups.get(&(100, None)).unwrap().len(), 2);
    }

    #[test]
    fn group_by_size_separates_same_size_different_extensions() {
        let mut jpg = entry("photo.jpg", 0);
        jpg.size = 100;
        let mut png = entry("photo.png", 0);
        png.size = 100;
        let groups = group_by_size(vec![jpg, png]);
        assert_eq!(
            groups.len(),
            2,
            "same-size files with different extensions must not share a bucket"
        );
    }

    #[test]
    fn oldest_modified_wins_as_original() {
        let mut files = vec![entry("newer.txt", 10), entry("older.txt", 5)];
        sort_group_original(&mut files);
        assert_eq!(files[0].path, PathBuf::from("older.txt"));
    }

    #[test]
    fn shortest_filename_breaks_tie_on_modified() {
        let mut files = vec![entry("longer_name.txt", 5), entry("short.txt", 5)];
        sort_group_original(&mut files);
        assert_eq!(files[0].path, PathBuf::from("short.txt"));
    }

    #[test]
    fn split_by_parent_keeps_only_folders_with_multiple_copies() {
        let files = vec![
            entry("dir_a/one.txt", 0),
            entry("dir_a/two.txt", 0),
            entry("dir_b/lonely.txt", 0),
        ];
        let mut subgroups = split_by_parent(files);
        assert_eq!(subgroups.len(), 1);
        let group = subgroups.remove(0);
        assert_eq!(group.len(), 2);
        assert!(group.iter().all(|f| f.path.starts_with("dir_a")));
    }
}
