mod common;

use common::fixture;
use serde_json::json;
use technogym::parse::{
    parse_exercise_analytics, parse_hr_session, parse_properties, parse_session_summary,
    parse_user, parse_workout_session,
};

#[test]
fn user_from_login_response() {
    let user = parse_user(&fixture("login")["data"]).expect("user");
    assert_eq!(user.user_id, "u1");
    assert_eq!(user.culture, "en-GB");
    assert_eq!(user.facilities[0].url, "my-gym");
    assert!(parse_user(&json!({ "userContext": {} })).is_none());
}

#[test]
fn session_summary_normalises_ids_dates_and_numbers() {
    let items = fixture("activity_history")["items"].clone();
    let a = parse_session_summary(&items[0]);
    assert_eq!(a.session_id, "1001");
    assert_eq!(a.date, "2026-09-01");
    assert_eq!(a.time, "07:05");
    assert_eq!(a.duration_min, Some(48));
    assert_eq!(a.calories, Some(410));
    assert_eq!(a.workout_compliance, Some(95));
    let b = parse_session_summary(&items[1]);
    assert_eq!(b.session_id, "1002");
    assert_eq!(b.date, "2026-09-03");
    assert_eq!(b.time, "18:30");
    assert_eq!(b.duration_min, Some(31));
    assert_eq!(b.moves, None);
    assert_eq!(b.facility_id, "");
}

#[test]
fn properties_accept_both_row_shapes() {
    let rows = json!([
        { "physicalProperty": "Calories", "value": 410.7, "displayValue": 411, "unitOfMeasureShortString": "kcal", "formattedValue": "411 kcal" },
        { "property": "AvgSpeed", "rawValue": 11.2, "uM": "km/h", "value": "11,2 km/h" },
        { "physicalProperty": "NoDisplay", "value": 3 },
        { "value": 1 }
    ]);
    let map = parse_properties(&rows);
    assert_eq!(map.len(), 3);
    assert_eq!(map["Calories"].display_value, json!(411));
    assert_eq!(map["Calories"].unit, "kcal");
    assert_eq!(map["AvgSpeed"].value, json!(11.2));
    assert_eq!(map["AvgSpeed"].display_value, json!(11.2));
    assert_eq!(map["AvgSpeed"].formatted, json!("11,2 km/h"));
    assert_eq!(map["NoDisplay"].display_value, json!(3));
}

#[test]
fn workout_session_keeps_only_performed_exercises() {
    let ws = parse_workout_session(&fixture("workout_session"), "1001");
    assert_eq!(ws.session_id, "1001");
    assert_eq!(ws.date, "2026-09-01");
    assert_eq!(ws.name, "Push day");
    assert_eq!(ws.calories, "411 kcal");
    assert_eq!(ws.moves, "530");
    assert_eq!(ws.efficacy, Some(88));
    assert_eq!(ws.muscle_scores["Chest"], 80);
    assert_eq!(ws.muscle_scores["Triceps"], 40);
    assert_eq!(ws.client_application, "Console");
    assert!(ws.from_tg_coach);
    assert_eq!(ws.exercises.len(), 2);

    let e = &ws.exercises[0];
    assert_eq!(e.activity_id, "ppa-1");
    assert_eq!(e.analytics_id, "an-strength");
    assert_eq!(e.machine, "Chest press #12");
    assert_eq!(e.kind, "StrengthLoad");
    assert_eq!(e.muscles, vec!["Chest", "Triceps"]);
    assert_eq!(e.summary["Rm1"].unit, "kg");
    assert_eq!(e.progress, Some(100));
    assert!(e.has_adjustments);
    assert_eq!(e.estimated.calories, json!(38));
    assert_eq!(e.workload_ranges.len(), 1);
    assert_eq!(e.workload_ranges["IsoWeight"].max, json!(100));
    assert_eq!(e.ext_data.len(), 2);
    assert_eq!(e.ext_data["console"], json!("C1"));
    assert_eq!(e.ext_data["note"], json!("x"));

    assert_eq!(e.sets.len(), 2);
    assert_eq!(e.sets[0].set_number, 1);
    assert_eq!(e.sets[0].reps, Some(10.0));
    assert_eq!(e.sets[0].weight_kg, Some(40.0));
    assert_eq!(e.sets[0].rest_sec, Some(60.0));
    assert_eq!(e.sets[1].reps, Some(8.0));
    assert_eq!(e.sets[1].weight_kg, Some(45.0));
    assert_eq!(e.prescribed_sets[0].reps, Some(12.0));

    let run = &ws.exercises[1];
    assert!(run.is_cardio);
    assert_eq!(run.analytics_id, "an-run");
}

#[test]
fn exercise_analytics_maps_channels_by_descriptor_index() {
    let a = parse_exercise_analytics(&fixture("exercise_analytics"), "an-run");
    assert_eq!(a.analytics_id, "an-run");
    assert_eq!(a.channels, vec!["Speed", "Grade", "HDistance"]);
    assert_eq!(a.samples.len(), 3, "empty value vectors are dropped");
    assert_eq!(a.samples[0].t, 1);
    assert_eq!(a.samples[0].channels["Speed"], Some(10.0));
    assert_eq!(a.samples[0].channels["Grade"], Some(1.0));
    assert_eq!(a.samples[2].channels["Grade"], None);
    assert_eq!(a.hr.len(), 2);
    assert_eq!(a.hr[1].hr, 130);
    assert_eq!(a.hr_zones[0].seconds_in_zone, 30.0);
    assert_eq!(a.laps[0].total_time_sec, Some(320.0));
    assert_eq!(a.summary["AvgSpeed"].unit, "km/h");
    assert_eq!(a.date, "03.09.2026");

    let json = serde_json::to_value(&a.samples[0]).unwrap();
    assert_eq!(
        json,
        json!({ "t": 1, "Speed": 10.0, "Grade": 1.0, "HDistance": 3.0 })
    );
}

#[test]
fn analytics_block_may_be_spelled_correctly_too() {
    let a = parse_exercise_analytics(
        &json!({ "analytics": { "descriptor": [{ "i": 0, "pr": { "name": "Power" } }], "samples": [{ "t": 0, "vs": [5] }] } }),
        "x",
    );
    assert_eq!(a.channels, vec!["Power"]);
    assert_eq!(a.samples[0].channels["Power"], Some(5.0));
}

#[test]
fn hr_session() {
    let hr = parse_hr_session(&fixture("hr_session"), "1001", "2026-09-01");
    assert_eq!(hr.date, "2026-09-01");
    assert_eq!(hr.summary["AvgHr"].formatted, json!("141 bpm"));
    assert_eq!(hr.hr_samples[1].hr, 101);
    assert_eq!(hr.hr_zones[0].name, "Z1");
}
