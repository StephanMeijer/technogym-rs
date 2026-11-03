//! Pure parsers from mywellness payloads to the types in [`types`](crate::types). They never
//! fail on a missing field: absent strings become `""`, absent numbers `None`, absent lists `[]`.

use serde_json::Value;
use std::collections::BTreeMap;

use crate::dates::from_partition_key;
use crate::types::{
    AnalyticsSample, Estimated, Exercise, ExerciseAnalytics, Facility, HrSample, HrSession,
    HrSessionSample, HrZone, Lap, PropertyMap, SessionSummary, SetData, UserInfo, WorkloadRange,
    WorkoutSession,
};

/// The value when it is an object, else null: indexing null yields null, never a panic.
fn rec(value: &Value) -> &Value {
    if value.is_object() {
        value
    } else {
        &Value::Null
    }
}

/// The array's elements, or none.
fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// Strings as they are, numbers and booleans as text, objects as JSON, null as `""`.
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The first value that is neither null nor an empty string, as text.
fn first_text(values: &[&Value]) -> String {
    values
        .iter()
        .find(|v| !v.is_null() && v.as_str() != Some(""))
        .map_or_else(String::new, |v| text(v))
}

/// JavaScript's `a ?? b`: the first value that is not null.
fn coalesce<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if a.is_null() { b } else { a }
}

/// JavaScript truthiness for the values the API uses as ids.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// A number, or a numeric string, as `f64`.
pub fn num_or_null(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.is_empty() => s.trim().parse().ok(),
        _ => None,
    }
}

/// A number, or a numeric string, truncated to an integer.
#[expect(
    clippy::cast_possible_truncation,
    reason = "truncation toward zero is the intent"
)]
pub fn int_or_null(value: &Value) -> Option<i64> {
    num_or_null(value).map(|f| f.trunc() as i64)
}

/// The login response's account, or `None` when it has no `userContext.id`.
pub fn parse_user(data: &Value) -> Option<UserInfo> {
    let d = rec(data);
    let uc = rec(&d["userContext"]);
    if !truthy(&uc["id"]) {
        return None;
    }
    let facilities = list(&d["facilities"])
        .iter()
        .map(|f| Facility {
            id: text(&f["id"]),
            url: text(&f["url"]),
            name: text(&f["name"]),
        })
        .collect();
    Some(UserInfo {
        user_id: text(&uc["id"]),
        first_name: text(&uc["firstName"]),
        last_name: text(&uc["lastName"]),
        nick_name: text(&uc["nickName"]),
        measurement_system: text(&uc["measurementSystem"]),
        culture: text(&uc["defaultCulture"]),
        facilities,
    })
}

/// One `ActivityHistory` item.
pub fn parse_session_summary(item: &Value) -> SessionSummary {
    let i = rec(item);
    let pad = |v: &Value| format!("{:02}", int_or_null(v).unwrap_or(0));
    SessionSummary {
        session_id: text(&i["idCr"]),
        date: from_partition_key(&i["partitionDate"]),
        time: format!("{}:{}", pad(&i["hour"]), pad(&i["minute"])),
        name: text(&i["name"]),
        kind: text(&i["type"]),
        duration_min: int_or_null(coalesce(&i["totalDurationInMinutes"], &i["duration"])),
        moves: int_or_null(&i["moves"]),
        calories: int_or_null(&i["calories"]),
        exercises_done: int_or_null(&i["numberOfExerciseDoneWithGroups"]),
        exercises_planned: int_or_null(&i["numberOfExerciseToDoWithGroups"]),
        user_workout_session_id: text(&i["userWorkoutSessionId"]),
        facility_id: text(&i["facilityId"]),
        workout_effectiveness: int_or_null(&i["workoutEffectiveness"]),
        workout_compliance: int_or_null(&i["workoutCompliance"]),
    }
}

/// Normalises mywellness property rows. Two shapes exist: session and HR rows carry `value` in
/// the base unit plus `displayValue` in `unitOfMeasureShortString`; `CardioLog` rows carry
/// `rawValue` already in the display unit (`uM`) and `value` as a formatted string.
pub fn parse_properties(rows: &Value) -> PropertyMap {
    let mut out = PropertyMap::new();
    for p in list(rows) {
        let key = text(coalesce(&p["physicalProperty"], &p["property"]));
        if key.is_empty() {
            continue;
        }
        let property = if let Some(raw) = p.get("rawValue") {
            crate::types::Property {
                value: raw.clone(),
                display_value: raw.clone(),
                unit: text(&p["uM"]),
                formatted: p["value"].clone(),
            }
        } else {
            crate::types::Property {
                value: p["value"].clone(),
                display_value: p.get("displayValue").unwrap_or(&p["value"]).clone(),
                unit: text(&p["unitOfMeasureShortString"]),
                formatted: p["formattedValue"].clone(),
            }
        };
        out.insert(key, property);
    }
    out
}

fn step_to_set(set_number: usize, properties: PropertyMap) -> SetData {
    let num = |key: &str| properties.get(key).and_then(|p| num_or_null(&p.value));
    SetData {
        set_number,
        reps: num("IsoReps").or_else(|| num("Reps")),
        weight_kg: num("IsoWeight").or_else(|| num("Weight")),
        rest_sec: num("RestTime"),
        duration_sec: num("Duration"),
        properties,
    }
}

fn sets(steps: &Value, key: &str) -> Vec<SetData> {
    list(steps)
        .iter()
        .enumerate()
        .map(|(i, step)| step_to_set(i + 1, parse_properties(&step[key])))
        .collect()
}

/// One `physicalActivities` entry of a session.
pub fn parse_exercise(activity: &Value) -> Exercise {
    let act = rec(activity);
    let pa = rec(&act["performedPhysicalActivity"]);
    let pdata = rec(&pa["data"]);
    // sic: the API's spelling.
    let prescribed = rec(&act["displayPrescibedPhysicalActivity"]);

    let mut workload_ranges = BTreeMap::new();
    for c in list(&pdata["constraints"]) {
        let prop = text(&c["physicalProperty"]);
        if prop.is_empty() {
            continue;
        }
        let range = &c["discreteRange"];
        workload_ranges.insert(
            prop,
            WorkloadRange {
                min: range["min"].clone(),
                max: range["max"].clone(),
                step: range["step"].clone(),
                read_only: c["isReadOnly"].as_bool().unwrap_or(false),
                visibility: c["visibility"].clone(),
            },
        );
    }

    let ext_data = pa["extData"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, v)| !v.is_null() && v.as_str() != Some(""))
        .map(|(k, v)| (k.strip_prefix("mwc_").unwrap_or(k).to_string(), v.clone()))
        .collect();

    Exercise {
        activity_id: text(&pa["id"]),
        analytics_id: first_text(&[&act["analyticsId"], &pa["analiticsId"]]), // sic
        name: text(&act["physicalActivityName"]),
        machine: text(&act["equipmentName"]),
        equipment_type: text(&act["equipmentType"]),
        kind: text(&act["physicalActivityType"]),
        is_cardio: act["isCardio"].as_bool().unwrap_or(false),
        status: text(&act["status"]),
        done_on: text(&pa["doneOn"]),
        position: int_or_null(&act["position"]),
        duration: first_text(&[
            &act["displayDurationDoneShort"],
            &act["displayDurationShort"],
        ]),
        calories: text(&act["displayCaloriesDoneShort"]),
        moves: text(&act["displayMoveDoneShort"]),
        muscles: list(&act["muscles"])
            .iter()
            .map(|m| first_text(&[&m["muscleName"], &m["muscleType"]]))
            .collect(),
        summary: parse_properties(&pdata["data"]),
        sets: sets(&pdata["steps"], "data"),
        prescribed_sets: sets(&prescribed["steps"], "properties"),
        short_name: first_text(&[&act["physicalActivityShortName"], &pa["shortName"]]),
        activity_code: int_or_null(&act["physicalActivityCode"]),
        activity_type_id: text(&act["physicalActivityId"]),
        equipment_code: int_or_null(&act["equipmentCode"]),
        target_type: text(&act["targetType"]),
        done_as: text(&pa["doneAs"]),
        manually_done: pa["manuallyDone"].as_bool().unwrap_or(false),
        has_adjustments: act["hasAdjustments"].as_bool().unwrap_or(false),
        done_partially: act["hasBeenDonePartially"].as_bool().unwrap_or(false),
        progress: int_or_null(coalesce(&pa["progress"], &act["progress"])),
        estimated: Estimated {
            duration: act["estimatedDuration"].clone(),
            calories: act["estimatedCalories"].clone(),
            moves: act["estimatedMove"].clone(),
        },
        workload_ranges,
        ext_data,
    }
}

/// A `GetPerformedWorkoutSessionByIdCr` payload. Exercises never performed are left out.
pub fn parse_workout_session(data: &Value, session_id: &str) -> WorkoutSession {
    let d = rec(data);
    let started_on = text(&d["startedOn"]);
    let done = parse_properties(&d["doneSummary"]);
    let muscle_scores = list(&d["muscleScores"])
        .iter()
        .map(|m| {
            (
                first_text(&[&m["muscleName"], &m["muscleType"]]),
                int_or_null(&m["score"]).unwrap_or(0),
            )
        })
        .collect();
    let formatted = |key: &str| done.get(key).map_or(Value::Null, |p| p.formatted.clone());
    WorkoutSession {
        session_id: session_id.to_string(),
        date: started_on.chars().take(10).collect(),
        started_on: started_on.clone(),
        closed_on: text(&d["closedOn"]),
        name: first_text(&[&d["displayName"], &d["name"]]),
        kind: text(&d["sessionType"]),
        status: text(&d["workoutSessionStatus"]),
        duration: text(&d["displayDurationDoneShort"]),
        duration_planned: text(&d["displayDurationShort"]),
        calories: first_text(&[&formatted("Calories"), &d["displayCaloriesDoneShort"]]),
        moves: first_text(&[&formatted("Move"), &d["displayMoveDoneShort"]]),
        efficacy: int_or_null(&d["workoutEfficacy"]),
        efficacy_quality: text(&d["workoutEfficacyQuality"]),
        efficacy_description: text(&d["workoutEfficacyDescription"]),
        muscle_scores,
        exercises: list(&d["physicalActivities"])
            .iter()
            .filter(|a| truthy(&a["performedPhysicalActivity"]["id"]))
            .map(parse_exercise)
            .collect(),
        from_tg_coach: d["isFromTgCoach"].as_bool().unwrap_or(false),
        facility_id: text(&d["startedInFacilityId"]),
        user_workout_session_id: text(&d["userWorkoutSessionId"]),
        client_application: text(&d["extData"]["mwc_client_application"]),
        done_summary: done,
    }
}

fn parse_zones(rows: &Value) -> Vec<HrZone> {
    list(rows)
        .iter()
        .map(|z| HrZone {
            name: text(&z["name"]),
            lower_bound: num_or_null(&z["lowerBound"]),
            upper_bound: num_or_null(&z["upperBound"]),
            seconds_in_zone: num_or_null(&z["secondsInZone"]).unwrap_or(0.0),
        })
        .collect()
}

/// A `CardioLog/{id}/Details` payload. The API spells the block `analitics`; `analytics` is
/// accepted too. Samples with an empty value vector are dropped.
pub fn parse_exercise_analytics(data: &Value, analytics_id: &str) -> ExerciseAnalytics {
    let d = rec(data);
    let an = rec(coalesce(&d["analitics"], &d["analytics"]));

    let channel_by_index: BTreeMap<usize, String> = list(&an["descriptor"])
        .iter()
        .filter_map(|ch| {
            let index = usize::try_from(int_or_null(&ch["i"])?).ok()?;
            Some((index, text(&ch["pr"]["name"])))
        })
        .collect();
    let channels = channel_by_index.values().cloned().collect();

    let mut samples: Vec<AnalyticsSample> = list(&an["samples"])
        .iter()
        .filter_map(|raw| {
            let vs = list(&raw["vs"]);
            if vs.is_empty() {
                return None;
            }
            Some(AnalyticsSample {
                t: int_or_null(&raw["t"]).unwrap_or(0),
                channels: channel_by_index
                    .iter()
                    .map(|(i, name)| (name.clone(), vs.get(*i).and_then(num_or_null)))
                    .collect(),
            })
        })
        .collect();
    samples.sort_by_key(|s| s.t);

    let hr = list(&an["hr"])
        .iter()
        .filter(|h| h.get("hr").is_some())
        .map(|h| HrSample {
            t: int_or_null(&h["t"]).unwrap_or(0),
            hr: int_or_null(&h["hr"]).unwrap_or(0),
        })
        .collect();

    let laps = list(&an["laps"])
        .iter()
        .map(|lap| Lap {
            n: int_or_null(&lap["n"]),
            lap_distance_m: num_or_null(&lap["lapDistance"]),
            lap_time_sec: num_or_null(&lap["lapTime"]),
            total_distance_m: num_or_null(&lap["totalDistance"]),
            total_time_sec: num_or_null(&lap["totalTime"]),
        })
        .collect();

    ExerciseAnalytics {
        analytics_id: analytics_id.to_string(),
        name: text(&d["name"]),
        equipment_type: text(&d["equipmentType"]),
        channels,
        samples,
        hr,
        hr_zones: parse_zones(&an["hrZones"]),
        summary: parse_properties(&d["data"]),
        date: text(&d["date"]),
        target: text(&d["target"]),
        activity_type_id: text(&d["physicalActivityId"]),
        activity_name: text(&d["physicalActivityName"]),
        laps,
    }
}

/// A `GetHrSession` payload.
pub fn parse_hr_session(data: &Value, session_id: &str, date: &str) -> HrSession {
    let d = rec(data);
    HrSession {
        session_id: session_id.to_string(),
        date: date.to_string(),
        summary: parse_properties(&d["properties"]),
        hr_samples: list(&d["hrSamples"])
            .iter()
            .map(|h| HrSessionSample {
                timestamp: text(&h["timestamp"]),
                hr: int_or_null(&h["hr"]).unwrap_or(0),
            })
            .collect(),
        hr_zones: parse_zones(&d["hrZones"]),
    }
}
