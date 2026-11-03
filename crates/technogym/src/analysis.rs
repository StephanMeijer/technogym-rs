//! Derived views over [`ExerciseAnalytics::samples`](crate::types::ExerciseAnalytics): per-channel
//! stats and interval / lap / window segments. Pure functions, no HTTP. The mywellness summary
//! block omits most machine channels (`RunningCadence` and `RunningPower` on a Skillrun, for one),
//! so these fill that gap.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::{AnalyticsSample, HrSample, Lap};

/// Channels that accumulate over the exercise: only the final value means anything, not the
/// average.
pub const CUMULATIVE_CHANNELS: &[&str] =
    &["HDistance", "Distance", "VDistance", "Calories", "Move"];

/// Channels SET on the console rather than measured. A change in any of them starts a new
/// interval on a treadmill or bike programme.
pub const SETPOINT_CHANNELS: &[&str] = &[
    "Speed",
    "Grade",
    "Level",
    "Resistance",
    "Incline",
    "Rpm_Target",
];

/// Setpoint blocks shorter than this are ramps and are folded into the interval they lead into.
const MIN_INTERVAL_SEC: i64 = 10;

/// Aggregates of one channel. Measured channels get `avg` and `min_nonzero` over the non-zero
/// readings (the machine reports 0 while the belt is stopped); cumulative channels get `final`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStats {
    /// The largest reading.
    pub max: f64,
    /// How many numeric readings there were.
    pub samples: usize,
    /// Mean of the non-zero readings, one decimal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg: Option<f64>,
    /// Smallest non-zero reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_nonzero: Option<f64>,
    /// Last reading, on cumulative channels.
    #[serde(rename = "final", skip_serializing_if = "Option::is_none")]
    pub final_value: Option<f64>,
}

/// A cumulative channel within one segment: the amount covered in it, and its value at the end.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CumulativeDelta {
    /// Covered within the segment.
    pub delta: f64,
    /// Value at the segment's end.
    pub end: f64,
}

/// What a segment reports per channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SegmentChannel {
    /// A measured channel.
    Stats(ChannelStats),
    /// A cumulative channel.
    Cumulative(CumulativeDelta),
}

/// How an exercise was split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentMode {
    /// Intervals detected from setpoint changes.
    Workload,
    /// Console auto-laps.
    Laps,
    /// Fixed windows.
    Fixed,
}

/// How to split an exercise. `Auto`: workload intervals when there are at least two, else laps,
/// else fixed windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentBy {
    /// Intervals, else laps, else fixed windows.
    #[default]
    Auto,
    /// Intervals only.
    Workload,
    /// Laps only.
    Laps,
    /// Fixed windows only.
    Fixed,
}

impl std::str::FromStr for SegmentBy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "workload" => Ok(Self::Workload),
            "laps" => Ok(Self::Laps),
            "fixed" => Ok(Self::Fixed),
            _ => Err(format!(
                "segment_by must be auto, workload, laps or fixed, got {s:?}"
            )),
        }
    }
}

/// One segment of an exercise with every channel aggregated over it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Exclusive start: the segment covers `start_sec < t <= end_sec`.
    pub start_sec: i64,
    /// Inclusive end.
    pub end_sec: i64,
    /// `end_sec - start_sec`.
    pub duration_sec: i64,
    /// 1-based, on `workload` segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<usize>,
    /// The constant setpoint values of a `workload` segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setpoint: Option<BTreeMap<String, f64>>,
    /// On `laps` segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lap: Option<i64>,
    /// On `laps` segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lap_distance_m: Option<f64>,
    /// From the average `Speed` (km/h), when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pace_min_per_km: Option<f64>,
    /// Per-channel aggregates.
    pub channels: BTreeMap<String, SegmentChannel>,
}

/// Options for [`segment_analytics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentOptions {
    /// Default `Auto`.
    pub segment_by: SegmentBy,
    /// Window length for `Fixed`, at least 10. Default 300.
    pub segment_seconds: Option<i64>,
}

/// Rows that carry a time offset.
pub trait Timed {
    /// Seconds since the start of the exercise.
    fn t(&self) -> i64;
}

impl Timed for AnalyticsSample {
    fn t(&self) -> i64 {
        self.t
    }
}

impl Timed for HrSample {
    fn t(&self) -> i64 {
        self.t
    }
}

fn round(n: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (n * f).round() / f
}

fn numbers(samples: &[&AnalyticsSample], channel: &str) -> Vec<f64> {
    samples
        .iter()
        .filter_map(|row| row.channels.get(channel).copied().flatten())
        .collect()
}

fn is_cumulative(channel: &str) -> bool {
    CUMULATIVE_CHANNELS.contains(&channel)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "sample counts are far below 2^52"
)]
fn stats_of(samples: &[&AnalyticsSample], channels: &[String]) -> BTreeMap<String, ChannelStats> {
    let mut stats = BTreeMap::new();
    for ch in channels {
        let values = numbers(samples, ch);
        if values.is_empty() {
            continue;
        }
        let mut entry = ChannelStats {
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            samples: values.len(),
            ..ChannelStats::default()
        };
        if is_cumulative(ch) {
            entry.final_value = values.last().copied();
        } else {
            let nonzero: Vec<f64> = values.iter().copied().filter(|v| *v > 0.0).collect();
            entry.avg = Some(if nonzero.is_empty() {
                0.0
            } else {
                round(nonzero.iter().sum::<f64>() / nonzero.len() as f64, 1)
            });
            entry.min_nonzero = Some(nonzero.iter().copied().fold(f64::INFINITY, f64::min))
                .filter(|m| m.is_finite())
                .or(Some(0.0));
        }
        stats.insert(ch.clone(), entry);
    }
    stats
}

/// Per-channel aggregates over `samples`. Channels with no numeric reading are left out.
pub fn channel_stats(
    samples: &[AnalyticsSample],
    channels: &[String],
) -> BTreeMap<String, ChannelStats> {
    let refs: Vec<&AnalyticsSample> = samples.iter().collect();
    stats_of(&refs, channels)
}

struct Bound {
    start: i64,
    end: i64,
    interval: Option<usize>,
    setpoint: Option<BTreeMap<String, f64>>,
    lap: Option<i64>,
    lap_distance_m: Option<f64>,
}

impl Bound {
    fn plain(start: i64, end: i64) -> Self {
        Self {
            start,
            end,
            interval: None,
            setpoint: None,
            lap: None,
            lap_distance_m: None,
        }
    }
}

struct Block {
    /// Setpoint values in tenths, so equal setpoints compare exactly.
    sig: Vec<i64>,
    start: i64,
    end: i64,
}

/// Maximal runs of constant setpoint. The machine ramps between setpoints over a few seconds,
/// producing 1-2 s blocks; those merge into the following interval so it includes its own
/// acceleration. Fewer than two intervals is not an interval workout: returns `[]`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "setpoints in tenths round-trip exactly through i64"
)]
fn workload_bounds(samples: &[AnalyticsSample], channels: &[String]) -> Vec<Bound> {
    let keys: Vec<&str> = SETPOINT_CHANNELS
        .iter()
        .copied()
        .filter(|k| channels.iter().any(|c| c == k))
        .collect();
    if keys.is_empty() {
        return vec![];
    }
    let sig = |row: &AnalyticsSample| -> Vec<i64> {
        keys.iter()
            .map(|k| (row.channels.get(*k).copied().flatten().unwrap_or(0.0) * 10.0).round() as i64)
            .collect()
    };

    let mut blocks: Vec<Block> = vec![];
    for row in samples {
        let s = sig(row);
        match blocks.last_mut() {
            Some(last) if last.sig == s => last.end = row.t,
            _ => blocks.push(Block {
                sig: s,
                start: row.t,
                end: row.t,
            }),
        }
    }

    let mut merged: Vec<Block> = vec![];
    let mut carry_start: Option<i64> = None;
    for b in &blocks {
        if b.end - b.start + 1 < MIN_INTERVAL_SEC {
            carry_start.get_or_insert(b.start);
            continue;
        }
        merged.push(Block {
            sig: b.sig.clone(),
            start: carry_start.take().unwrap_or(b.start),
            end: b.end,
        });
    }
    if carry_start.is_some() {
        let last_end = blocks.last().map_or(0, |b| b.end);
        match merged.last_mut() {
            Some(last) => last.end = last_end,
            None => merged.push(Block {
                sig: blocks[0].sig.clone(),
                start: blocks[0].start,
                end: last_end,
            }),
        }
    }
    if merged.len() < 2 {
        return vec![];
    }
    merged
        .iter()
        .enumerate()
        .map(|(i, m)| Bound {
            start: m.start - 1,
            end: m.end,
            interval: Some(i + 1),
            setpoint: Some(
                keys.iter()
                    .zip(&m.sig)
                    .map(|(k, v)| ((*k).to_string(), *v as f64 / 10.0))
                    .collect(),
            ),
            lap: None,
            lap_distance_m: None,
        })
        .collect()
}

/// Splits an exercise into segments and aggregates every channel per segment. Cumulative
/// channels report the distance (or calories, MOVEs) covered within the segment. A segment
/// covers samples with `start_sec < t <= end_sec`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "lap times are whole seconds"
)]
pub fn segment_analytics(
    samples: &[AnalyticsSample],
    channels: &[String],
    laps: &[Lap],
    options: SegmentOptions,
) -> (SegmentMode, Vec<Segment>) {
    let by = options.segment_by;
    if samples.is_empty() {
        let mode = match by {
            SegmentBy::Workload => SegmentMode::Workload,
            SegmentBy::Laps => SegmentMode::Laps,
            SegmentBy::Auto | SegmentBy::Fixed => SegmentMode::Fixed,
        };
        return (mode, vec![]);
    }

    let mut mode = SegmentMode::Fixed;
    let mut bounds: Vec<Bound> = vec![];
    if matches!(by, SegmentBy::Auto | SegmentBy::Workload) {
        bounds = workload_bounds(samples, channels);
        if !bounds.is_empty() {
            mode = SegmentMode::Workload;
        }
    }
    if bounds.is_empty() && matches!(by, SegmentBy::Auto | SegmentBy::Laps) && !laps.is_empty() {
        let mut prev = 0;
        for lap in laps {
            let Some(total) = lap.total_time_sec else {
                continue;
            };
            let end = total.round() as i64;
            bounds.push(Bound {
                lap: lap.n,
                lap_distance_m: lap.lap_distance_m,
                ..Bound::plain(prev, end)
            });
            prev = end;
        }
        if !bounds.is_empty() {
            mode = SegmentMode::Laps;
        }
    }
    if bounds.is_empty() {
        mode = SegmentMode::Fixed;
        let step = options
            .segment_seconds
            .filter(|s| *s > 0)
            .unwrap_or(300)
            .max(10);
        let last_t = samples.iter().map(|r| r.t).max().unwrap_or(0);
        let mut s = 0;
        while s < last_t {
            bounds.push(Bound::plain(s, (s + step).min(last_t)));
            s += step;
        }
    }

    let mut segments = vec![];
    for b in bounds {
        let rows: Vec<&AnalyticsSample> = samples
            .iter()
            .filter(|r| b.start < r.t && r.t <= b.end)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let stats = stats_of(&rows, channels);
        let speed = stats.get("Speed").and_then(|s| s.avg).filter(|v| *v > 0.0);
        let channels = stats
            .into_iter()
            .map(|(ch, st)| {
                let entry = if is_cumulative(&ch) {
                    let vals = numbers(&rows, &ch);
                    let (first, last) = (vals[0], vals[vals.len() - 1]);
                    SegmentChannel::Cumulative(CumulativeDelta {
                        delta: round(last - first, 2),
                        end: last,
                    })
                } else {
                    SegmentChannel::Stats(st)
                };
                (ch, entry)
            })
            .collect();
        segments.push(Segment {
            start_sec: b.start,
            end_sec: b.end,
            duration_sec: b.end - b.start,
            interval: b.interval,
            setpoint: b.setpoint,
            lap: b.lap,
            lap_distance_m: b.lap_distance_m,
            pace_min_per_km: speed.map(|v| round(60.0 / v, 2)),
            channels,
        });
    }
    (mode, segments)
}

/// Rows with `from_sec <= t <= to_sec`; `to_sec` of 0 or less means no upper bound.
pub fn window_samples<T: Timed + Clone>(rows: &[T], from_sec: i64, to_sec: i64) -> Vec<T> {
    let lo = from_sec.max(0);
    rows.iter()
        .filter(|r| r.t() >= lo && (to_sec <= 0 || r.t() <= to_sec))
        .cloned()
        .collect()
}

/// Every `step`th row, starting with the first.
pub fn downsample<T: Clone>(rows: &[T], step: usize) -> Vec<T> {
    let n = step.max(1);
    rows.iter().step_by(n).cloned().collect()
}
