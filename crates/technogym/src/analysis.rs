//! Derived views over [`ExerciseAnalytics::samples`](crate::types::ExerciseAnalytics): per-channel
//! stats and interval / lap / window segments. Pure functions, no HTTP. The mywellness summary
//! block omits most machine channels (`RunningCadence` and `RunningPower` on a Skillrun, for one),
//! so these fill that gap.
//!
//! Samples are change-driven: the machine sends a row when a value changes, and that value holds
//! until the next row. Every aggregate here is weighted by the seconds a value held, not by rows.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::{AnalyticsSample, ExerciseAnalytics, HrSample, Lap};

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
/// Channels where 0 is a real setting (a flat belt), not a stopped machine: their zeros count.
const ZERO_IS_A_READING: &[&str] = &["Grade", "Incline"];
/// Treadmill speed (km/h) from which the ACSM running equation replaces the walking one.
const ACSM_RUNNING_KMH: f64 = 8.0;
/// The longest a heart-rate reading holds: a longer gap means the strap dropped out.
const HR_HOLD_MAX_SEC: i64 = 5;

/// Aggregates of one channel over a span. Measured channels get the time-weighted `avg` and
/// `min_nonzero` over the non-zero readings (the machine reports 0 while the belt is stopped);
/// cumulative channels get `final`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStats {
    /// The largest reading.
    pub max: f64,
    /// How many readings were in effect during the span.
    pub samples: usize,
    /// Seconds of the span the channel had a reading.
    pub seconds: i64,
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

/// Heart rate over a span, and how much of the span had a reading.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HrStats {
    /// Time-weighted mean bpm, one decimal.
    pub avg: f64,
    /// Lowest reading.
    pub min: i64,
    /// Highest reading.
    pub max: i64,
    /// Share of the span with a reading, 0 to 1. A reading holds until the next, at most 5 s.
    pub coverage: f64,
}

/// Seconds spent moving (`Speed` above 0) per grade band.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GradeBands {
    /// Below 5 %.
    #[serde(rename = "below5")]
    pub below_5: i64,
    /// 5 % up to 10 %.
    #[serde(rename = "5to10")]
    pub from_5_to_10: i64,
    /// 10 % and steeper.
    #[serde(rename = "10plus")]
    pub from_10: i64,
}

/// A cumulative channel within one segment: the amount covered in it, and its value at the end.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CumulativeDelta {
    /// Covered within the segment, interpolated at its bounds.
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
    /// Climbed in the segment: the `VDistance` delta, else from speed and positive grade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevation_gain_m: Option<f64>,
    /// Oxygen cost of the average speed and grade (ACSM walking or running equation), ml/kg/min.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acsm_vo2: Option<f64>,
    /// Average heart rate per unit of [`acsm_vo2`](Self::acsm_vo2): falling over weeks at the same
    /// work means fitter; rising across one steady session is cardiac drift.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hr_per_vo2: Option<f64>,
    /// Per-channel aggregates.
    pub channels: BTreeMap<String, SegmentChannel>,
    /// Heart rate over the segment, when there was any; see [`add_heart_rate`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hr: Option<HrStats>,
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

/// Each row with the seconds of `[lo, hi)` it holds for: from its `t` until the next row's (the
/// last row: one second). Rows holding for no second of the span are left out.
fn held(samples: &[AnalyticsSample], lo: i64, hi: i64) -> Vec<(&AnalyticsSample, i64)> {
    samples
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            let next = samples.get(i + 1).map_or(row.t + 1, |n| n.t);
            let secs = next.min(hi) - row.t.max(lo);
            (secs > 0).then_some((row, secs))
        })
        .collect()
}

/// A cumulative channel's value at second `t`: linear between the readings around it, from 0 at
/// `t = 0`, and the last reading after the last one.
#[expect(
    clippy::cast_precision_loss,
    reason = "second offsets are far below 2^52"
)]
fn value_at(samples: &[AnalyticsSample], channel: &str, t: i64) -> f64 {
    let t = t.max(0);
    let mut prev = (0, 0.0);
    for row in samples {
        let Some(v) = row.channels.get(channel).copied().flatten() else {
            continue;
        };
        if row.t >= t {
            if row.t == prev.0 {
                return v;
            }
            return prev.1 + (v - prev.1) * (t - prev.0) as f64 / (row.t - prev.0) as f64;
        }
        prev = (row.t, v);
    }
    prev.1
}

fn is_cumulative(channel: &str) -> bool {
    CUMULATIVE_CHANNELS.contains(&channel)
}

/// Time-weighted aggregates of every channel over the seconds `[lo, hi)`.
#[expect(
    clippy::cast_precision_loss,
    reason = "second counts are far below 2^52"
)]
fn stats_of(
    samples: &[AnalyticsSample],
    lo: i64,
    hi: i64,
    channels: &[String],
) -> BTreeMap<String, ChannelStats> {
    let rows = held(samples, lo, hi);
    let mut stats = BTreeMap::new();
    for ch in channels {
        let values: Vec<(f64, i64)> = rows
            .iter()
            .filter_map(|(row, secs)| Some((row.channels.get(ch).copied().flatten()?, *secs)))
            .collect();
        if values.is_empty() {
            continue;
        }
        let mut entry = ChannelStats {
            max: values.iter().map(|v| v.0).fold(f64::NEG_INFINITY, f64::max),
            samples: values.len(),
            seconds: values.iter().map(|v| v.1).sum(),
            ..ChannelStats::default()
        };
        if is_cumulative(ch) {
            entry.final_value = values.last().map(|v| v.0);
        } else {
            let zero_counts = ZERO_IS_A_READING.contains(&ch.as_str());
            let nonzero: Vec<(f64, i64)> = values
                .iter()
                .copied()
                .filter(|v| v.0 > 0.0 || zero_counts)
                .collect();
            let secs: i64 = nonzero.iter().map(|v| v.1).sum();
            entry.avg = Some(if secs == 0 {
                0.0
            } else {
                round(
                    nonzero.iter().map(|(v, s)| v * *s as f64).sum::<f64>() / secs as f64,
                    1,
                )
            });
            entry.min_nonzero = Some(nonzero.iter().map(|v| v.0).fold(f64::INFINITY, f64::min))
                .filter(|m| m.is_finite())
                .or(Some(0.0));
        }
        stats.insert(ch.clone(), entry);
    }
    stats
}

/// Per-channel aggregates over the whole exercise. Channels with no numeric reading are left out.
pub fn channel_stats(
    samples: &[AnalyticsSample],
    channels: &[String],
) -> BTreeMap<String, ChannelStats> {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return BTreeMap::new();
    };
    stats_of(samples, first.t, last.t + 1, channels)
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
            _ => {
                // A setpoint holds until the next change, not just until its own last row.
                if let Some(last) = blocks.last_mut() {
                    last.end = row.t - 1;
                }
                blocks.push(Block {
                    sig: s,
                    start: row.t,
                    end: row.t,
                });
            }
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
/// channels report the distance (or calories, MOVEs) covered within the segment, interpolated at
/// its bounds so consecutive segments add up to the whole. A segment covers the seconds
/// `start_sec < t <= end_sec`, including a value that started before it and still holds.
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
        let stats = stats_of(samples, b.start + 1, b.end + 1, channels);
        if stats.is_empty() {
            continue;
        }
        let speed = stats.get("Speed").and_then(|s| s.avg).filter(|v| *v > 0.0);
        let grade = stats
            .get("Grade")
            .or_else(|| stats.get("Incline"))
            .and_then(|s| s.avg)
            .unwrap_or(0.0);
        let channels = stats
            .into_iter()
            .map(|(ch, st)| {
                let entry = if is_cumulative(&ch) {
                    let end = value_at(samples, &ch, b.end);
                    SegmentChannel::Cumulative(CumulativeDelta {
                        delta: round(end - value_at(samples, &ch, b.start), 2),
                        end: round(end, 2),
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
            elevation_gain_m: elevation_gain(samples, b.start + 1, b.end + 1),
            acsm_vo2: speed.map(|kmh| acsm_vo2(kmh, grade)),
            hr_per_vo2: None,
            channels,
            hr: None,
        });
    }
    (mode, segments)
}

/// ACSM oxygen cost (ml/kg/min) of a treadmill at `kmh` and `grade_pct`: the walking equation
/// below 8 km/h, the running one from there. Unlike speed x grade it stays meaningful on the flat.
pub fn acsm_vo2(kmh: f64, grade_pct: f64) -> f64 {
    let m_per_min = kmh * 1000.0 / 60.0;
    let grade = grade_pct / 100.0;
    let vo2 = if kmh < ACSM_RUNNING_KMH {
        3.5 + 0.1 * m_per_min + 1.8 * m_per_min * grade
    } else {
        3.5 + 0.2 * m_per_min + 0.9 * m_per_min * grade
    };
    round(vo2, 1)
}

/// Metres climbed in the seconds `[lo, hi)`: the `VDistance` covered when the machine reports
/// it, else held speed times positive grade. `None` when neither is there.
#[expect(
    clippy::cast_precision_loss,
    reason = "second counts are far below 2^52"
)]
pub fn elevation_gain(samples: &[AnalyticsSample], lo: i64, hi: i64) -> Option<f64> {
    let reports = |ch: &str| {
        samples
            .iter()
            .any(|r| r.channels.get(ch).copied().flatten().is_some())
    };
    if reports("VDistance") {
        let gain = value_at(samples, "VDistance", hi - 1) - value_at(samples, "VDistance", lo - 1);
        return Some(round(gain, 1));
    }
    let grade_channel = ["Grade", "Incline"].into_iter().find(|ch| reports(ch))?;
    if !reports("Speed") {
        return None;
    }
    let metres = held(samples, lo, hi)
        .iter()
        .filter_map(|(row, secs)| {
            let kmh = row.channels.get("Speed").copied().flatten()?;
            let grade = row.channels.get(grade_channel).copied().flatten()?;
            Some(kmh / 3.6 * *secs as f64 * grade.max(0.0) / 100.0)
        })
        .sum::<f64>();
    Some(round(metres, 1))
}

/// Metres climbed over the whole exercise; see [`elevation_gain`].
pub fn exercise_elevation_gain(samples: &[AnalyticsSample]) -> Option<f64> {
    elevation_gain(samples, 0, samples.last()?.t + 1)
}

/// Seconds spent moving per grade band over the whole exercise; `None` without a `Grade` or
/// `Incline` channel.
pub fn grade_bands(samples: &[AnalyticsSample]) -> Option<GradeBands> {
    let grade_channel = ["Grade", "Incline"].into_iter().find(|ch| {
        samples
            .iter()
            .any(|r| r.channels.get(*ch).copied().flatten().is_some())
    })?;
    let (first, last) = (samples.first()?, samples.last()?);
    let mut bands = GradeBands::default();
    for (row, secs) in held(samples, first.t, last.t + 1) {
        let moving = row
            .channels
            .get("Speed")
            .copied()
            .flatten()
            .is_none_or(|s| s > 0.0);
        let Some(grade) = row.channels.get(grade_channel).copied().flatten() else {
            continue;
        };
        if !moving {
            continue;
        }
        match grade {
            g if g < 5.0 => bands.below_5 += secs,
            g if g < 10.0 => bands.from_5_to_10 += secs,
            _ => bands.from_10 += secs,
        }
    }
    Some(bands)
}

/// Heart rate over the seconds `[lo, hi)`, or `None` without a reading in it. `hr` holds valid
/// readings only, sorted by `t`, as the parser leaves them.
#[expect(
    clippy::cast_precision_loss,
    reason = "second counts and bpm are far below 2^52"
)]
pub fn hr_stats(hr: &[HrSample], lo: i64, hi: i64) -> Option<HrStats> {
    let mut covered = 0;
    let mut weighted = 0.0;
    let (mut min, mut max) = (i64::MAX, i64::MIN);
    for (i, h) in hr.iter().enumerate() {
        let next = hr
            .get(i + 1)
            .map_or(h.t + 1, |n| n.t)
            .min(h.t + HR_HOLD_MAX_SEC);
        let secs = next.min(hi) - h.t.max(lo);
        if secs <= 0 {
            continue;
        }
        covered += secs;
        weighted += h.hr as f64 * secs as f64;
        min = min.min(h.hr);
        max = max.max(h.hr);
    }
    (covered > 0).then(|| HrStats {
        avg: round(weighted / covered as f64, 1),
        min,
        max,
        coverage: round(covered as f64 / (hi - lo) as f64, 2),
    })
}

/// Fills in each segment's heart rate and returns the whole exercise's, from its first to its
/// last reading of any kind.
pub fn add_heart_rate(analytics: &ExerciseAnalytics, segments: &mut [Segment]) -> Option<HrStats> {
    for s in segments.iter_mut() {
        s.hr = hr_stats(&analytics.hr, s.start_sec + 1, s.end_sec + 1);
        s.hr_per_vo2 =
            s.hr.as_ref()
                .zip(s.acsm_vo2)
                .map(|(hr, vo2)| round(hr.avg / vo2, 2));
    }
    let firsts = [
        analytics.samples.first().map(|s| s.t),
        analytics.hr.first().map(|h| h.t),
    ];
    let lasts = [
        analytics.samples.last().map(|s| s.t),
        analytics.hr.last().map(|h| h.t),
    ];
    let lo = firsts.into_iter().flatten().min()?;
    let hi = lasts.into_iter().flatten().max()? + 1;
    hr_stats(&analytics.hr, lo, hi)
}

/// Rows with `from_sec <= t <= to_sec`, plus the row still in effect at `from_sec` (its `t` is
/// earlier); `to_sec` of 0 or less means no upper bound.
pub fn window_samples<T: Timed + Clone>(rows: &[T], from_sec: i64, to_sec: i64) -> Vec<T> {
    let lo = from_sec.max(0);
    let start = rows.iter().rposition(|r| r.t() <= lo).unwrap_or(0);
    rows[start.min(rows.len())..]
        .iter()
        .filter(|r| to_sec <= 0 || r.t() <= to_sec)
        .cloned()
        .collect()
}

/// The rows in effect at every `step`th second from the first row's `t`, each at most once and
/// with its own `t`: a change-driven series thinned by time, not by row count.
pub fn sample_every<T: Timed + Clone>(rows: &[T], step: usize) -> Vec<T> {
    let Some(first) = rows.first().map(Timed::t) else {
        return vec![];
    };
    // Capped so the arithmetic below cannot overflow; no exercise lasts 2^31 s.
    let step = i64::try_from(step).unwrap_or(i64::MAX).clamp(1, 1 << 31);
    rows.iter()
        .enumerate()
        .filter(|(i, row)| {
            let next = rows.get(i + 1).map_or(row.t() + 1, Timed::t);
            let grid = first + (row.t() - first + step - 1).div_euclid(step) * step;
            grid < next
        })
        .map(|(_, row)| row.clone())
        .collect()
}

/// Every `step`th row, starting with the first, for rows without a time offset.
pub fn downsample<T: Clone>(rows: &[T], step: usize) -> Vec<T> {
    let n = step.max(1);
    rows.iter().step_by(n).cloned().collect()
}
