use chrono::NaiveDate;
use serde_json::json;
use technogym::dates::{add_days, from_partition_key, parse_iso, to_iso, to_partition_key};

#[test]
fn iso_dates_are_strict() {
    assert_eq!(
        parse_iso("2026-09-01").unwrap(),
        NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()
    );
    for bad in [
        "2026-13-01",
        "2026-02-30",
        "2026-9-1",
        "20260901",
        "2026-09-01T00:00",
        "",
    ] {
        assert!(parse_iso(bad).is_err(), "{bad}");
    }
}

#[test]
fn partition_keys_round_trip() {
    let d = parse_iso("2026-09-03").unwrap();
    assert_eq!(to_partition_key(d), "20260903");
    assert_eq!(from_partition_key(&json!(20_260_903)), "2026-09-03");
    assert_eq!(from_partition_key(&json!("20260903")), "2026-09-03");
    assert_eq!(from_partition_key(&json!("yesterday")), "yesterday");
    assert_eq!(from_partition_key(&json!(null)), "");
    assert_eq!(to_iso(add_days(d, -3)), "2026-08-31");
}
