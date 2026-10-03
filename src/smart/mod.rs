//! Drive health from S.M.A.R.T. data.
//!
//! Reading SMART means sending raw ATA commands to the disk, which Windows
//! only allows administrators to do. Rather than running the whole app
//! elevated, a health check re-launches this executable as a short-lived
//! elevated helper (one UAC prompt per check, see `elevate`) that reads the
//! requested drives and writes the results to a JSON file for the app to
//! pick up.
//!
//! The commands go out as SAT (SCSI / ATA Translation) ATA PASS-THROUGH
//! CDBs, which is what lets them through most SATA-to-USB adapters (the
//! JMicron and ASMedia bridges in common enclosures and docks understand
//! them); internal drives fall back to the classic `SMART_RCV_DRIVE_DATA`
//! IOCTL. Some cheap bridges pass neither through — those drives simply
//! report that SMART isn't available.

mod ata;
mod elevate;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use ata::device_identity;

/// Command-line flag that makes the executable run as the elevated helper
/// instead of opening the window: `--read-smart <out.json> D: E: ...`.
pub const HELPER_FLAG: &str = "--read-smart";

/// One attribute from the drive's SMART data table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartAttribute {
    pub id: u8,
    /// Pre-failure attribute: dropping to its threshold predicts imminent
    /// failure (old-age attributes only indicate wear).
    pub prefail: bool,
    /// Normalized value (usually 1-253, higher is better).
    pub current: u8,
    pub worst: u8,
    /// Normalized failure threshold; 0 if the drive didn't report one.
    pub threshold: u8,
    /// The 48-bit vendor raw value; see `SmartAttribute::value`.
    pub raw: u64,
}

impl SmartAttribute {
    /// The raw value as a human-readable count. Several vendors pack extra
    /// data into the upper bytes of some attributes (e.g. min/max
    /// temperature, or milliseconds next to power-on hours), which is masked
    /// off here the way smartctl does by default.
    pub fn value(&self) -> u64 {
        match self.id {
            TEMPERATURE | AIRFLOW_TEMPERATURE => self.raw & 0xFF,
            POWER_ON_HOURS | REALLOCATED | REALLOCATION_EVENTS | PENDING | UNCORRECTABLE => {
                self.raw & 0xFFFF_FFFF
            }
            _ => self.raw,
        }
    }

    pub fn name(&self) -> &'static str {
        attribute_name(self.id)
    }

    /// The normalized value has reached the drive's own failure threshold.
    pub fn below_threshold(&self) -> bool {
        // 0 means "always passing"; 0xFE/0xFF are placeholders some drives
        // report for attributes that aren't meant to be checked.
        self.threshold != 0 && self.threshold < 0xFE && self.current <= self.threshold
    }
}

pub const REALLOCATED: u8 = 5;
pub const POWER_ON_HOURS: u8 = 9;
pub const POWER_CYCLES: u8 = 12;
pub const REPORTED_UNCORRECTABLE: u8 = 187;
pub const AIRFLOW_TEMPERATURE: u8 = 190;
pub const TEMPERATURE: u8 = 194;
pub const REALLOCATION_EVENTS: u8 = 196;
pub const PENDING: u8 = 197;
pub const UNCORRECTABLE: u8 = 198;
pub const CRC_ERRORS: u8 = 199;

/// Common names of the standard attributes; vendor-specific IDs that aren't
/// widely agreed on show as "Vendor specific".
pub fn attribute_name(id: u8) -> &'static str {
    match id {
        1 => "Raw read error rate",
        2 => "Throughput performance",
        3 => "Spin-up time",
        4 => "Start/stop count",
        REALLOCATED => "Reallocated sectors",
        7 => "Seek error rate",
        8 => "Seek time performance",
        POWER_ON_HOURS => "Power-on hours",
        10 => "Spin retry count",
        11 => "Calibration retry count",
        POWER_CYCLES => "Power cycle count",
        183 => "Runtime bad blocks",
        184 => "End-to-end errors",
        REPORTED_UNCORRECTABLE => "Reported uncorrectable errors",
        188 => "Command timeouts",
        189 => "High fly writes",
        AIRFLOW_TEMPERATURE => "Airflow temperature",
        191 => "G-sense error rate",
        192 => "Power-off retract count",
        193 => "Load cycle count",
        TEMPERATURE => "Temperature",
        195 => "Hardware ECC recovered",
        REALLOCATION_EVENTS => "Reallocation events",
        PENDING => "Pending sectors",
        UNCORRECTABLE => "Offline uncorrectable sectors",
        CRC_ERRORS => "Interface CRC errors",
        200 => "Write error rate",
        220 => "Disk shift",
        222 => "Loaded hours",
        223 => "Load retry count",
        224 => "Load friction",
        225 => "Load/unload cycle count",
        226 => "Load-in time",
        240 => "Head flying hours",
        241 => "Total LBAs written",
        242 => "Total LBAs read",
        _ => "Vendor specific",
    }
}

/// What the drive says about itself, from ATA IDENTIFY DEVICE (with admin
/// rights) or the storage driver's device descriptor (without). Through a
/// USB adapter the descriptor sometimes names the adapter rather than the
/// disk; IDENTIFY always names the disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub model: String,
    pub serial: String,
    pub firmware: String,
    /// How it's attached, e.g. "USB" or "SATA".
    pub bus: String,
}

/// A successful SMART read of one drive.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SmartReport {
    pub identity: Option<DeviceIdentity>,
    pub attributes: Vec<SmartAttribute>,
}

/// The helper's result for one requested drive letter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DriveReading {
    pub letter: String,
    pub result: Result<SmartReport, String>,
}

/// Reads SMART data for each of `letters`, asking for administrator rights
/// first if this process doesn't have them. Blocks until done (and, when
/// elevating, until the user answers the UAC prompt), so call it off the UI
/// thread. `Err` means nothing could be read at all, e.g. the prompt was
/// declined.
pub fn read_health(letters: &[String]) -> Result<Vec<DriveReading>, String> {
    if elevate::is_elevated() {
        Ok(read_all(letters))
    } else {
        elevate::run_helper(letters)
    }
}

fn read_all(letters: &[String]) -> Vec<DriveReading> {
    letters
        .iter()
        .map(|letter| DriveReading {
            letter: letter.clone(),
            result: ata::read_smart(letter),
        })
        .collect()
}

/// If the process was started as the elevated helper (see `HELPER_FLAG`),
/// does the helper's work and returns its exit code; otherwise `None`, and
/// the app starts normally.
pub fn run_helper_from_args() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(HELPER_FLAG) {
        return None;
    }
    let out = PathBuf::from(args.next()?);
    let letters: Vec<String> = args.filter(|a| is_drive_letter(a)).collect();
    let json = serde_json::to_vec(&read_all(&letters)).unwrap_or_default();
    Some(if std::fs::write(&out, json).is_ok() {
        0
    } else {
        1
    })
}

/// `"D:"`-style arguments only, so the elevated helper can't be pointed at
/// anything else.
fn is_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Overall verdict on a drive's SMART data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HealthStatus {
    Good,
    /// Worth watching: sectors have been remapped, errors logged, etc.
    Caution,
    /// A pre-failure attribute reached its threshold: copy the data off.
    Failing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment {
    pub status: HealthStatus,
    /// Why, one line each; may also hold informational notes for a `Good`
    /// drive.
    pub notes: Vec<String>,
}

/// Temperature at or above which a drive is called out as running hot.
const HOT_CELSIUS: u64 = 55;

/// Judges `current` SMART attributes, comparing against `previous` (the
/// drive's last check, if any) to call out counters that went up since.
pub fn assess(current: &[SmartAttribute], previous: Option<&[SmartAttribute]>) -> Assessment {
    let mut status = HealthStatus::Good;
    let mut notes = Vec::new();
    let mut flag = |level: HealthStatus, note: String| {
        status = status.max(level);
        notes.push(note);
    };
    let value = |attrs: &[SmartAttribute], id: u8| {
        attrs.iter().find(|a| a.id == id).map(SmartAttribute::value)
    };

    for a in current.iter().filter(|a| a.below_threshold()) {
        if a.prefail {
            flag(
                HealthStatus::Failing,
                format!("{} has reached the drive's failure threshold", a.name()),
            );
        } else {
            flag(
                HealthStatus::Caution,
                format!("{} has reached its threshold (wear)", a.name()),
            );
        }
    }

    let counters = [
        (REALLOCATED, "reallocated sector(s)"),
        (PENDING, "sector(s) waiting to be reallocated"),
        (UNCORRECTABLE, "uncorrectable sector(s)"),
        (REPORTED_UNCORRECTABLE, "uncorrectable error(s) reported"),
    ];
    for (id, what) in counters {
        let Some(now) = value(current, id).filter(|&n| n > 0) else {
            continue;
        };
        match previous.and_then(|p| value(p, id)) {
            Some(before) if now > before => flag(
                HealthStatus::Caution,
                format!("{now} {what}, up from {before} at the last check"),
            ),
            _ => flag(HealthStatus::Caution, format!("{now} {what}")),
        }
    }

    if let Some(crc) = value(current, CRC_ERRORS).filter(|&n| n > 0) {
        match previous.and_then(|p| value(p, CRC_ERRORS)) {
            Some(before) if crc > before => flag(
                HealthStatus::Caution,
                format!(
                    "Interface CRC errors went up from {before} to {crc}: check the cable or \
                     USB adapter"
                ),
            ),
            _ => flag(
                HealthStatus::Good,
                format!(
                    "{crc} interface CRC error(s) logged (usually a cable or adapter issue, not \
                 the disk)"
                ),
            ),
        }
    }

    if let Some(t) = temperature(current).filter(|&t| t >= HOT_CELSIUS) {
        flag(HealthStatus::Caution, format!("Running hot: {t} °C"));
    }

    Assessment { status, notes }
}

/// Drive temperature in °C, if reported.
pub fn temperature(attrs: &[SmartAttribute]) -> Option<u64> {
    [TEMPERATURE, AIRFLOW_TEMPERATURE]
        .iter()
        .find_map(|id| attrs.iter().find(|a| a.id == *id))
        .map(SmartAttribute::value)
        .filter(|&t| t > 0 && t < 120)
}

/// Parses the 512-byte SMART READ DATA sector, filling in thresholds from the
/// SMART READ THRESHOLDS sector when there is one. Both list up to 30
/// 12-byte entries starting at offset 2; empty slots have ID 0.
pub fn parse_attributes(data: &[u8; 512], thresholds: Option<&[u8; 512]>) -> Vec<SmartAttribute> {
    (0..30)
        .map(|i| 2 + i * 12)
        .filter(|&off| data[off] != 0)
        .map(|off| {
            let id = data[off];
            let flags = u16::from_le_bytes([data[off + 1], data[off + 2]]);
            let mut raw = [0u8; 8];
            raw[..6].copy_from_slice(&data[off + 5..off + 11]);
            let threshold = thresholds
                .and_then(|t| {
                    (0..30)
                        .map(|j| 2 + j * 12)
                        .find(|&toff| t[toff] == id)
                        .map(|toff| t[toff + 1])
                })
                .unwrap_or(0);
            SmartAttribute {
                id,
                prefail: flags & 1 != 0,
                current: data[off + 3],
                worst: data[off + 4],
                threshold,
                raw: u64::from_le_bytes(raw),
            }
        })
        .collect()
}

/// Model, serial and firmware from a 512-byte ATA IDENTIFY DEVICE response,
/// or `None` if it doesn't look like one. Its strings are stored as 16-bit
/// words with the two characters of each word swapped.
pub fn parse_identify(data: &[u8; 512]) -> Option<DeviceIdentity> {
    let string = |first_word: usize, last_word: usize| {
        let mut bytes = Vec::with_capacity((last_word - first_word + 1) * 2);
        for w in first_word..=last_word {
            bytes.push(data[w * 2 + 1]);
            bytes.push(data[w * 2]);
        }
        String::from_utf8_lossy(&bytes)
            .trim_matches(|c: char| c.is_whitespace() || c == '\0')
            .to_string()
    };
    // Word 0 bit 15 set means "not an ATA device".
    if data[1] & 0x80 != 0 {
        return None;
    }
    let model = string(27, 46);
    if model.is_empty() {
        return None;
    }
    Some(DeviceIdentity {
        model,
        serial: string(10, 19),
        firmware: string(23, 26),
        bus: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(id: u8, prefail: bool, current: u8, threshold: u8, raw: u64) -> SmartAttribute {
        SmartAttribute {
            id,
            prefail,
            current,
            worst: current,
            threshold,
            raw,
        }
    }

    fn put_entry(buf: &mut [u8; 512], slot: usize, id: u8, flags: u16, current: u8, raw: u64) {
        let off = 2 + slot * 12;
        buf[off] = id;
        buf[off + 1..off + 3].copy_from_slice(&flags.to_le_bytes());
        buf[off + 3] = current;
        buf[off + 4] = current - 1;
        buf[off + 5..off + 11].copy_from_slice(&raw.to_le_bytes()[..6]);
    }

    #[test]
    fn parses_attribute_table_and_thresholds() {
        let mut data = [0u8; 512];
        put_entry(&mut data, 0, REALLOCATED, 0x0033, 100, 8);
        put_entry(
            &mut data,
            1,
            POWER_ON_HOURS,
            0x0032,
            90,
            0x0000_1234_0000_2710,
        );
        put_entry(&mut data, 3, TEMPERATURE, 0x0022, 64, 0x0028_0012_0024);
        let mut thresholds = [0u8; 512];
        thresholds[2] = REALLOCATED;
        thresholds[3] = 36;

        let attrs = parse_attributes(&data, Some(&thresholds));

        assert_eq!(attrs.len(), 3, "empty slots are skipped");
        assert_eq!(attrs[0], attr(REALLOCATED, true, 100, 36, 8).with_worst(99));
        assert!(!attrs[1].prefail);
        assert_eq!(
            attrs[1].value(),
            0x2710,
            "upper bytes of power-on hours are masked off"
        );
        assert_eq!(
            attrs[2].value(),
            0x24,
            "only the low byte is the current temperature"
        );
        assert_eq!(attrs[2].threshold, 0, "no threshold entry for it");
    }

    impl SmartAttribute {
        fn with_worst(mut self, worst: u8) -> Self {
            self.worst = worst;
            self
        }
    }

    #[test]
    fn parses_identify_strings_with_swapped_bytes() {
        let mut data = [0u8; 512];
        let put = |data: &mut [u8; 512], first_word: usize, s: &str, words: usize| {
            let mut padded = s.as_bytes().to_vec();
            padded.resize(words * 2, b' ');
            for (i, pair) in padded.chunks(2).enumerate() {
                data[(first_word + i) * 2] = pair[1];
                data[(first_word + i) * 2 + 1] = pair[0];
            }
        };
        put(&mut data, 10, "WD-WCC4N1234567", 10);
        put(&mut data, 23, "82.00A82", 4);
        put(&mut data, 27, "WDC WD40EFRX-68N32N0", 20);

        let id = parse_identify(&data).unwrap();
        assert_eq!(id.model, "WDC WD40EFRX-68N32N0");
        assert_eq!(id.serial, "WD-WCC4N1234567");
        assert_eq!(id.firmware, "82.00A82");

        data[1] = 0x80;
        assert!(parse_identify(&data).is_none(), "not an ATA device");
    }

    #[test]
    fn healthy_drive_is_good() {
        let attrs = [
            attr(REALLOCATED, true, 200, 140, 0),
            attr(PENDING, false, 200, 0, 0),
            attr(TEMPERATURE, false, 110, 0, 33),
        ];
        let a = assess(&attrs, None);
        assert_eq!(a.status, HealthStatus::Good);
        assert!(a.notes.is_empty());
    }

    #[test]
    fn prefail_attribute_at_threshold_is_failing() {
        let attrs = [attr(REALLOCATED, true, 140, 140, 2000)];
        assert_eq!(assess(&attrs, None).status, HealthStatus::Failing);
    }

    #[test]
    fn old_age_attribute_at_threshold_is_only_caution() {
        let attrs = [attr(193, false, 1, 1, 600_000)];
        assert_eq!(assess(&attrs, None).status, HealthStatus::Caution);
    }

    #[test]
    fn placeholder_thresholds_are_ignored() {
        let attrs = [attr(1, true, 100, 0xFE, 0)];
        assert_eq!(assess(&attrs, None).status, HealthStatus::Good);
    }

    #[test]
    fn reallocated_sectors_are_caution_and_growth_is_called_out() {
        let before = [attr(REALLOCATED, true, 200, 140, 8)];
        let now = [attr(REALLOCATED, true, 199, 140, 24)];
        let a = assess(&now, Some(&before));
        assert_eq!(a.status, HealthStatus::Caution);
        assert_eq!(
            a.notes,
            vec!["24 reallocated sector(s), up from 8 at the last check"]
        );
    }

    #[test]
    fn crc_errors_alone_are_a_note_until_they_grow() {
        let before = [attr(CRC_ERRORS, false, 200, 0, 3)];
        let a = assess(&before, None);
        assert_eq!(a.status, HealthStatus::Good);
        assert_eq!(a.notes.len(), 1);

        let now = [attr(CRC_ERRORS, false, 200, 0, 9)];
        assert_eq!(assess(&now, Some(&before)).status, HealthStatus::Caution);
    }

    #[test]
    fn hot_drive_is_caution() {
        let attrs = [attr(TEMPERATURE, false, 50, 0, 0x003C_0014_003A)];
        assert_eq!(temperature(&attrs), Some(58));
        assert_eq!(assess(&attrs, None).status, HealthStatus::Caution);
    }

    #[test]
    fn only_drive_letters_are_accepted_by_the_helper() {
        assert!(is_drive_letter("E:"));
        assert!(!is_drive_letter("E:\\"));
        assert!(!is_drive_letter("\\\\.\\PhysicalDrive0"));
    }
}
