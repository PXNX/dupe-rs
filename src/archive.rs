//! Recognising RAR archives and split-archive volumes.
//!
//! Volumes of a split archive only work as a complete set in one folder.
//! When the same set exists in two places, a duplicate scan pairs the parts
//! up one by one, and deleting "the duplicate" of any single part silently
//! breaks one of the sets. Renaming one part on its own breaks it just the
//! same. This module tells such files apart so callers can leave them out
//! or treat a set as a unit.

/// Splits a file name into `(set stem, volume suffix)` if it's a RAR archive
/// or a volume of a split archive, e.g.
/// - `movie.part01.rar` -> `("movie", ".part01.rar")`
/// - `movie.rar` -> `("movie", ".rar")`
/// - `movie.r00` -> `("movie", ".r00")`
/// - `backup.7z.001` -> `("backup", ".7z.001")`
/// - `backup.z01` -> `("backup", ".z01")`
///
/// Every volume of one set shares the same stem, so renaming all of them by
/// inserting the same text between stem and suffix keeps the set intact.
pub fn split_archive_name(name: &str) -> Option<(&str, &str)> {
    let lower = name.to_ascii_lowercase();

    // `<stem>.partN.rar`
    if let Some(before) = lower.strip_suffix(".rar") {
        if let Some(dot) = before.rfind('.') {
            let part = &before[dot + 1..];
            if part.len() > 4
                && part.starts_with("part")
                && part[4..].bytes().all(|b| b.is_ascii_digit())
            {
                return Some((&name[..dot], &name[dot..]));
            }
        }
        let stem_len = before.len();
        return (stem_len > 0).then(|| (&name[..stem_len], &name[stem_len..]));
    }

    let dot = lower.rfind('.')?;
    let (stem_end, ext) = (dot, &lower[dot + 1..]);
    if stem_end == 0 {
        return None;
    }
    // Old-style RAR volumes `.r00`..`.r999`, and split ZIP volumes `.z01`...
    let numbered = |prefix: u8| {
        ext.len() >= 3
            && ext.as_bytes()[0] == prefix
            && ext[1..].bytes().all(|b| b.is_ascii_digit())
    };
    if numbered(b'r') || numbered(b'z') {
        return Some((&name[..stem_end], &name[stem_end..]));
    }
    // `<stem>.7z.001`, `<stem>.zip.001`, `<stem>.rar.001`
    if ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()) {
        let before = &lower[..dot];
        for inner in [".7z", ".zip", ".rar"] {
            if before.ends_with(inner) && before.len() > inner.len() {
                let stem_len = before.len() - inner.len();
                return Some((&name[..stem_len], &name[stem_len..]));
            }
        }
    }
    None
}

/// Whether a file is a RAR archive or a split-archive volume.
pub fn is_archive_part(name: &str) -> bool {
    split_archive_name(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_rar_and_split_volumes() {
        assert_eq!(split_archive_name("Movie.part01.rar"), Some(("Movie", ".part01.rar")));
        assert_eq!(split_archive_name("movie.rar"), Some(("movie", ".rar")));
        assert_eq!(split_archive_name("movie.R00"), Some(("movie", ".R00")));
        assert_eq!(split_archive_name("movie.r123"), Some(("movie", ".r123")));
        assert_eq!(split_archive_name("backup.7z.001"), Some(("backup", ".7z.001")));
        assert_eq!(split_archive_name("backup.zip.002"), Some(("backup", ".zip.002")));
        assert_eq!(split_archive_name("backup.z01"), Some(("backup", ".z01")));
    }

    #[test]
    fn ignores_ordinary_files() {
        for name in [
            "photo.jpg",
            "notes.txt",
            "archive.zip",
            "song.r",
            "data.001",
            "readme",
            ".rar",
            "report.rtf",
            "movie.part.rar.txt",
        ] {
            assert!(!is_archive_part(name), "{name}");
        }
    }
}
