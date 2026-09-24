use chrono::{DateTime, Local};
use std::time::SystemTime;

pub fn format_timestamp(t: SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn hex_prefix(hash: &[u8; 32]) -> String {
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}
