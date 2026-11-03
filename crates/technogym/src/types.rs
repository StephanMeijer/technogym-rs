//! Parsed shapes of mywellness responses. Every parsed object has a `*_raw` twin method on
//! [`Technogym`](crate::Technogym) that returns the untouched payload, for anything these types
//! do not model. Serialised as camelCase JSON.
//!
//! Localised strings (exercise names, units, `formatted` values) come back in the account's
//! culture.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A gym the account belongs to. `url` is the slug every training endpoint path starts with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Facility {
    /// The facility id.
    pub id: String,
    /// The url slug.
    pub url: String,
    /// The display name.
    pub name: String,
}

/// The signed-in account, from the login response's `userContext` and `facilities`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UserInfo {
    /// The user id, substituted for `{userId}` in training paths.
    pub user_id: String,
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Nickname.
    pub nick_name: String,
    /// e.g. `Metric`.
    pub measurement_system: String,
    /// e.g. `nb-NO`; localised strings in every response follow it.
    pub culture: String,
    /// The gyms the account belongs to.
    pub facilities: Vec<Facility>,
}

/// One measured or prescribed property, normalised from mywellness's two row shapes. `value` is
/// in the API's base unit (seconds, metres, kg), `display_value` is in `unit` (the unit the
/// console shows), and `formatted` is the localised display string. On `CardioLog` rows `value`
/// and `display_value` are the same number.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Property {
    /// In the base unit.
    pub value: Value,
    /// In `unit`.
    pub display_value: Value,
    /// The display unit.
    pub unit: String,
    /// The localised display string.
    pub formatted: Value,
}

/// Properties keyed by their mywellness name (`Calories`, `AvgSpeed`, `IsoWeight`, ...).
pub type PropertyMap = BTreeMap<String, Property>;

/// A row of `list_sessions`. `session_id` and `date` together address the session's detail calls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// The session's `idCr`.
    pub session_id: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `HH:MM`, local to the gym.
    pub time: String,
    /// The session's name.
    pub name: String,
    /// `WorkoutSession`, `HRWorkout`, ...
    #[serde(rename = "type")]
    pub kind: String,
    /// Minutes.
    pub duration_min: Option<i64>,
    /// MOVEs.
    pub moves: Option<i64>,
    /// Calories.
    pub calories: Option<i64>,
    /// Exercises done.
    pub exercises_done: Option<i64>,
    /// Exercises planned.
    pub exercises_planned: Option<i64>,
    /// The `userWorkoutSessionId`.
    pub user_workout_session_id: String,
    /// The facility the session was started in.
    pub facility_id: String,
    /// Effectiveness score.
    pub workout_effectiveness: Option<i64>,
    /// Compliance score.
    pub workout_compliance: Option<i64>,
}

/// One performed or prescribed set of an exercise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetData {
    /// 1-based.
    pub set_number: usize,
    /// `IsoReps`, else `Reps`.
    pub reps: Option<f64>,
    /// `IsoWeight`, else `Weight`.
    pub weight_kg: Option<f64>,
    /// `RestTime`.
    pub rest_sec: Option<f64>,
    /// `Duration`.
    pub duration_sec: Option<f64>,
    /// Every raw property of the step.
    pub properties: PropertyMap,
}

/// Allowed workload range for one property on this machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRange {
    /// Lower bound.
    pub min: Value,
    /// Upper bound.
    pub max: Value,
    /// Step.
    pub step: Value,
    /// Whether the console lets the user change it.
    pub read_only: bool,
    /// The API's visibility flag.
    pub visibility: Value,
}

/// Prescribed or estimated figures, to compare with what was done.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Estimated {
    /// Duration.
    pub duration: Value,
    /// Calories.
    pub calories: Value,
    /// MOVEs.
    pub moves: Value,
}

/// One exercise of a workout session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exercise {
    /// `performedPhysicalActivity.id`.
    pub activity_id: String,
    /// Pass to `get_exercise_analytics` for the per-second data.
    pub analytics_id: String,
    /// The exercise name.
    pub name: String,
    /// The equipment's name, e.g. `Leg press #40`.
    pub machine: String,
    /// The equipment type.
    pub equipment_type: String,
    /// `StrengthLoad`, `Cardio`, ... (`physicalActivityType`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Whether it is a cardio exercise.
    pub is_cardio: bool,
    /// The status.
    pub status: String,
    /// When it was done.
    pub done_on: String,
    /// Position in the session.
    pub position: Option<i64>,
    /// Localised display string, e.g. `6 min.`.
    pub duration: String,
    /// Localised display string.
    pub calories: String,
    /// Localised display string.
    pub moves: String,
    /// Muscle names.
    pub muscles: Vec<String>,
    /// Every summary property the machine reported (cardio: `AvgSpeed`, `AvgRunningCadence`,
    /// `AvgRunningPower`, `Pace`...; strength: `Rm1`, `TotalIsoWeight`...).
    pub summary: PropertyMap,
    /// Performed sets.
    pub sets: Vec<SetData>,
    /// Prescribed sets.
    pub prescribed_sets: Vec<SetData>,
    /// Short name.
    pub short_name: String,
    /// Activity code.
    pub activity_code: Option<i64>,
    /// Activity type id.
    pub activity_type_id: String,
    /// Equipment code.
    pub equipment_code: Option<i64>,
    /// What the exercise was set to: `Duration`, `Distance`, `Calories`, ...
    pub target_type: String,
    /// `Added`, `Prescribed`, ...
    pub done_as: String,
    /// Logged by hand rather than by a machine.
    pub manually_done: bool,
    /// The user changed the prescription.
    pub has_adjustments: bool,
    /// Stopped early.
    pub done_partially: bool,
    /// Progress percentage.
    pub progress: Option<i64>,
    /// Prescribed or estimated figures.
    pub estimated: Estimated,
    /// Allowed ranges per property; left out when empty.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub workload_ranges: BTreeMap<String, WorkloadRange>,
    /// Non-empty console `extData`, with the `mwc_` prefix removed.
    pub ext_data: BTreeMap<String, Value>,
}

/// Full detail of one workout session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkoutSession {
    /// The session id.
    pub session_id: String,
    /// `YYYY-MM-DD`, from `startedOn`.
    pub date: String,
    /// Start timestamp.
    pub started_on: String,
    /// End timestamp.
    pub closed_on: String,
    /// The session name.
    pub name: String,
    /// The session type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The status.
    pub status: String,
    /// Localised display string.
    pub duration: String,
    /// Localised display string.
    pub duration_planned: String,
    /// Localised display string.
    pub calories: String,
    /// Localised display string.
    pub moves: String,
    /// Efficacy score.
    pub efficacy: Option<i64>,
    /// Efficacy quality label.
    pub efficacy_quality: String,
    /// Efficacy description.
    pub efficacy_description: String,
    /// Muscle name to score.
    pub muscle_scores: BTreeMap<String, i64>,
    /// Only exercises that were performed.
    pub exercises: Vec<Exercise>,
    /// Prescribed by a Technogym coach.
    pub from_tg_coach: bool,
    /// The facility the session was started in.
    pub facility_id: String,
    /// The `userWorkoutSessionId`.
    pub user_workout_session_id: String,
    /// The console application that logged it.
    pub client_application: String,
    /// Every session-level total.
    pub done_summary: PropertyMap,
}

/// A heart-rate zone and the time spent in it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HrZone {
    /// The zone name.
    pub name: String,
    /// Lower bound in bpm.
    pub lower_bound: Option<f64>,
    /// Upper bound in bpm.
    pub upper_bound: Option<f64>,
    /// Seconds spent in the zone.
    pub seconds_in_zone: f64,
}

/// A reading of an exercise: `t` plus one value per machine channel (`Speed`, `Power`, ...).
/// Change-driven: a row is sent when a value changes, and holds until the next row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsSample {
    /// Seconds since the start of the exercise.
    pub t: i64,
    /// Channel name to reading.
    #[serde(flatten)]
    pub channels: BTreeMap<String, Option<f64>>,
}

/// One heart-rate reading of an exercise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HrSample {
    /// Seconds since the start of the exercise.
    pub t: i64,
    /// Beats per minute.
    pub hr: i64,
}

/// A console auto-lap (every km on a treadmill).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lap {
    /// Lap number.
    pub n: Option<i64>,
    /// Lap distance in metres.
    pub lap_distance_m: Option<f64>,
    /// Lap time in seconds.
    pub lap_time_sec: Option<f64>,
    /// Total distance at the end of the lap.
    pub total_distance_m: Option<f64>,
    /// Total time at the end of the lap.
    pub total_time_sec: Option<f64>,
}

/// Per-second analytics of one exercise: machine channels, heart rate and laps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseAnalytics {
    /// The analytics id.
    pub analytics_id: String,
    /// The exercise name.
    pub name: String,
    /// The equipment type.
    pub equipment_type: String,
    /// Channel names in descriptor order. Strength machines have none.
    pub channels: Vec<String>,
    /// Change-driven samples, sorted by `t`.
    pub samples: Vec<AnalyticsSample>,
    /// Heart-rate samples.
    pub hr: Vec<HrSample>,
    /// Heart-rate zones.
    pub hr_zones: Vec<HrZone>,
    /// Summary properties.
    pub summary: PropertyMap,
    /// As the API formats it, e.g. `09.09.2026`.
    pub date: String,
    /// The target.
    pub target: String,
    /// Activity type id.
    pub activity_type_id: String,
    /// Activity name.
    pub activity_name: String,
    /// Console auto-laps.
    pub laps: Vec<Lap>,
}

/// One timestamped heart-rate reading of a session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HrSessionSample {
    /// The timestamp as the API formats it.
    pub timestamp: String,
    /// Beats per minute.
    pub hr: i64,
}

/// The whole-session heart-rate trace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HrSession {
    /// The session id.
    pub session_id: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// Duration, avg/min/max HR, calories, MOVEs, METs.
    pub summary: PropertyMap,
    /// Timestamped readings.
    pub hr_samples: Vec<HrSessionSample>,
    /// Heart-rate zones.
    pub hr_zones: Vec<HrZone>,
}
