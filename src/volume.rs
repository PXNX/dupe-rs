use std::path::Path;

/// Identity of the drive a reverse-search index entry was captured from: the
/// drive letter it was mounted as at index time (e.g. `"D:"`), its Windows
/// volume label (e.g. `"500GB-5"`), and its volume serial number. The volume
/// is identified by label + serial rather than the letter, since USB drives
/// get whatever letter is free when they're plugged in. The label alone isn't
/// enough either (mirrored drives are often labeled alike), and the serial
/// alone isn't (a sector-by-sector clone copies it), but the pair is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    pub drive_letter: String,
    pub label: String,
    /// `None` if the volume couldn't be queried.
    pub serial: Option<u32>,
}

impl VolumeInfo {
    /// The key this volume is stored under in the index and drive registry.
    pub fn key(&self) -> String {
        volume_key(&self.drive_letter, &self.label, self.serial)
    }
}

/// Key identifying a volume: label + serial, or — for entries indexed before
/// serials were recorded — letter + label, which is how those were told apart.
pub fn volume_key(drive_letter: &str, label: &str, serial: Option<u32>) -> String {
    match serial {
        Some(serial) => format!("{}|{label}", format_serial(serial)),
        None => format!("{drive_letter}|{label}"),
    }
}

/// Formats a volume serial number the way `vol` and Explorer show it, e.g.
/// `"1A2B-3C4D"`.
pub fn format_serial(serial: u32) -> String {
    format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF)
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

/// Looks up the Windows volume label and serial for `drive_letter` (e.g.
/// `"D:"` -> `"500GB-5"`), falling back to the drive letter itself as the
/// label if the volume is unlabeled or the lookup fails (e.g. the drive isn't
/// ready). This is the one OS call in this module — callers should look it
/// up once per drive letter and cache it, not per file.
pub fn volume_info(drive_letter: &str) -> VolumeInfo {
    try_volume_info(drive_letter).unwrap_or_else(|| VolumeInfo {
        drive_letter: drive_letter.to_string(),
        label: drive_letter.to_string(),
        serial: None,
    })
}

/// Like `volume_info`, but `None` if the volume can't be queried at all
/// (e.g. an empty card reader slot).
#[cfg(windows)]
pub fn try_volume_info(drive_letter: &str) -> Option<VolumeInfo> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

    let root = format!("{drive_letter}\\");
    let root_wide: Vec<u16> = OsStr::new(&root).encode_wide().chain(Some(0)).collect();
    let mut name_buf = [0u16; 261];
    let mut serial = 0u32;
    // SAFETY: valid NUL-terminated path, buffer length matches, out-pointer
    // to a local; the unused outputs are null, which the API allows.
    let ok = unsafe {
        GetVolumeInformationW(
            root_wide.as_ptr(),
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = name_buf.iter().position(|&c| c == 0).unwrap_or(0);
    let label = String::from_utf16_lossy(&name_buf[..len]);
    Some(VolumeInfo {
        drive_letter: drive_letter.to_string(),
        label: if label.is_empty() {
            drive_letter.to_string()
        } else {
            label
        },
        serial: Some(serial),
    })
}

#[cfg(not(windows))]
pub fn try_volume_info(_drive_letter: &str) -> Option<VolumeInfo> {
    None
}

/// Letters of the local fixed and removable drives currently mounted (USB
/// hard drives count as one or the other depending on the adapter). Network
/// shares, optical drives and RAM disks are left out. Cheap: no disk I/O.
#[cfg(windows)]
pub fn local_drive_letters() -> Vec<String> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;

    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:", (b'A' + i) as char))
        .filter(|letter| {
            let root: Vec<u16> = format!("{letter}\\").encode_utf16().chain(Some(0)).collect();
            // SAFETY: valid NUL-terminated path.
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            kind == DRIVE_REMOVABLE || kind == DRIVE_FIXED
        })
        .collect()
}

#[cfg(not(windows))]
pub fn local_drive_letters() -> Vec<String> {
    Vec::new()
}

/// Free/total space of the drive holding a directory, plus its cluster
/// (allocation unit) size, which is what file sizes round up to on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpace {
    /// Free bytes available to this user (honours disk quotas).
    pub free: u64,
    pub total: u64,
    pub cluster: u64,
}

/// Looks up `DiskSpace` for the drive `dir` lives on, or `None` if it can't
/// be queried (e.g. the directory doesn't exist).
#[cfg(windows)]
pub fn disk_space(dir: &Path) -> Option<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDiskFreeSpaceW};

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let dir_wide = wide(dir.as_os_str());
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: valid NUL-terminated path and out-pointers to locals.
    if unsafe { GetDiskFreeSpaceExW(dir_wide.as_ptr(), &mut free, &mut total, &mut total_free) }
        == 0
    {
        return None;
    }

    let drive = drive_letter_of(dir);
    let mut cluster = 4096;
    if !drive.is_empty() {
        let root = wide(std::ffi::OsStr::new(&format!("{drive}\\")));
        let (mut sectors, mut bytes, mut free_clusters, mut clusters) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: as above.
        let ok = unsafe {
            GetDiskFreeSpaceW(root.as_ptr(), &mut sectors, &mut bytes, &mut free_clusters, &mut clusters)
        };
        if ok != 0 && sectors > 0 && bytes > 0 {
            cluster = sectors as u64 * bytes as u64;
        }
    }
    Some(DiskSpace { free, total, cluster })
}

#[cfg(not(windows))]
pub fn disk_space(_dir: &Path) -> Option<DiskSpace> {
    None
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
    fn formats_serials_like_windows_does() {
        assert_eq!(format_serial(0x1A2B_3C4D), "1A2B-3C4D");
        assert_eq!(format_serial(0x0000_00FF), "0000-00FF");
    }

    #[test]
    fn volume_key_prefers_serial_over_letter() {
        assert_eq!(volume_key("D:", "data", Some(0xABCD_0001)), "ABCD-0001|data");
        assert_eq!(volume_key("D:", "data", None), "D:|data");
    }

    #[test]
    fn returns_empty_string_for_relative_path() {
        assert_eq!(drive_letter_of(&PathBuf::from("Photos/a.jpg")), "");
    }
}
