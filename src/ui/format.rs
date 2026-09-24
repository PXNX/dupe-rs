use chrono::{DateTime, Local};
use std::time::SystemTime;

pub fn format_timestamp(t: SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn hex_prefix(hash: &[u8; 32]) -> String {
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Formats a millisecond duration as `hh:mm:ss`, e.g. `00:11:40` instead of a
/// raw "700 seconds" style figure once a scan runs long.
pub fn format_duration_hms(elapsed_ms: u128) -> String {
    let total_secs = elapsed_ms / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_long_duration_as_hh_mm_ss() {
        assert_eq!(format_duration_hms(700_000), "00:11:40");
    }

    #[test]
    fn formats_an_hour_plus_duration() {
        assert_eq!(format_duration_hms(3_661_000), "01:01:01");
    }

    #[test]
    fn formats_a_sub_second_duration() {
        assert_eq!(format_duration_hms(400), "00:00:00");
    }
}
