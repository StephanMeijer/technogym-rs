//! The read-only tools.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Datelike as _, NaiveDate};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use technogym::dates::{add_days, parse_iso, to_iso, today};
use technogym::parse::{parse_exercise_analytics, parse_workout_session};
use technogym::{
    SegmentBy, SegmentOptions, SessionSummary, Technogym, add_heart_rate, channel_stats,
    downsample, exercise_elevation_gain, grade_bands, sample_every, segment_analytics,
    window_samples,
};

use crate::results::{MAX_RESULT_CHARS, compact_json, error_result, json_result, text_result};
use crate::source::ApiSource;

const INSTRUCTIONS: &str = "Read-only access to the user's Technogym mywellness training history: workout sessions, \
per-exercise data with per-set reps/weight, per-second exercise analytics and whole-session heart rate. \
Dates are ISO (YYYY-MM-DD). A session is identified by session_id plus its date. Start with \
get_workout_sessions or get_recent_workouts, then get_workout_session for exercises and sets. \
Session/exercise summaries are averages and maxima only: for intervals, per-km splits and per-second \
speed / cadence / power / grade call get_exercise_analytics with the exercise's analyticsId; zoom into \
one interval with from_sec/to_sec and sample_every_seconds=1 (and get_session_heart_rate for the HR trace).";

/// Why a tool call did not produce data.
enum ToolError {
    /// The caller's arguments.
    Input(String),
    /// mywellness or the network.
    Api(technogym::Error),
}

impl From<technogym::Error> for ToolError {
    fn from(e: technogym::Error) -> Self {
        Self::Api(e)
    }
}

type ToolResult = Result<CallToolResult, ToolError>;

/// Truncation hints, per tool: what to change to get less back.
const DATE_RANGE_HINT: &str = "narrow from_date/to_date";
const ONE_SESSION_HINT: &str = "drop include_raw";
const RECENT_HINT: &str = "reduce days or call get_workout_session for one session";
const SAMPLES_HINT: &str = "narrow from_sec/to_sec or raise sample_every_seconds";
const HR_HINT: &str = "raise sample_every_seconds";

fn date(value: Option<&String>) -> Result<Option<NaiveDate>, ToolError> {
    match value.map(String::as_str).filter(|s| !s.trim().is_empty()) {
        Some(s) => parse_iso(s.trim())
            .map(Some)
            .map_err(|e| ToolError::Input(e.to_string())),
        None => Ok(None),
    }
}

/// `(from, to)` for a range tool: `to` defaults to today, `from` to `default_days` before `to`.
fn date_range(
    from: Option<&String>,
    to: Option<&String>,
    default_days: i64,
) -> Result<(NaiveDate, NaiveDate), ToolError> {
    let to = date(to)?.unwrap_or_else(today);
    let from = date(from)?.unwrap_or_else(|| add_days(to, -default_days));
    Ok((from, to))
}

fn required<'a>(value: Option<&'a String>, name: &str) -> Result<&'a str, ToolError> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::Input(format!("{name} is required")))
}

/// `get_workout_sessions` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct WorkoutSessionsArgs {
    /// Start date, YYYY-MM-DD (default: 30 days before to_date).
    #[serde(default)]
    pub from_date: Option<String>,
    /// End date, YYYY-MM-DD (default: today).
    #[serde(default)]
    pub to_date: Option<String>,
    /// Only gym-floor workout sessions (default true). false includes classes, outdoor, ...
    #[serde(default)]
    pub only_workouts: Option<bool>,
}

/// `get_training_trends` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct TrainingTrendsArgs {
    /// Start date, YYYY-MM-DD (default: 12 weeks before to_date).
    #[serde(default)]
    pub from_date: Option<String>,
    /// End date, YYYY-MM-DD (default: today).
    #[serde(default)]
    pub to_date: Option<String>,
}

/// Sessions per ISO week, oldest first: count, minutes, calories and MOVEs. A week without
/// sessions is left out.
pub fn weekly_totals(sessions: &[SessionSummary]) -> Vec<Value> {
    let mut weeks: BTreeMap<(i32, u32), [i64; 4]> = BTreeMap::new();
    for s in sessions {
        let Ok(day) = parse_iso(&s.date) else {
            continue;
        };
        let week = day.iso_week();
        let w = weeks.entry((week.year(), week.week())).or_default();
        w[0] += 1;
        w[1] += s.duration_min.unwrap_or(0);
        w[2] += s.calories.unwrap_or(0);
        w[3] += s.moves.unwrap_or(0);
    }
    weeks
        .into_iter()
        .map(|((year, week), [sessions, minutes, calories, moves])| {
            json!({
                "week": format!("{year}-W{week:02}"),
                "sessions": sessions,
                "minutes": minutes,
                "calories": calories,
                "moves": moves,
            })
        })
        .collect()
}

/// `get_workout_session` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct WorkoutSessionArgs {
    /// session_id from get_workout_sessions.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The session's date, YYYY-MM-DD. Strongly recommended; without it the last year is scanned.
    #[serde(default)]
    pub date: Option<String>,
    /// Also return the untouched mywellness payload under raw (large).
    #[serde(default)]
    pub include_raw: Option<bool>,
}

/// `get_recent_workouts` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct RecentWorkoutsArgs {
    /// Look-back window in days, 1-365 (default 14).
    #[serde(default)]
    pub days: Option<i64>,
}

/// `get_exercise_analytics` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ExerciseAnalyticsArgs {
    /// An exercise's analyticsId from get_workout_session.
    #[serde(default)]
    pub analytics_id: Option<String>,
    /// Keep every Nth sample (default 10; 1 = full resolution).
    #[serde(default)]
    pub sample_every_seconds: Option<i64>,
    /// auto (default): intervals when there are at least two, else laps, else fixed windows. Or workload, laps, fixed.
    #[serde(default)]
    pub segment_by: Option<String>,
    /// Window length for segment_by=fixed (default 300).
    #[serde(default)]
    pub segment_seconds: Option<i64>,
    /// Only return samples/hr from this second (stats and segments always cover the whole exercise).
    #[serde(default)]
    pub from_sec: Option<i64>,
    /// Only return samples/hr up to this second (0 = end).
    #[serde(default)]
    pub to_sec: Option<i64>,
    /// Also return the untouched payload under raw, minus its per-second samples.
    #[serde(default)]
    pub include_raw: Option<bool>,
}

/// `get_session_heart_rate` arguments.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct SessionHeartRateArgs {
    /// session_id from get_workout_sessions.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The session's date, YYYY-MM-DD (recommended; otherwise found from history).
    #[serde(default)]
    pub date: Option<String>,
    /// Keep every Nth sample (default 10; 1 = all).
    #[serde(default)]
    pub sample_every_seconds: Option<i64>,
}

/// The MCP server: the read-only tools over one [`ApiSource`].
#[derive(Clone)]
pub struct TechnogymMcp {
    source: Arc<dyn ApiSource>,
}

impl TechnogymMcp {
    /// Tools over `source`.
    pub fn new(source: Arc<dyn ApiSource>) -> Self {
        Self { source }
    }

    /// Runs `f` with a signed-in client; input and API failures become error results, never
    /// protocol errors, so the model can read them.
    async fn run<F, Fut>(&self, f: F) -> Result<CallToolResult, McpError>
    where
        F: FnOnce(Arc<Technogym>) -> Fut,
        Fut: Future<Output = ToolResult>,
    {
        let outcome = match self.source.get().await {
            Ok(api) => f(api).await,
            Err(e) => Err(ToolError::Api(e)),
        };
        Ok(match outcome {
            Ok(result) => result,
            Err(ToolError::Input(message)) => error_result(message),
            Err(ToolError::Api(error)) => {
                if matches!(error, technogym::Error::Auth(_)) {
                    // A login made in a terminal since must be picked up by the next call.
                    self.source.reset();
                }
                error_result(crate::errors::describe(&error, self.source.login_hint()))
            }
        })
    }
}

#[tool_router]
impl TechnogymMcp {
    #[tool(
        name = "get_account_info",
        description = "The mywellness account: user id, name, culture, facilities and current Movergy score. Use it to check the login works.",
        annotations(title = "Account info", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_account_info(&self) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let mut value = serde_json::to_value(api.user().await?).unwrap_or(Value::Null);
            value["movergy"] = json!(api.get_movergy().await?);
            Ok(json_result(&value, ""))
        })
        .await
    }

    #[tool(
        name = "get_workout_sessions",
        description = "Workout sessions in a date range, newest first. Each has sessionId and date, which together identify the session for the detail tools, plus name, duration, MOVEs, calories and exercise counts. An overview only: call get_workout_session for exercises and sets, then get_exercise_analytics for splits, intervals and per-second data.",
        annotations(
            title = "List workout sessions",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_workout_sessions(
        &self,
        Parameters(args): Parameters<WorkoutSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let (from, to) = date_range(args.from_date.as_ref(), args.to_date.as_ref(), 30)?;
            let sessions = api
                .list_sessions(from, to, args.only_workouts.unwrap_or(true))
                .await?;
            Ok(if sessions.is_empty() {
                text_result(
                    format!(
                        "No workout sessions found between {} and {}",
                        to_iso(from),
                        to_iso(to)
                    ),
                    DATE_RANGE_HINT,
                )
            } else {
                json_result(&sessions, DATE_RANGE_HINT)
            })
        })
        .await
    }

    #[tool(
        name = "get_training_trends",
        description = "Training volume per ISO week, oldest first: sessions, minutes, calories and MOVEs, from all activity types (gym floor, classes, outdoor). For trends over weeks or months; one request whatever the range. Weeks without sessions are left out. The current Movergy score is in get_account_info.",
        annotations(
            title = "Training trends",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_training_trends(
        &self,
        Parameters(args): Parameters<TrainingTrendsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let (from, to) = date_range(args.from_date.as_ref(), args.to_date.as_ref(), 83)?;
            let sessions = api.list_sessions(from, to, false).await?;
            let value = json!({
                "fromDate": to_iso(from),
                "toDate": to_iso(to),
                "weeks": weekly_totals(&sessions),
            });
            Ok(json_result(&value, DATE_RANGE_HINT))
        })
        .await
    }

    #[tool(
        name = "get_workout_session",
        description = "Full detail of one workout session: every exercise with machine, status, duration, calories, MOVEs, muscles, performed and prescribed sets (reps, kg, rest), the machine's full summary (Skillrun: pace, speed, running cadence/power, step length, ground contact; strength: est. 1RM, total weight...), workload ranges and estimated vs done figures, plus session efficacy, muscle scores and totals. Every property is {value, displayValue, unit, formatted}: value is in the base unit (seconds, metres, kg), unit belongs to displayValue. Summaries are averages and maxima only; for splits and time series call get_exercise_analytics with an exercise's analyticsId.",
        annotations(
            title = "Workout session detail",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_workout_session(
        &self,
        Parameters(args): Parameters<WorkoutSessionArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let id = required(args.session_id.as_ref(), "session_id")?;
            let day = match date(args.date.as_ref())? {
                Some(d) => d,
                None => api.find_session_date(id, 365).await?,
            };
            let raw = api.get_workout_session_raw(id, Some(day)).await?;
            let mut value =
                serde_json::to_value(parse_workout_session(&raw, id)).unwrap_or(Value::Null);
            if args.include_raw.unwrap_or(false) {
                value["raw"] = raw;
            }
            Ok(json_result(&value, ONE_SESSION_HINT))
        })
        .await
    }

    #[tool(
        name = "get_recent_workouts",
        description = "The workout sessions of the last N days, newest first and fully loaded (get_workout_sessions + get_workout_session), as many as fit one result; omitted says how many older ones were left out. For splits and time series follow each exercise's analyticsId into get_exercise_analytics.",
        annotations(
            title = "Recent workouts",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_recent_workouts(
        &self,
        Parameters(args): Parameters<RecentWorkoutsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let days = args.days.unwrap_or(14).clamp(1, 365);
            let to = today();
            let summaries = api.list_sessions(add_days(to, -days), to, true).await?;
            if summaries.is_empty() {
                return Ok(text_result(
                    format!("No workout sessions found in the last {days} days"),
                    RECENT_HINT,
                ));
            }
            // One request per session, newest first, stopping at the result budget: a session
            // that would only be cut off again is never downloaded, and the JSON stays whole.
            let budget = MAX_RESULT_CHARS * 9 / 10;
            let mut sessions = vec![];
            let mut used = 0;
            for s in &summaries {
                let mut session = api
                    .get_workout_session(&s.session_id, Some(parse_iso(&s.date)?))
                    .await?;
                // Machine limits (65535 kcal and the like) repeat on every exercise and say
                // nothing about the workout; get_workout_session still has them.
                for e in &mut session.exercises {
                    e.workload_ranges.clear();
                }
                // As it will appear: inside `{ "sessions": [ … ] }` every line is indented 4 more.
                let size = serde_json::to_string_pretty(&session)
                    .map_or(0, |t| t.chars().count() + 4 * t.lines().count());
                if !sessions.is_empty() && used + size > budget {
                    break;
                }
                used += size;
                sessions.push(session);
            }
            let omitted = summaries.len() - sessions.len();
            let mut value = json!({ "days": days, "sessions": sessions, "omitted": omitted });
            if omitted > 0 {
                value["note"] = json!(format!(
                    "{omitted} older session(s) left out to stay under the result limit; {RECENT_HINT}"
                ));
            }
            Ok(json_result(&value, RECENT_HINT))
        })
        .await
    }

    #[tool(
        name = "get_exercise_analytics",
        description = "Granular analytics for one exercise, coarse to fine: summary totals; channelStats (time-weighted avg, max, min per machine channel over the whole exercise); laps (console auto-laps, per km on a treadmill); segments, by default one per INTERVAL detected from setpoint changes (speed/grade/level), each with start/end, setpoint, pace, distance and avg/max/min of every channel; samples, the time series as the readings in effect every 10 s by default (rows are change-driven: a value holds until the next row); hr (strap dropouts removed) and hrStats (time-weighted avg, min, max and coverage: the share of the exercise with a reading; also per segment) and hrZones; elevationGainM and gradeBands (seconds moving below 5 %, 5-10 % and 10 %+ grade); per segment also elevationGainM, acsmVo2 (oxygen cost of its speed and grade) and hrPerVo2 (heart rate per unit of that work: compare across segments for cardiac drift, across weeks for fitness). Channels are whatever the machine reports (Skillrun: Speed, Grade, HDistance, RunningCadence, RunningPower; bikes: Power, Rpm...). Strength machines have no time series. For interval analysis: call once with defaults, then zoom into one interval with from_sec/to_sec and sample_every_seconds=1.",
        annotations(
            title = "Exercise analytics",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_exercise_analytics(
        &self,
        Parameters(args): Parameters<ExerciseAnalyticsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let id = required(args.analytics_id.as_ref(), "analytics_id")?;
            let segment_by: SegmentBy = args
                .segment_by
                .as_deref()
                .unwrap_or("auto")
                .parse()
                .map_err(ToolError::Input)?;
            let step = usize::try_from(args.sample_every_seconds.unwrap_or(10))
                .unwrap_or(1)
                .max(1);
            let from_sec = args.from_sec.unwrap_or(0);
            let to_sec = args.to_sec.unwrap_or(0);

            let mut raw = api.get_exercise_analytics_raw(id).await?;
            let analytics = parse_exercise_analytics(&raw, id);
            let options = SegmentOptions {
                segment_by,
                segment_seconds: args.segment_seconds,
            };
            let (mode, mut segments) = segment_analytics(
                &analytics.samples,
                &analytics.channels,
                &analytics.laps,
                options,
            );
            let stats = channel_stats(&analytics.samples, &analytics.channels);
            let hr_stats = add_heart_rate(&analytics, &mut segments);
            let samples = sample_every(&window_samples(&analytics.samples, from_sec, to_sec), step);
            let hr = sample_every(&window_samples(&analytics.hr, from_sec, to_sec), step);

            let mut value = serde_json::to_value(&analytics).unwrap_or(Value::Null);
            value["channelStats"] = json!(stats);
            value["hrStats"] = json!(hr_stats);
            value["elevationGainM"] = json!(exercise_elevation_gain(&analytics.samples));
            value["gradeBands"] = json!(grade_bands(&analytics.samples));
            value["segmentMode"] = json!(mode);
            value["segments"] = json!(segments);
            value["sampleCount"] = json!(samples.len());
            value["hrCount"] = json!(hr.len());
            value["samples"] = json!(samples);
            value["hr"] = json!(hr);
            if from_sec != 0 || to_sec != 0 {
                let to = if to_sec == 0 {
                    Value::Null
                } else {
                    json!(to_sec)
                };
                value["window"] = json!({ "fromSec": from_sec, "toSec": to });
            }
            if args.include_raw.unwrap_or(false) {
                if let Some(count) = raw["analitics"]["samples"].as_array().map(Vec::len) {
                    raw["analitics"]["samples"] =
                        json!(format!("<{count} samples omitted; see samples>"));
                }
                value["raw"] = raw;
            }
            Ok(text_result(
                compact_json(value, &["samples", "hr"]),
                SAMPLES_HINT,
            ))
        })
        .await
    }

    #[tool(
        name = "get_session_heart_rate",
        description = "The whole-session heart-rate trace: timestamped HR samples, HR zones and the session summary (avg/min/max HR, calories, MOVEs, METs).",
        annotations(
            title = "Session heart rate",
            read_only_hint = true,
            open_world_hint = true
        )
    )]
    async fn get_session_heart_rate(
        &self,
        Parameters(args): Parameters<SessionHeartRateArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.run(|api| async move {
            let id = required(args.session_id.as_ref(), "session_id")?;
            let hr = api
                .get_session_heart_rate(id, date(args.date.as_ref())?)
                .await?;
            let step = usize::try_from(args.sample_every_seconds.unwrap_or(10))
                .unwrap_or(1)
                .max(1);
            let samples = downsample(&hr.hr_samples, step);
            let mut value = serde_json::to_value(&hr).unwrap_or(Value::Null);
            value["sampleCount"] = json!(samples.len());
            value["hrSamples"] = json!(samples);
            Ok(text_result(compact_json(value, &["hrSamples"]), HR_HINT))
        })
        .await
    }
}

#[tool_handler]
impl ServerHandler for TechnogymMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("technogym-mcp", env!("CARGO_PKG_VERSION"))
                    .with_title("Technogym mywellness (unofficial)")
                    .with_website_url(env!("CARGO_PKG_REPOSITORY")),
            )
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(date: &str, minutes: i64, moves: i64) -> SessionSummary {
        SessionSummary {
            date: date.into(),
            duration_min: Some(minutes),
            moves: Some(moves),
            ..SessionSummary::default()
        }
    }

    #[test]
    fn weekly_totals_group_by_iso_week_oldest_first() {
        // 2026-09-06 is a Sunday: it belongs to the week of Monday 2026-08-31.
        let weeks = weekly_totals(&[
            summary("2026-09-08", 45, 300),
            summary("2026-09-06", 30, 200),
            summary("2026-08-31", 20, 100),
            summary("not a date", 99, 99),
        ]);
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0]["week"], "2026-W36");
        assert_eq!(weeks[0]["sessions"], 2);
        assert_eq!(weeks[0]["minutes"], 50);
        assert_eq!(weeks[0]["moves"], 300);
        assert_eq!(weeks[1]["week"], "2026-W37");
        assert_eq!(weeks[1]["calories"], 0, "missing figures count as 0");
    }

    #[test]
    fn seven_read_only_tools_are_listed() {
        let mut names: Vec<String> = TechnogymMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "get_account_info",
                "get_exercise_analytics",
                "get_recent_workouts",
                "get_session_heart_rate",
                "get_training_trends",
                "get_workout_session",
                "get_workout_sessions",
            ]
        );
        for tool in TechnogymMcp::tool_router().list_all() {
            assert_eq!(
                tool.annotations.as_ref().and_then(|a| a.read_only_hint),
                Some(true),
                "{}",
                tool.name
            );
        }
    }
}
