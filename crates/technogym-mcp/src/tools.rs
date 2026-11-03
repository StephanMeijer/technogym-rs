//! The six read-only tools.

use std::sync::Arc;

use chrono::NaiveDate;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use technogym::dates::{add_days, parse_iso, to_iso, today};
use technogym::parse::{parse_exercise_analytics, parse_workout_session};
use technogym::{
    SegmentBy, SegmentOptions, Technogym, channel_stats, downsample, segment_analytics,
    window_samples,
};

use crate::results::{compact_json, error_result, json_result, text_result};
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

fn date(value: Option<&String>) -> Result<Option<NaiveDate>, ToolError> {
    match value.map(String::as_str).filter(|s| !s.trim().is_empty()) {
        Some(s) => parse_iso(s.trim())
            .map(Some)
            .map_err(|e| ToolError::Input(e.to_string())),
        None => Ok(None),
    }
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

/// The MCP server: six read-only tools over one [`ApiSource`].
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
            Ok(json_result(&value))
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
            let to = date(args.to_date.as_ref())?.unwrap_or_else(today);
            let from = date(args.from_date.as_ref())?.unwrap_or_else(|| add_days(to, -30));
            let sessions = api
                .list_sessions(from, to, args.only_workouts.unwrap_or(true))
                .await?;
            Ok(if sessions.is_empty() {
                text_result(format!(
                    "No workout sessions found between {} and {}",
                    to_iso(from),
                    to_iso(to)
                ))
            } else {
                json_result(&sessions)
            })
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
            Ok(json_result(&value))
        })
        .await
    }

    #[tool(
        name = "get_recent_workouts",
        description = "Every workout session of the last N days, fully loaded (get_workout_sessions + get_workout_session). For splits and time series follow each exercise's analyticsId into get_exercise_analytics.",
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
            let sessions = api.get_recent_workouts(days).await?;
            Ok(if sessions.is_empty() {
                text_result(format!("No workout sessions found in the last {days} days"))
            } else {
                json_result(&sessions)
            })
        })
        .await
    }

    #[tool(
        name = "get_exercise_analytics",
        description = "Granular analytics for one exercise, coarse to fine: summary totals; channelStats (avg/max/min per machine channel over the full 1 Hz data); laps (console auto-laps, per km on a treadmill); segments, by default one per INTERVAL detected from setpoint changes (speed/grade/level), each with start/end, setpoint, pace, distance and avg/max/min of every channel; samples, the time series every 10 s by default; hr and hrZones. Channels are whatever the machine reports (Skillrun: Speed, Grade, HDistance, RunningCadence, RunningPower; bikes: Power, Rpm...). Strength machines have no time series. For interval analysis: call once with defaults, then zoom into one interval with from_sec/to_sec and sample_every_seconds=1.",
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
            let (mode, segments) = segment_analytics(
                &analytics.samples,
                &analytics.channels,
                &analytics.laps,
                options,
            );
            let stats = channel_stats(&analytics.samples, &analytics.channels);
            let samples = downsample(&window_samples(&analytics.samples, from_sec, to_sec), step);
            let hr = downsample(&window_samples(&analytics.hr, from_sec, to_sec), step);

            let mut value = serde_json::to_value(&analytics).unwrap_or(Value::Null);
            value["channelStats"] = json!(stats);
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
            Ok(text_result(compact_json(value, &["samples", "hr"])))
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
            Ok(text_result(compact_json(value, &["hrSamples"])))
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

    #[test]
    fn six_read_only_tools_are_listed() {
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
