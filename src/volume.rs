use std::path::Path;

/// Identity of the drive a reverse-search index entry was captured from: the
/// drive letter it was mounted as at index time (e.g. `"D:"`) and its Windows
/// volume label (e.g. `"500GB-5"`). Both are persisted rather than just the
/// full path, since a drive letter can be reassigned across reboots but the
/// label travels with the physical drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    pub drive_letter: String,
    pub label: String,
}

/// The drive-letter component of `path` (e.g. `"D:"` for `"D:\Photos\a.jpg"`),
/// or an empty string for paths with none (UNC paths, relative paths). A pure
/// string operation — no filesystem or OS calls — so it's cheap enough to call
/// per file during a scan.
pub fn drive_letter_of(path: &Path) -> String {
    match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => prefix
            .as_os_str()
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string(),
        _ => String::new(),
    }
}

/// Looks up the Windows volume label for `drive_letter` (e.g. `"D:"` ->
/// `"500GB-5"`), falling back to the drive letter itself if the volume is
/// unlabeled or the lookup fails (e.g. the drive isn't ready). This is the
/// one OS call in this module — callers should look it up once per drive
/// letter and cache it, not per file.
#[cfg(windows)]
pub fn volume_info(drive_letter: &str) -> VolumeInfo {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

    let root = format!("{drive_letter}\\");
    let root_wide: Vec<u16> = OsStr::new(&root).encode_wide().chain(Some(0)).collect();
    let mut name_buf = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            root_wide.as_ptr(),
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    let label = if ok != 0 {
        let len = name_buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&name_buf[..len])
    } else {
        String::new()
    };
    VolumeInfo {
        drive_letter: drive_letter.to_string(),
        label: if label.is_empty() {
            drive_letter.to_string()
        } else {
            label
        },
    }
}

#[cfg(not(windows))]
pub fn volume_info(drive_letter: &str) -> VolumeInfo {
    VolumeInfo {
        drive_letter: drive_letter.to_string(),
        label: drive_letter.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extracts_drive_letter_from_windows_path() {
        assert_eq!(drive_letter_of(&PathBuf::from(r"D:\Photos\a.jpg")), "D:");
    }

    #[test]
    fn returns_empty_string_for_relative_path() {
        assert_eq!(drive_letter_of(&PathBuf::from("Photos/a.jpg")), "");
    }
}
