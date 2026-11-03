# technogym

Unofficial async Rust client for Technogym's **mywellness** cloud. Reads your gym training
history: workout sessions, every exercise with per-set reps and weight, the machine's full
summary, per-second treadmill and bike data, and whole-session heart rate.

Not affiliated with Technogym. It speaks the JSON API the end-user web app uses and signs in with
your email and password; it will break if Technogym changes the login flow. Read-only.

```sh
cargo add technogym
```

## Quick start

```rust
use technogym::{FileSessionStore, Technogym, dates};

let tg = Technogym::builder()
    .session_store(FileSessionStore::new("./session"))   // session.json, mode 0600
    .build();
if !tg.load_session().await? {
    tg.login("me@example.com", "secret").await?;
}

// Overview, newest first. session_id + date address a session.
let sessions = tg.list_sessions(dates::parse_iso("2026-09-01")?, dates::today(), true).await?;

// Every exercise, sets, machine summary, efficacy and muscle scores.
let first = &sessions[0];
let session = tg.get_workout_session(&first.session_id, Some(dates::parse_iso(&first.date)?)).await?;

// Per-second data of a cardio exercise, plus intervals and per-channel stats.
let run = session.exercises.iter().find(|e| e.is_cardio).unwrap();
let analyzed = tg.analyze_exercise(&run.analytics_id, Default::default()).await?;
for s in &analyzed.segments {
    println!("{:?} {:?} {:?}", s.interval, s.setpoint, s.pace_min_per_km);
}
```

Or give the builder `Credentials` and it signs in lazily, and again whenever the token is
rejected. A mywellness token lives one hour; every response hands out a fresh one and the client
keeps it, so a session in use never expires.

## API

| Method | Returns |
|---|---|
| `user()` | The account: user id, name, culture, measurement system, facilities. No request once signed in. |
| `get_movergy()` | The current Movergy score, or `None`. |
| `list_sessions(from, to, only_workouts)` | `Vec<SessionSummary>`, newest first: id, date, time, name, duration, MOVEs, calories, exercise counts, effectiveness, compliance. |
| `find_session_date(session_id, lookback_days)` | A session's date, found by scanning history. |
| `get_workout_session(session_id, date)` | `WorkoutSession`: every performed exercise with machine, sets, prescribed sets, summary, workload ranges, estimated figures, console metadata; session efficacy, muscle scores, totals. |
| `get_recent_workouts(days)` | `Vec<WorkoutSession>` for the last N days. |
| `get_exercise_analytics(analytics_id)` | `ExerciseAnalytics`: channel names, 1 Hz samples, HR, HR zones, laps, summary. |
| `analyze_exercise(analytics_id, SegmentOptions)` | The above plus `channel_stats`, `segment_mode` and `segments`. |
| `get_session_heart_rate(session_id, date)` | `HrSession`: timestamped HR, zones, avg/min/max HR. |
| `*_raw` twins, `services(path, body)` | The untouched payloads, and any training endpoint the types do not model. |

Pass the `date` whenever you have it: the API partitions sessions by day, and without it the
last year of history is scanned first.

Every metric is a `Property` of `{ value, display_value, unit, formatted }`: `value` is in the
API's base unit (seconds, metres, kg), `display_value` is in `unit` (what the console shows),
`formatted` is the localised string. Types serialise as camelCase JSON.

### Analysis

Pure functions over samples you already hold: `channel_stats`, `segment_analytics`,
`window_samples`, `downsample`. `SegmentBy::Auto` (default) uses intervals detected from setpoint
changes (speed, grade, level) when there are at least two, else console laps, else fixed windows.
A 10 × (1 min hard / 1 min easy) treadmill session yields 20 segments, each with its setpoint,
pace, distance covered and avg / max / min of every channel. Ramps between setpoints are folded
into the interval they lead into.

### Errors

One `Error` enum: `Auth` (login failed, no session, token rejected), `Api` (a 200 whose `errors`
envelope is not about the session), `NotFound`, `RateLimit { retry_after }`,
`Http { status, url, body }`, `Connection`, `InvalidDate`.

Sessions are kept by `FileSessionStore` or `MemorySessionStore`; implement `SessionStore` for a
database.

## License

MPL-2.0. Not affiliated with Technogym; inspired by technogym-js and health_mcp (see NOTICE).
