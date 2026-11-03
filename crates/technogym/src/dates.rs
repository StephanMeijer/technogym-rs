//! `YYYY-MM-DD` in and out, and the API's `yyyyMMdd` partition keys.

use chrono::{Local, NaiveDate, TimeDelta};
use serde_json::Value;

use crate::error::{Error, Result};

/// Parses `YYYY-MM-DD` strictly: ten characters, real calendar day.
pub fn parse_iso(value: &str) -> Result<NaiveDate> {
    let b = value.as_bytes();
    let shaped = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shaped {
        return Err(Error::InvalidDate(value.to_string()));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| Error::InvalidDate(value.to_string()))
}

/// `YYYY-MM-DD`.
pub fn to_iso(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// The API's `yyyyMMdd` partition key.
pub fn to_partition_key(date: NaiveDate) -> String {
    date.format("%Y%m%d").to_string()
}

/// A `yyyyMMdd` partition key (string or number) to `YYYY-MM-DD`; anything else as text.
pub fn from_partition_key(value: &Value) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if s.len() == 8 && s.bytes().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..])
    } else {
        s
    }
}

/// Today in the local time zone: sessions are partitioned by the gym's day, and "today" means
/// the caller's today.
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// `date` moved by `days` calendar days (negative moves back).
pub fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    date.checked_add_signed(TimeDelta::days(days))
        .unwrap_or(date)
}
