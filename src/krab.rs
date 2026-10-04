//! Leftovers of the GandCrab v4 ransomware: `name.ext.KRAB` files that sit
//! next to (or elsewhere than) an unencrypted copy of `name.ext`. Without the
//! ransom note they can't be decrypted, so once the original is found the
//! encrypted copy is dead weight. They never match their original by content
//! hash, so the exact-duplicate scan pairs them up by name and size instead.

use crate::model::{DupeGroup, FileEntry};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// What GandCrab v4 appends to every file it encrypts: the RSA-2048-wrapped
/// Salsa20 key (256 bytes) and nonce (256 bytes), then the original file size
/// as a little-endian `u64`.
pub const FOOTER_LEN: u64 = 256 + 256 + 8;

/// True if `path` has the `.krab` extension (any case).
pub fn is_krab(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("krab"))
}

/// The file name the encrypted file had before GandCrab appended `.KRAB`,
/// lowercased for lookups.
fn original_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = &name[..name.len().checked_sub(".krab".len())?];
    (!stem.is_empty()).then(|| stem.to_lowercase())
}

/// Reads the original size from the GandCrab footer of a file that is `size`
/// bytes long. `None` if the footer is unreadable or doesn't add up, i.e. the
/// file isn't a GandCrab v4 file after all.
fn footer_original_size(path: &Path, size: u64) -> Option<u64> {
    if size < FOOTER_LEN {
        return None;
    }
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::End(-8)).ok()?;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf).ok()?;
    let original = u64::from_le_bytes(buf);
    (original.checked_add(FOOTER_LEN) == Some(size)).then_some(original)
}

/// Pairs every `.KRAB` file in `files` with an unencrypted original: same
/// name without `.KRAB` (case-insensitive) and exactly the size recorded in
/// the encrypted file's footer. Each original becomes a group with its
/// `.KRAB` copies as the duplicates. Returns those groups plus the files left
/// for the content-hash pass: everything except the matched `.KRAB` files.
///
/// With `same_folder_only`, the original has to be in the same folder.
pub fn match_leftovers(files: Vec<FileEntry>, same_folder_only: bool) -> (Vec<DupeGroup>, Vec<FileEntry>) {
    let (krabs, rest): (Vec<FileEntry>, Vec<FileEntry>) = files.into_iter().partition(|f| is_krab(&f.path));
    if krabs.is_empty() {
        return (Vec::new(), rest);
    }

    let mut by_name_size: HashMap<(String, u64), Vec<&FileEntry>> = HashMap::new();
    for f in &rest {
        if let Some(name) = f.path.file_name().and_then(|n| n.to_str()) {
            by_name_size.entry((name.to_lowercase(), f.size)).or_default().push(f);
        }
    }

    // Only files whose name has a candidate original are opened to read the
    // footer.
    let matches: Vec<Option<PathBuf>> = krabs
        .par_iter()
        .map(|krab| {
            let name = original_name(&krab.path)?;
            let size = krab.size.checked_sub(FOOTER_LEN)?;
            let candidates = by_name_size.get(&(name, size))?;
            if footer_original_size(&krab.path, krab.size)? != size {
                return None;
            }
            let mut candidates: Vec<FileEntry> = candidates
                .iter()
                .filter(|c| !same_folder_only || c.path.parent() == krab.path.parent())
                .map(|c| (*c).clone())
                .collect();
            crate::scanner::sort_group_original(&mut candidates);
            candidates.into_iter().next().map(|c| c.path)
        })
        .collect();

    let originals: HashMap<&Path, &FileEntry> = rest.iter().map(|f| (f.path.as_path(), f)).collect();
    let mut groups: HashMap<PathBuf, DupeGroup> = HashMap::new();
    let mut unmatched = Vec::new();
    for (krab, original) in krabs.into_iter().zip(matches) {
        let Some(original) = original else {
            unmatched.push(krab);
            continue;
        };
        groups
            .entry(original.clone())
            .or_insert_with(|| DupeGroup {
                // Not a content hash (the files differ), just a stable,
                // per-original key for the group.
                hash: *blake3::hash(original.as_os_str().as_encoded_bytes()).as_bytes(),
                files: vec![originals[original.as_path()].clone()],
            })
            .files
            .push(krab);
    }

    let mut rest = rest;
    rest.extend(unmatched);
    (groups.into_values().collect(), rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;
    use tempfile::tempdir;

    fn entry(path: PathBuf) -> FileEntry {
        let size = fs::metadata(&path).unwrap().len();
        FileEntry {
            path,
            size,
            created: SystemTime::UNIX_EPOCH,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    /// Writes a fake GandCrab v4 file: `len` bytes of "ciphertext", 512 bytes
    /// of wrapped key and nonce, and `footer_size` as the recorded size.
    fn write_krab(path: &Path, len: usize, footer_size: u64) {
        let mut data = vec![0xA5u8; len + 512];
        data.extend_from_slice(&footer_size.to_le_bytes());
        fs::write(path, data).unwrap();
    }

    #[test]
    fn detects_krab_extension_in_any_case() {
        assert!(is_krab(Path::new("clip.mp4.KRAB")));
        assert!(is_krab(Path::new("clip.mp4.KRAb")));
        assert!(!is_krab(Path::new("clip.mp4")));
        assert!(!is_krab(Path::new("krab")));
    }

    #[test]
    fn pairs_krab_file_with_original_elsewhere() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::create_dir(dir.path().join("b")).unwrap();
        let original = dir.path().join("a/Clip.mp4");
        let krab = dir.path().join("b/clip.mp4.KRAb");
        fs::write(&original, vec![1u8; 300]).unwrap();
        write_krab(&krab, 300, 300);

        let (groups, rest) = match_leftovers(vec![entry(original.clone()), entry(krab.clone())], false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files[0].path, original);
        assert_eq!(groups[0].files[1].path, krab);
        assert_eq!(rest.len(), 1, "the original still takes part in the hash pass");
        assert_eq!(rest[0].path, original);
    }

    #[test]
    fn leaves_krab_files_without_a_matching_original() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("photo.jpg");
        fs::write(&original, vec![1u8; 300]).unwrap();
        // Original is one byte bigger than what was encrypted.
        let off_by_one = dir.path().join("photo.jpg.KRAB");
        write_krab(&off_by_one, 299, 299);
        // Size adds up, but the footer disagrees: not a GandCrab file.
        let bad_footer = dir.path().join("sub_photo.jpg.KRAB");
        write_krab(&bad_footer, 300, 12345);
        // No original at all.
        let orphan = dir.path().join("lost.docx.KRAB");
        write_krab(&orphan, 50, 50);

        let files = vec![entry(original), entry(off_by_one), entry(bad_footer), entry(orphan)];
        let (groups, rest) = match_leftovers(files, false);
        assert!(groups.is_empty());
        assert_eq!(rest.len(), 4);
    }

    #[test]
    fn same_folder_only_requires_original_next_to_it() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        let original = dir.path().join("a/song.mp3");
        let krab = dir.path().join("song.mp3.KRAB");
        fs::write(&original, vec![1u8; 100]).unwrap();
        write_krab(&krab, 100, 100);

        let (groups, _) = match_leftovers(vec![entry(original), entry(krab)], true);
        assert!(groups.is_empty());
    }

    #[test]
    fn several_encrypted_copies_share_one_group() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("x")).unwrap();
        let original = dir.path().join("doc.pdf");
        fs::write(&original, vec![1u8; 64]).unwrap();
        write_krab(&dir.path().join("doc.pdf.KRAB"), 64, 64);
        write_krab(&dir.path().join("x/doc.pdf.KRAB"), 64, 64);

        let files = vec![
            entry(original.clone()),
            entry(dir.path().join("doc.pdf.KRAB")),
            entry(dir.path().join("x/doc.pdf.KRAB")),
        ];
        let (groups, _) = match_leftovers(files, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
        assert_eq!(groups[0].files[0].path, original);
    }
}
