//! Raw ATA commands to a disk via Windows storage IOCTLs.

use super::{DeviceIdentity, SmartReport};

/// The ATA SMART command and its sub-commands (the ATA "features" field).
const ATA_SMART: u8 = 0xB0;
const SMART_READ_DATA: u8 = 0xD0;
const SMART_READ_THRESHOLDS: u8 = 0xD1;
const ATA_IDENTIFY_DEVICE: u8 = 0xEC;
/// SMART commands only run with this signature in the LBA mid/high fields.
const SMART_LBA_MID: u8 = 0x4F;
const SMART_LBA_HIGH: u8 = 0xC2;

/// The ways of getting an ATA command to the disk, tried in this order.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Route {
    /// SAT ATA PASS-THROUGH (16): most USB bridges, many SATA controllers.
    Sat16,
    /// SAT ATA PASS-THROUGH (12): some older USB bridges only take this one.
    Sat12,
    /// `SMART_RCV_DRIVE_DATA`: internal drives on the standard AHCI driver.
    Legacy,
}

const ROUTES: [Route; 3] = [Route::Sat16, Route::Sat12, Route::Legacy];

/// ATA PASS-THROUGH (16) CDB for a PIO data-in command returning one
/// 512-byte sector.
#[cfg_attr(not(windows), allow(dead_code))]
fn cdb16(command: u8, features: u8, lba_mid: u8, lba_high: u8) -> [u8; 16] {
    [
        0x85,
        4 << 1, // protocol: PIO data-in
        0x0E,   // T_DIR=from device, BYT_BLOK=blocks, T_LENGTH=sector count
        0,
        features,
        0,
        1, // sector count
        0,
        0, // LBA low
        0,
        lba_mid,
        0,
        lba_high,
        0xA0, // device
        command,
        0,
    ]
}

/// ATA PASS-THROUGH (12) CDB, same command as `cdb16`.
#[cfg_attr(not(windows), allow(dead_code))]
fn cdb12(command: u8, features: u8, lba_mid: u8, lba_high: u8) -> [u8; 12] {
    [
        0xA1,
        4 << 1,
        0x0E,
        features,
        1,
        0,
        lba_mid,
        lba_high,
        0xA0,
        command,
        0,
        0,
    ]
}

/// Reads IDENTIFY DEVICE, SMART data and thresholds from the disk holding
/// `letter` (e.g. `"E:"`). Needs administrator rights.
pub fn read_smart(letter: &str) -> Result<SmartReport, String> {
    let disk = win::Disk::open_for(letter)?;
    let mut last_err = String::from("SMART isn't available on this drive");
    for route in ROUTES {
        let data = match disk.command(
            route,
            ATA_SMART,
            SMART_READ_DATA,
            SMART_LBA_MID,
            SMART_LBA_HIGH,
        ) {
            Ok(data) if data[2..].chunks(12).take(30).any(|e| e[0] != 0) => data,
            // Some bridges "succeed" and hand back zeros.
            Ok(_) => continue,
            Err(err) => {
                last_err = err;
                continue;
            }
        };
        let thresholds = disk
            .command(
                route,
                ATA_SMART,
                SMART_READ_THRESHOLDS,
                SMART_LBA_MID,
                SMART_LBA_HIGH,
            )
            .ok();
        let identity = disk
            .command(route, ATA_IDENTIFY_DEVICE, 0, 0, 0)
            .ok()
            .and_then(|d| super::parse_identify(&d))
            .map(|mut id| {
                id.bus = disk.bus.clone();
                id
            });
        return Ok(SmartReport {
            identity,
            attributes: super::parse_attributes(&data, thresholds.as_ref()),
        });
    }
    Err(format!(
        "{last_err}. The USB adapter may not pass SMART commands through."
    ))
}

/// Model, serial and bus as reported by the storage driver, for the disk
/// holding `letter`. Needs no special rights and doesn't touch the disk.
pub fn device_identity(letter: &str) -> Option<DeviceIdentity> {
    win::query_identity(letter)
}

/// Turns a `STORAGE_BUS_TYPE` into a short name.
#[cfg_attr(not(windows), allow(dead_code))]
fn bus_name(bus: i32) -> &'static str {
    match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "FireWire",
        6 => "Fibre Channel",
        7 => "USB",
        8 => "RAID",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        15 => "File-backed",
        16 => "Storage Spaces",
        17 => "NVMe",
        _ => "Unknown",
    }
}

#[cfg(windows)]
mod win {
    use super::{DeviceIdentity, Route, bus_name, cdb12, cdb16};
    use std::ffi::c_void;
    use std::mem::{offset_of, size_of, zeroed};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::Storage::IscsiDisc::{
        IOCTL_SCSI_PASS_THROUGH, SCSI_IOCTL_DATA_IN, SCSI_PASS_THROUGH,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        SMART_RCV_DRIVE_DATA, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
    };

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful CreateFileW.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(path: &str, access: u32) -> Result<Handle, String> {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: valid NUL-terminated path; null security attributes and
        // template are allowed.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            Err(format!(
                "Couldn't open {path}: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Handle(h))
        }
    }

    /// Sends `code` with `buf` as both input and output (the convention of
    /// every IOCTL used here).
    fn ioctl<T>(h: &Handle, code: u32, input: &T, output: &mut [u8]) -> Result<(), String> {
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for the lengths passed.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                code,
                input as *const T as *const c_void,
                size_of::<T>() as u32,
                output.as_mut_ptr() as *mut c_void,
                output.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    }

    fn volume_path(letter: &str) -> String {
        format!("\\\\.\\{letter}")
    }

    /// The physical disk (`\\.\PhysicalDriveN`) holding the volume `letter`.
    fn disk_number(letter: &str) -> Result<u32, String> {
        // Access 0: only device metadata is queried.
        let volume = open(&volume_path(letter), 0)?;
        let mut out = [0u8; size_of::<STORAGE_DEVICE_NUMBER>()];
        ioctl(&volume, IOCTL_STORAGE_GET_DEVICE_NUMBER, &(), &mut out)
            .map_err(|e| format!("Couldn't find the disk behind {letter}: {e}"))?;
        // SAFETY: `out` holds a STORAGE_DEVICE_NUMBER written by the driver.
        let number: STORAGE_DEVICE_NUMBER =
            unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
        Ok(number.DeviceNumber)
    }

    pub fn query_identity(letter: &str) -> Option<DeviceIdentity> {
        let h = open(&volume_path(letter), 0).ok()?;
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut out = [0u8; 1024];
        ioctl(&h, IOCTL_STORAGE_QUERY_PROPERTY, &query, &mut out).ok()?;
        Some(parse_descriptor(&out))
    }

    /// Reads a `STORAGE_DEVICE_DESCRIPTOR` by offset: its strings follow the
    /// fixed fields at offsets the fields point to (0 = absent).
    fn parse_descriptor(buf: &[u8]) -> DeviceIdentity {
        let u32_at =
            |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        let string_at = |off: usize| {
            let start = u32_at(off);
            if start == 0 || start >= buf.len() {
                return String::new();
            }
            let end = buf[start..]
                .iter()
                .position(|&b| b == 0)
                .map_or(buf.len(), |n| start + n);
            String::from_utf8_lossy(&buf[start..end]).trim().to_string()
        };
        let vendor = string_at(12);
        let product = string_at(16);
        DeviceIdentity {
            model: [vendor, product]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            firmware: string_at(20),
            serial: string_at(24),
            bus: bus_name(i32::from_le_bytes(buf[28..32].try_into().unwrap())).to_string(),
        }
    }

    pub struct Disk {
        handle: Handle,
        number: u32,
        pub bus: String,
    }

    /// A SCSI_PASS_THROUGH header with its sense and data buffers following
    /// it in the same allocation, as the buffered IOCTL expects.
    #[repr(C)]
    struct PassThrough {
        spt: SCSI_PASS_THROUGH,
        sense: [u8; 32],
        data: [u8; 512],
    }

    impl Disk {
        pub fn open_for(letter: &str) -> Result<Disk, String> {
            let number = disk_number(letter)?;
            let handle = open(
                &format!("\\\\.\\PhysicalDrive{number}"),
                GENERIC_READ | GENERIC_WRITE,
            )?;
            let bus = query_identity(letter).map(|id| id.bus).unwrap_or_default();
            Ok(Disk {
                handle,
                number,
                bus,
            })
        }

        pub fn command(
            &self,
            route: Route,
            command: u8,
            features: u8,
            lba_mid: u8,
            lba_high: u8,
        ) -> Result<[u8; 512], String> {
            match route {
                Route::Sat16 => self.pass_through(&cdb16(command, features, lba_mid, lba_high)),
                Route::Sat12 => self.pass_through(&cdb12(command, features, lba_mid, lba_high)),
                Route::Legacy => self.legacy(command, features, lba_mid, lba_high),
            }
        }

        fn pass_through(&self, cdb: &[u8]) -> Result<[u8; 512], String> {
            // SAFETY: plain-old-data; all-zero is a valid starting state.
            let mut buf: PassThrough = unsafe { zeroed() };
            buf.spt.Length = size_of::<SCSI_PASS_THROUGH>() as u16;
            buf.spt.CdbLength = cdb.len() as u8;
            buf.spt.SenseInfoLength = buf.sense.len() as u8;
            buf.spt.DataIn = SCSI_IOCTL_DATA_IN as u8;
            buf.spt.DataTransferLength = buf.data.len() as u32;
            buf.spt.TimeOutValue = 10;
            buf.spt.DataBufferOffset = offset_of!(PassThrough, data);
            buf.spt.SenseInfoOffset = offset_of!(PassThrough, sense) as u32;
            buf.spt.Cdb[..cdb.len()].copy_from_slice(cdb);

            let mut returned = 0u32;
            let ptr = &mut buf as *mut PassThrough as *mut c_void;
            let len = size_of::<PassThrough>() as u32;
            // SAFETY: `buf` is valid for `len` bytes as input and output.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle.0,
                    IOCTL_SCSI_PASS_THROUGH,
                    ptr,
                    len,
                    ptr,
                    len,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if buf.spt.ScsiStatus != 0 {
                return Err(format!(
                    "the drive rejected the command (SCSI status {:#04x})",
                    buf.spt.ScsiStatus
                ));
            }
            Ok(buf.data)
        }

        /// `SMART_RCV_DRIVE_DATA`, laid out by hand since `SENDCMDINPARAMS`
        /// is a packed struct: 4-byte buffer size, 8 IDE registers, drive
        /// number, 3 + 16 reserved bytes, then a 1-byte buffer stub.
        fn legacy(
            &self,
            command: u8,
            features: u8,
            lba_mid: u8,
            lba_high: u8,
        ) -> Result<[u8; 512], String> {
            let mut input = [0u8; 33];
            input[0..4].copy_from_slice(&512u32.to_le_bytes());
            input[4] = features;
            input[5] = 1; // sector count
            input[6] = 1; // sector number
            input[7] = lba_mid;
            input[8] = lba_high;
            input[9] = 0xA0 | (((self.number & 1) as u8) << 4);
            input[10] = command;
            input[12] = self.number as u8;
            // SENDCMDOUTPARAMS: 4-byte size, 12-byte DRIVERSTATUS, data.
            let mut output = [0u8; 16 + 512];
            ioctl(&self.handle, SMART_RCV_DRIVE_DATA, &input, &mut output)?;
            let mut data = [0u8; 512];
            data.copy_from_slice(&output[16..]);
            Ok(data)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_a_storage_device_descriptor() {
            let mut buf = [0u8; 128];
            buf[12..16].copy_from_slice(&64u32.to_le_bytes());
            buf[16..20].copy_from_slice(&72u32.to_le_bytes());
            buf[24..28].copy_from_slice(&96u32.to_le_bytes());
            buf[28..32].copy_from_slice(&7i32.to_le_bytes());
            buf[64..68].copy_from_slice(b"WDC ");
            buf[72..85].copy_from_slice(b"WD40EFRX-68N3");
            buf[96..104].copy_from_slice(b"57584431");

            let id = parse_descriptor(&buf);
            assert_eq!(id.model, "WDC WD40EFRX-68N3");
            assert_eq!(id.serial, "57584431");
            assert_eq!(id.firmware, "");
            assert_eq!(id.bus, "USB");
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::{DeviceIdentity, Route};

    pub struct Disk {
        pub bus: String,
    }

    impl Disk {
        pub fn open_for(_letter: &str) -> Result<Disk, String> {
            Err("Reading SMART data is only supported on Windows".into())
        }

        pub fn command(&self, _: Route, _: u8, _: u8, _: u8, _: u8) -> Result<[u8; 512], String> {
            unreachable!()
        }
    }

    pub fn query_identity(_letter: &str) -> Option<DeviceIdentity> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_through_cdbs_carry_the_smart_signature() {
        let c = cdb16(ATA_SMART, SMART_READ_DATA, SMART_LBA_MID, SMART_LBA_HIGH);
        assert_eq!(
            (c[0], c[4], c[10], c[12], c[14]),
            (0x85, 0xD0, 0x4F, 0xC2, 0xB0)
        );
        let c = cdb12(ATA_SMART, SMART_READ_DATA, SMART_LBA_MID, SMART_LBA_HIGH);
        assert_eq!(
            (c[0], c[3], c[6], c[7], c[9]),
            (0xA1, 0xD0, 0x4F, 0xC2, 0xB0)
        );
    }
}
