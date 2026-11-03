#![expect(
    clippy::cast_precision_loss,
    reason = "test data: small integers as f64"
)]
use std::collections::BTreeMap;

use technogym::{
    AnalyticsSample, ExerciseAnalytics, HrSample, Lap, SegmentBy, SegmentChannel, SegmentMode,
    SegmentOptions, add_heart_rate, channel_stats, downsample, hr_stats, sample_every,
    segment_analytics, window_samples,
};

fn sample(t: i64, speed: f64, grade: f64, dist: f64) -> AnalyticsSample {
    AnalyticsSample {
        t,
        channels: BTreeMap::from([
            ("Speed".into(), Some(speed)),
            ("Grade".into(), Some(grade)),
            ("HDistance".into(), Some(dist)),
        ]),
    }
}

fn channels() -> Vec<String> {
    vec!["Speed".into(), "Grade".into(), "HDistance".into()]
}

/// 60 s at 10 km/h, a 2 s ramp, 60 s at 14 km/h, repeated once: four intervals.
fn interval_run() -> Vec<AnalyticsSample> {
    let mut out = vec![];
    let mut t = 1;
    let mut dist = 0.0;
    for _ in 0..2 {
        for (speed, secs) in [(10.0, 60), (12.0, 2), (14.0, 60), (12.0, 2)] {
            for _ in 0..secs {
                dist += speed / 3.6;
                out.push(sample(t, speed, 1.0, dist));
                t += 1;
            }
        }
    }
    out
}

#[test]
fn channel_stats_separate_measured_and_cumulative() {
    let samples = vec![
        sample(1, 0.0, 1.0, 0.0),
        sample(2, 10.0, 1.0, 3.0),
        sample(3, 12.0, 1.0, 6.0),
    ];
    let stats = channel_stats(&samples, &channels());
    assert_eq!(stats["Speed"].max, 12.0);
    assert_eq!(stats["Speed"].samples, 3);
    assert_eq!(
        stats["Speed"].avg,
        Some(11.0),
        "zero readings are left out of the average"
    );
    assert_eq!(stats["Speed"].min_nonzero, Some(10.0));
    assert_eq!(stats["HDistance"].final_value, Some(6.0));
    assert_eq!(stats["HDistance"].avg, None);
    assert!(!stats.contains_key("Power"));
}

#[test]
fn intervals_are_detected_from_setpoint_changes_and_ramps_fold_forward() {
    let samples = interval_run();
    let (mode, segments) = segment_analytics(&samples, &channels(), &[], SegmentOptions::default());
    assert_eq!(mode, SegmentMode::Workload);
    assert_eq!(segments.len(), 4);
    assert_eq!(segments[0].interval, Some(1));
    assert_eq!(segments[0].setpoint.as_ref().unwrap()["Speed"], 10.0);
    assert_eq!(segments[0].start_sec, 0);
    assert_eq!(segments[0].end_sec, 60);
    assert_eq!(segments[1].setpoint.as_ref().unwrap()["Speed"], 14.0);
    assert_eq!(
        segments[1].start_sec, 60,
        "the ramp belongs to the interval it leads into"
    );
    assert_eq!(segments[1].end_sec, 122);
    assert_eq!(segments[1].duration_sec, 62);
    assert_eq!(
        segments[3].end_sec, 248,
        "a trailing ramp extends the last interval"
    );
    assert_eq!(segments[0].pace_min_per_km, Some(6.0));
    match &segments[1].channels["HDistance"] {
        SegmentChannel::Cumulative(d) => assert!(
            (d.delta - (2.0 * 12.0 / 3.6 + 60.0 * 14.0 / 3.6)).abs() < 0.01,
            "every second of the segment, its first one included"
        ),
        SegmentChannel::Stats(_) => panic!("cumulative channel"),
    }
}

#[test]
fn a_steady_run_falls_back_to_laps_then_fixed_windows() {
    let samples: Vec<_> = (1..=700).map(|t| sample(t, 10.0, 1.0, t as f64)).collect();
    let laps = vec![
        Lap {
            n: Some(1),
            lap_distance_m: Some(1000.0),
            total_time_sec: Some(360.0),
            ..Lap::default()
        },
        Lap {
            n: Some(2),
            lap_distance_m: Some(1000.0),
            total_time_sec: Some(700.0),
            ..Lap::default()
        },
        Lap {
            n: Some(3),
            total_time_sec: None,
            ..Lap::default()
        },
    ];
    let (mode, segments) =
        segment_analytics(&samples, &channels(), &laps, SegmentOptions::default());
    assert_eq!(mode, SegmentMode::Laps);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].lap, Some(1));
    assert_eq!((segments[0].start_sec, segments[0].end_sec), (0, 360));
    assert_eq!((segments[1].start_sec, segments[1].end_sec), (360, 700));

    let (mode, segments) = segment_analytics(&samples, &channels(), &[], SegmentOptions::default());
    assert_eq!(mode, SegmentMode::Fixed);
    assert_eq!(segments.len(), 3, "300 s windows over 700 s");
    assert_eq!((segments[2].start_sec, segments[2].end_sec), (600, 700));

    let opts = SegmentOptions {
        segment_by: SegmentBy::Fixed,
        segment_seconds: Some(3),
    };
    let (_, segments) = segment_analytics(&samples, &channels(), &laps, opts);
    assert_eq!(segments.len(), 70, "windows are at least 10 s");

    let opts = SegmentOptions {
        segment_by: SegmentBy::Laps,
        ..SegmentOptions::default()
    };
    let (mode, segments) = segment_analytics(&samples, &channels(), &[], opts);
    assert_eq!(mode, SegmentMode::Fixed, "no laps: fixed windows");
    assert_eq!(segments.len(), 3);
}

#[test]
fn empty_samples_give_no_segments() {
    let (mode, segments) = segment_analytics(&[], &channels(), &[], SegmentOptions::default());
    assert_eq!(mode, SegmentMode::Fixed);
    assert_eq!(segments.len(), 0);
}

#[test]
fn window_and_downsample() {
    let hr: Vec<_> = (0..10).map(|t| HrSample { t, hr: 100 + t }).collect();
    assert_eq!(
        window_samples(&hr, 3, 5)
            .iter()
            .map(|h| h.t)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert_eq!(
        window_samples(&hr, -1, 0).len(),
        10,
        "to_sec 0 means no upper bound"
    );
    assert_eq!(
        downsample(&hr, 4).iter().map(|h| h.t).collect::<Vec<_>>(),
        vec![0, 4, 8]
    );
    assert_eq!(downsample(&hr, 0).len(), 10);
    assert_eq!(
        sample_every(&hr, 4).iter().map(|h| h.t).collect::<Vec<_>>(),
        vec![0, 4, 8],
        "on 1 Hz rows, seconds and rows agree"
    );
}

/// Change-driven rows: a reading only when something changes.
fn sparse(rows: &[(i64, i64)]) -> Vec<HrSample> {
    rows.iter().map(|&(t, hr)| HrSample { t, hr }).collect()
}

#[test]
fn sampling_and_windows_go_by_seconds_on_change_driven_rows() {
    let hr = sparse(&[(0, 100), (1, 101), (2, 102), (30, 130), (31, 131)]);
    assert_eq!(
        sample_every(&hr, 10)
            .iter()
            .map(|h| h.t)
            .collect::<Vec<_>>(),
        vec![0, 2, 30],
        "t=10 and t=20 are still the reading from t=2, sent once"
    );
    assert_eq!(
        window_samples(&hr, 10, 30)
            .iter()
            .map(|h| h.t)
            .collect::<Vec<_>>(),
        vec![2, 30],
        "the reading in effect at from_sec comes along"
    );
    assert_eq!(sample_every(&hr, usize::MAX).len(), 1);
}

#[test]
fn averages_and_distances_hold_values_until_the_next_row() {
    // 10 km/h for 90 s, then 20 km/h for 10 s: rows only at the changes.
    let samples = vec![
        sample(1, 10.0, 0.0, 2.8),
        sample(91, 20.0, 0.0, 250.0),
        sample(100, 20.0, 0.0, 300.0),
    ];
    let stats = channel_stats(&samples, &channels());
    assert_eq!(stats["Speed"].seconds, 100);
    assert_eq!(
        stats["Speed"].avg,
        Some(11.0),
        "90 s at 10 and 10 s at 20, not the row mean of 16.7"
    );

    // 40 s windows: the first has one row, the second none at all.
    let opts = SegmentOptions {
        segment_by: SegmentBy::Fixed,
        segment_seconds: Some(40),
    };
    let (_, segments) = segment_analytics(&samples, &channels(), &[], opts);
    assert_eq!(
        segments.len(),
        3,
        "a window with no row still has a held speed"
    );
    assert_eq!(segments[1].pace_min_per_km, Some(6.0));
    let total: f64 = segments
        .iter()
        .map(|s| match &s.channels["HDistance"] {
            SegmentChannel::Cumulative(d) => d.delta,
            SegmentChannel::Stats(_) => panic!("cumulative channel"),
        })
        .sum();
    assert!(
        (total - 300.0).abs() < 0.05,
        "segments add up to the whole distance, got {total}"
    );
}

#[test]
fn an_interval_lasts_until_the_next_setpoint_change() {
    // Two rows per interval: the setpoint changes at 1, 61 and 121.
    let samples = vec![
        sample(1, 10.0, 0.0, 3.0),
        sample(2, 10.0, 0.0, 6.0),
        sample(61, 14.0, 0.0, 170.0),
        sample(62, 14.0, 0.0, 174.0),
        sample(121, 10.0, 0.0, 400.0),
        sample(180, 10.0, 0.0, 560.0),
    ];
    let (mode, segments) = segment_analytics(&samples, &channels(), &[], SegmentOptions::default());
    assert_eq!(
        mode,
        SegmentMode::Workload,
        "not folded away as short ramps"
    );
    assert_eq!(
        segments
            .iter()
            .map(|s| (s.start_sec, s.end_sec))
            .collect::<Vec<_>>(),
        vec![(0, 60), (60, 120), (120, 180)]
    );
}

#[test]
fn segment_by_parses_case_insensitively() {
    assert_eq!("LAPS".parse::<SegmentBy>().unwrap(), SegmentBy::Laps);
    assert!("sometimes".parse::<SegmentBy>().is_err());
}

#[test]
fn heart_rate_holds_at_most_five_seconds_and_reports_coverage() {
    // 100 bpm at 0..10 (1 Hz), then the strap drops out until 30, then 150 bpm for 10 s.
    let mut rows: Vec<(i64, i64)> = (0..10).map(|t| (t, 100)).collect();
    rows.extend((30..40).map(|t| (t, 150)));
    let hr = sparse(&rows);
    let s = hr_stats(&hr, 0, 40).unwrap();
    assert_eq!((s.min, s.max), (100, 150));
    // Covered: 0..9 (9 s), 9..14 (5 s, held at most 5 s), 30..40 (10 s) = 24 of 40.
    assert_eq!(s.coverage, 0.6);
    assert_eq!(s.avg, 120.8, "(14 * 100 + 10 * 150) / 24");
    assert_eq!(hr_stats(&hr, 15, 30), None, "no reading in the gap");

    let analytics = ExerciseAnalytics {
        samples: vec![sample(1, 10.0, 0.0, 3.0), sample(40, 10.0, 0.0, 111.0)],
        hr,
        ..ExerciseAnalytics::default()
    };
    let opts = SegmentOptions {
        segment_by: SegmentBy::Fixed,
        segment_seconds: Some(20),
    };
    let (_, mut segments) = segment_analytics(&analytics.samples, &channels(), &[], opts);
    let whole = add_heart_rate(&analytics, &mut segments).unwrap();
    assert_eq!(whole.max, 150);
    assert_eq!(segments[0].hr.as_ref().unwrap().max, 100);
    assert_eq!(segments[1].hr.as_ref().unwrap().min, 150);
}
