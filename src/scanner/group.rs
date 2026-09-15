use crate::model::FileEntry;
use std::collections::HashMap;

pub fn group_by_size(files: Vec<FileEntry>) -> HashMap<u64, Vec<FileEntry>> {
    let mut map: HashMap<u64, Vec<FileEntry>> = HashMap::new();
    for f in files {
        map.entry(f.size).or_default().push(f);
    }
    map
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
        FileEntry {
            path: PathBuf::from(name),
            size: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(modified_offset_secs),
        }
    }

    #[test]
    fn group_by_size_buckets_by_size() {
        let mut small = entry("a", 0);
        small.size = 50;
        let files = vec![small, entry("b", 0), entry("c", 1)];
        let groups = group_by_size(files);
        assert_eq!(groups.get(&50).unwrap().len(), 1);
        assert_eq!(groups.get(&100).unwrap().len(), 2);
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
}
