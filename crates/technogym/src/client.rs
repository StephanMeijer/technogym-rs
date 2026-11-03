//! [`Technogym`]: sign-in, session handling, the authenticated transport and one method per
//! endpoint.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Map, Value, json};

use crate::analysis::{
    ChannelStats, Segment, SegmentMode, SegmentOptions, channel_stats, segment_analytics,
};
use crate::constants::{CORE_URL, LOGIN_PATH, SERVICES_URL, app_headers};
use crate::dates::{add_days, to_iso, to_partition_key, today};
use crate::error::{Error, Result};
use crate::parse::{
    parse_exercise_analytics, parse_hr_session, parse_session_summary, parse_user,
    parse_workout_session,
};
use crate::session::{MemorySessionStore, Session, SessionStore};
use crate::transport::{Fetcher, TransportOptions};
use crate::types::{
    ExerciseAnalytics, Facility, HrSession, SessionSummary, UserInfo, WorkoutSession,
};

const HISTORY_PATH: &str = "/Training/User/{userId}/ActivityHistory";
const SESSION_PATH: &str = "/Training/User/{userId}/GetPerformedWorkoutSessionByIdCr";
const HR_SESSION_PATH: &str = "/Training/User/{userId}/GetHrSession";
const MOVERGY_PATH: &str = "/Training/User/{userId}/MyMovergy";

/// mywellness `details` values that mean "the session is gone", not "the call is wrong".
const AUTH_ERROR_DETAILS: &[&str] = &[
    "TokenNotValid",
    "InvalidToken",
    "TokenExpired",
    "NotAuthenticated",
    "Unauthorized",
    "UserNotLogged",
];

/// An email and password for mywellness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    /// The account's email.
    pub email: String,
    /// The account's password.
    pub password: String,
}

impl Credentials {
    /// A pair.
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
        }
    }
}

/// [`get_exercise_analytics`](Technogym::get_exercise_analytics) plus the derived stats and
/// segments of [`analyze_exercise`](Technogym::analyze_exercise).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzedExercise {
    /// The analytics.
    #[serde(flatten)]
    pub analytics: ExerciseAnalytics,
    /// Per-channel avg / max / min over the full 1 Hz data.
    pub channel_stats: std::collections::BTreeMap<String, ChannelStats>,
    /// How the exercise was split.
    pub segment_mode: SegmentMode,
    /// The segments.
    pub segments: Vec<Segment>,
}

/// Whether an envelope's `errors` describe a rejected session.
pub fn is_auth_error(errors: &[Value]) -> bool {
    errors.iter().any(|e| {
        let details = e
            .get("details")
            .or_else(|| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        AUTH_ERROR_DETAILS.contains(&details)
    })
}

fn errors_text(errors: &[Value]) -> String {
    errors
        .iter()
        .map(|e| {
            ["errorMessage", "message", "details"]
                .iter()
                .find_map(|k| e.get(*k).and_then(Value::as_str))
                .map_or_else(|| e.to_string(), str::to_string)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Parses a JSON response body; a non-JSON 200 is an error, never a panic.
async fn parse_envelope(res: reqwest::Response) -> Result<Map<String, Value>> {
    let text = res
        .text()
        .await
        .map_err(|e| Error::Other(format!("reading the response failed: {e}")))?;
    if text.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Ok(Map::from_iter([("data".to_string(), other)])),
        Err(_) => Err(Error::Other(
            "mywellness returned a non-JSON response".into(),
        )),
    }
}

struct State {
    session: Option<Session>,
    credentials: Option<Credentials>,
}

/// Builds a [`Technogym`]. Every setting is optional.
pub struct TechnogymBuilder {
    store: Option<Box<dyn SessionStore>>,
    credentials: Option<Credentials>,
    facility: Option<String>,
    transport: TransportOptions,
    core_url: String,
    services_url: String,
}

impl Default for TechnogymBuilder {
    fn default() -> Self {
        Self {
            store: None,
            credentials: None,
            facility: None,
            transport: TransportOptions::default(),
            core_url: CORE_URL.to_string(),
            services_url: SERVICES_URL.to_string(),
        }
    }
}

impl TechnogymBuilder {
    /// Where the session is loaded from and saved to. Default: a [`MemorySessionStore`].
    pub fn session_store(mut self, store: impl SessionStore + 'static) -> Self {
        self.store = Some(Box::new(store));
        self
    }

    /// Sign in lazily on the first call and again whenever mywellness rejects the token.
    /// Without them, call [`Technogym::login`] or [`Technogym::load_session`] first, and a
    /// rejected token is an [`Error::Auth`].
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Which facility's url prefixes training paths, matched against a facility's `id` or `url`.
    /// Default: the account's first facility.
    pub fn facility(mut self, facility: impl Into<String>) -> Self {
        self.facility = Some(facility.into());
        self
    }

    /// Timeout, retries and backoff.
    pub fn transport(mut self, options: TransportOptions) -> Self {
        self.transport = options;
        self
    }

    /// The login host, for tests and proxies. Default [`CORE_URL`].
    pub fn core_url(mut self, url: impl Into<String>) -> Self {
        self.core_url = url.into();
        self
    }

    /// The training-data host, for tests and proxies. Default [`SERVICES_URL`].
    pub fn services_url(mut self, url: impl Into<String>) -> Self {
        self.services_url = url.into();
        self
    }

    /// The client, not signed in yet.
    pub fn build(self) -> Technogym {
        Technogym {
            store: self
                .store
                .unwrap_or_else(|| Box::new(MemorySessionStore::default())),
            fetcher: Fetcher::new(self.transport),
            facility: self.facility,
            core_url: self.core_url.trim_end_matches('/').to_string(),
            services_url: self.services_url.trim_end_matches('/').to_string(),
            state: Mutex::new(State {
                session: None,
                credentials: self.credentials,
            }),
            sign_in_lock: tokio::sync::Mutex::new(()),
        }
    }
}

/// The Technogym mywellness API. Signs in, keeps the session, and offers one method per
/// endpoint plus [`services`](Technogym::services) for anything else.
///
/// ```no_run
/// # async fn demo() -> technogym::Result<()> {
/// use technogym::{FileSessionStore, Technogym, dates};
///
/// let tg = Technogym::builder().session_store(FileSessionStore::new("./session")).build();
/// if !tg.load_session().await? {
///     tg.login("me@example.com", "secret").await?;
/// }
/// let sessions = tg.list_sessions(dates::parse_iso("2026-09-01")?, dates::today(), true).await?;
/// let detail = tg.get_workout_session(&sessions[0].session_id, Some(dates::parse_iso(&sessions[0].date)?)).await?;
/// println!("{} exercises", detail.exercises.len());
/// # Ok(()) }
/// ```
///
/// A session is addressed by its `session_id` AND its date: the API partitions sessions by day.
/// Read-only: nothing here changes the account.
pub struct Technogym {
    store: Box<dyn SessionStore>,
    fetcher: Fetcher,
    facility: Option<String>,
    core_url: String,
    services_url: String,
    state: Mutex<State>,
    /// Concurrent sign-ins collapse into one login request.
    sign_in_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Technogym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Technogym")
            .field("facility", &self.facility)
            .finish_non_exhaustive()
    }
}

impl Technogym {
    /// A builder with every default.
    pub fn builder() -> TechnogymBuilder {
        TechnogymBuilder::default()
    }

    /// Signs in with email and password and saves the session to the store. The credentials are
    /// kept in memory so a rejected token can be renewed without asking again.
    pub async fn login(&self, email: &str, password: &str) -> Result<Session> {
        self.state.lock().expect("state").credentials = Some(Credentials::new(email, password));
        let _guard = self.sign_in_lock.lock().await;
        self.sign_in().await
    }

    /// Loads the saved session. `false` when the store holds none.
    pub async fn load_session(&self) -> Result<bool> {
        match self.store.load().await? {
            Some(session) => {
                self.set_session(session);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Uses this session from now on. Does not save it to the store.
    pub fn set_session(&self, session: Session) {
        self.state.lock().expect("state").session = Some(session);
    }

    /// The session currently held in memory, or `None` before a login or `load_session`.
    pub fn session(&self) -> Option<Session> {
        self.state.lock().expect("state").session.clone()
    }

    /// Forgets the session and the in-memory credentials, and clears the store.
    pub async fn logout(&self) -> Result<()> {
        {
            let mut state = self.state.lock().expect("state");
            state.session = None;
            state.credentials = None;
        }
        self.store.clear().await
    }

    /// The signed-in account. Signs in first when the client was given credentials and holds no
    /// session yet; otherwise [`Error::Auth`].
    pub async fn user(&self) -> Result<UserInfo> {
        Ok(self.require_session().await?.user)
    }

    /// The facility training paths are sent to, or `None` for an account with no facility (its
    /// paths go out unprefixed, which the API accepts).
    pub fn facility_of(&self, user: &UserInfo) -> Result<Option<Facility>> {
        let Some(wanted) = &self.facility else {
            return Ok(user.facilities.first().cloned());
        };
        user.facilities
            .iter()
            .find(|f| &f.id == wanted || &f.url == wanted)
            .cloned()
            .map(Some)
            .ok_or_else(|| Error::Other(format!("the account has no facility {wanted:?}")))
    }

    fn credentials(&self) -> Option<Credentials> {
        self.state.lock().expect("state").credentials.clone()
    }

    async fn require_session(&self) -> Result<Session> {
        if let Some(session) = self.session() {
            return Ok(session);
        }
        if self.credentials().is_none() {
            return Err(Error::Auth(
                "not signed in: call login() or load_session() first".into(),
            ));
        }
        let _guard = self.sign_in_lock.lock().await;
        // Another call may have signed in while this one waited for the lock.
        match self.session() {
            Some(session) => Ok(session),
            None => self.sign_in().await,
        }
    }

    /// Signs in with the stored credentials. Callers hold `sign_in_lock`.
    async fn sign_in(&self) -> Result<Session> {
        let credentials = self
            .credentials()
            .ok_or_else(|| Error::Auth("no credentials to sign in with".into()))?;
        let session = self.login_request(&credentials).await?;
        self.set_session(session.clone());
        self.store.save(&session).await?;
        Ok(session)
    }

    async fn login_request(&self, credentials: &Credentials) -> Result<Session> {
        let body = json!({ "username": credentials.email, "password": credentials.password, "keepMeLoggedIn": true });
        let url = format!("{}{LOGIN_PATH}", self.core_url);
        let res = self
            .fetcher
            .post_json(&url, app_headers(), &body, false)
            .await
            .map_err(|e| match e {
                Error::Auth(m) => Error::Auth(format!("login failed: {m}")),
                other => other,
            })?;
        let envelope = parse_envelope(res).await?;
        if let Some(errors) = envelope
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            return Err(Error::Auth(format!(
                "login failed: {}",
                errors_text(errors)
            )));
        }
        let data = envelope
            .get("data")
            .filter(|d| d.is_object())
            .cloned()
            .unwrap_or(Value::Object(envelope));
        let token = data
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        match (token, parse_user(&data)) {
            (Some(token), Some(user)) => Ok(Session {
                token: token.to_string(),
                user,
                saved_at: now_ms(),
            }),
            _ => Err(Error::Auth(format!(
                "login failed: unexpected response (result={})",
                data["result"]
            ))),
        }
    }

    /// POSTs `body` to a training endpoint and returns the envelope's `data` (or the whole body
    /// when it has none). `path` is relative to the facility, e.g.
    /// `/Training/User/{userId}/MyMovergy`; `{userId}` is replaced with the signed-in user's id.
    /// For endpoints this type has no method for.
    ///
    /// A rejected session is renewed once, when the client has credentials, and the call
    /// repeated.
    pub async fn services(&self, path: &str, body: Value) -> Result<Value> {
        let session = self.require_session().await?;
        match self.services_once(&session, path, &body).await {
            Err(Error::Auth(_)) if self.credentials().is_some() => {
                let renewed = self.renew(&session.token).await?;
                self.services_once(&renewed, path, &body).await
            }
            other => other,
        }
    }

    /// A fresh session, unless another call already renewed the one with `rejected_token`.
    async fn renew(&self, rejected_token: &str) -> Result<Session> {
        let _guard = self.sign_in_lock.lock().await;
        match self.session() {
            Some(current) if current.token != rejected_token => Ok(current),
            _ => self.sign_in().await,
        }
    }

    async fn services_once(&self, session: &Session, path: &str, body: &Value) -> Result<Value> {
        let facility = self.facility_of(&session.user)?;
        let resolved = path.replace("{userId}", &session.user.user_id);
        let prefix = facility
            .filter(|f| !f.url.is_empty())
            .map(|f| format!("/{}", f.url))
            .unwrap_or_default();
        let url = format!("{}{prefix}{resolved}", self.services_url);
        let mut headers = app_headers();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", session.token))
                .map_err(|_| Error::Auth("the session token is not a valid header value".into()))?,
        );
        let res = self.fetcher.post_json(&url, headers, body, true).await?;
        let rotated = res
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(|t| t.trim().to_string());
        let mut envelope = parse_envelope(res).await?;
        let action = resolved.rsplit('/').next().unwrap_or(&resolved).to_string();
        if let Some(errors) = envelope
            .get("errors")
            .and_then(Value::as_array)
            .filter(|e| !e.is_empty())
        {
            let message = errors_text(errors);
            if is_auth_error(errors) {
                return Err(Error::Auth(format!(
                    "{action}: mywellness rejected the session ({message})"
                )));
            }
            return Err(Error::Api {
                action,
                message,
                errors: errors.clone(),
            });
        }
        let from_envelope = envelope
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.adopt_rotated_token(session, rotated.filter(|t| !t.is_empty()).or(from_envelope))
            .await?;
        Ok(match envelope.remove("data") {
            Some(data) => data,
            None => Value::Object(envelope),
        })
    }

    /// A mywellness token lives one hour, and responses hand out a fresh one in the
    /// `authorization` header (and often the envelope's `token`). Keeping it is what lets a
    /// session outlive the hour while it is in use.
    async fn adopt_rotated_token(&self, session: &Session, token: Option<String>) -> Result<()> {
        let Some(token) = token.filter(|t| !t.is_empty() && *t != session.token) else {
            return Ok(());
        };
        let updated = {
            let mut state = self.state.lock().expect("state");
            // A newer session may already have replaced this one.
            if state
                .session
                .as_ref()
                .is_none_or(|s| s.token != session.token)
            {
                return Ok(());
            }
            let updated = Session {
                token,
                user: session.user.clone(),
                saved_at: now_ms(),
            };
            state.session = Some(updated.clone());
            updated
        };
        self.store.save(&updated).await
    }

    // ---- Endpoints -------------------------------------------------------------------------

    /// The account's current Movergy score, or `None` when mywellness has none.
    pub async fn get_movergy(&self) -> Result<Option<f64>> {
        let data = self.services(MOVERGY_PATH, json!({})).await?;
        Ok(data.get("movergy").and_then(Value::as_f64))
    }

    /// The untouched `ActivityHistory` payload for a date range (bounds in either order).
    pub async fn get_activity_history_raw(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        only_workouts: bool,
    ) -> Result<Value> {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        let mut body =
            json!({ "startDay": to_partition_key(start), "endDay": to_partition_key(end) });
        if only_workouts {
            body["justThisType"] = json!("WorkoutSession");
        }
        self.services(HISTORY_PATH, body).await
    }

    /// Sessions between two dates (inclusive, either order), newest first. An overview only.
    /// `only_workouts` keeps gym-floor workout sessions; `false` includes classes, outdoor, ...
    pub async fn list_sessions(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        only_workouts: bool,
    ) -> Result<Vec<SessionSummary>> {
        let data = self
            .get_activity_history_raw(from, to, only_workouts)
            .await?;
        let mut sessions: Vec<SessionSummary> = data
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(parse_session_summary).collect())
            .unwrap_or_default();
        sessions.sort_by(|a, b| (&b.date, &b.time).cmp(&(&a.date, &a.time)));
        Ok(sessions)
    }

    /// The date of a session, found by scanning the last `lookback_days` of history across every
    /// activity type. [`Error::NotFound`] when it is not there.
    pub async fn find_session_date(
        &self,
        session_id: &str,
        lookback_days: i64,
    ) -> Result<NaiveDate> {
        let end = today();
        let sessions = self
            .list_sessions(add_days(end, -lookback_days), end, false)
            .await?;
        match sessions.iter().find(|s| s.session_id == session_id) {
            Some(s) => crate::dates::parse_iso(&s.date),
            None => Err(Error::NotFound(format!(
                "session {session_id} not found in the last {lookback_days} days"
            ))),
        }
    }

    async fn resolve_date(&self, session_id: &str, date: Option<NaiveDate>) -> Result<NaiveDate> {
        match date {
            Some(d) => Ok(d),
            None => self.find_session_date(session_id, 365).await,
        }
    }

    /// The untouched `GetPerformedWorkoutSessionByIdCr` payload. Pass `date` whenever you have
    /// it; without it the last year is scanned for the session.
    pub async fn get_workout_session_raw(
        &self,
        session_id: &str,
        date: Option<NaiveDate>,
    ) -> Result<Value> {
        let day = self.resolve_date(session_id, date).await?;
        let body = json!({ "idCr": id_cr(session_id)?, "partitionDate": to_partition_key(day) });
        let data = self.services(SESSION_PATH, body).await?;
        if data.get("id").is_none_or(Value::is_null) {
            return Err(Error::NotFound(format!(
                "session {session_id} on {} not found",
                to_iso(day)
            )));
        }
        Ok(data)
    }

    /// Every exercise of a session with machine, sets (performed and prescribed), the machine's
    /// full summary and workload ranges, plus efficacy, muscle scores and totals. Summaries are
    /// averages and maxima only: for splits and time series, call
    /// [`get_exercise_analytics`](Self::get_exercise_analytics) with an exercise's `analytics_id`.
    pub async fn get_workout_session(
        &self,
        session_id: &str,
        date: Option<NaiveDate>,
    ) -> Result<WorkoutSession> {
        Ok(parse_workout_session(
            &self.get_workout_session_raw(session_id, date).await?,
            session_id,
        ))
    }

    /// Every workout session of the last `days` days (1-365), fully loaded, newest first. One
    /// request for the list plus one per session, sequential on purpose: a burst of parallel
    /// calls is the fastest way to get throttled by an unofficial API.
    pub async fn get_recent_workouts(&self, days: i64) -> Result<Vec<WorkoutSession>> {
        let n = days.clamp(1, 365);
        let end = today();
        let items = self.list_sessions(add_days(end, -n), end, true).await?;
        let mut sessions = Vec::with_capacity(items.len());
        for item in items {
            sessions.push(
                self.get_workout_session(
                    &item.session_id,
                    Some(crate::dates::parse_iso(&item.date)?),
                )
                .await?,
            );
        }
        Ok(sessions)
    }

    /// The untouched `CardioLog/{analyticsId}/Details` payload.
    pub async fn get_exercise_analytics_raw(&self, analytics_id: &str) -> Result<Value> {
        if analytics_id.is_empty() {
            return Err(Error::Other("analytics_id is required".into()));
        }
        // shortcut: the id goes into the path unescaped; mywellness ids are GUID-like. Percent-encode if one ever carries '/' or '?'.
        let data = self
            .services(
                &format!("/Training/CardioLog/{analytics_id}/Details"),
                json!({}),
            )
            .await?;
        if data.as_object().is_none_or(Map::is_empty) {
            return Err(Error::NotFound(format!("no analytics for {analytics_id}")));
        }
        Ok(data)
    }

    /// Per-second analytics of one exercise: the machine's channels (Skillrun: Speed, Grade,
    /// `HDistance`, `RunningCadence`, `RunningPower`; bikes: `Power`, `Rpm`...), heart rate, HR zones, laps
    /// and summary. Strength machines have no time series; use the sets of a workout session.
    pub async fn get_exercise_analytics(&self, analytics_id: &str) -> Result<ExerciseAnalytics> {
        Ok(parse_exercise_analytics(
            &self.get_exercise_analytics_raw(analytics_id).await?,
            analytics_id,
        ))
    }

    /// [`get_exercise_analytics`](Self::get_exercise_analytics) plus per-channel stats and
    /// segments: by default one per interval detected from setpoint changes (speed / grade /
    /// level), else per console lap, else fixed windows.
    pub async fn analyze_exercise(
        &self,
        analytics_id: &str,
        options: SegmentOptions,
    ) -> Result<AnalyzedExercise> {
        let analytics = self.get_exercise_analytics(analytics_id).await?;
        let (segment_mode, segments) = segment_analytics(
            &analytics.samples,
            &analytics.channels,
            &analytics.laps,
            options,
        );
        let channel_stats = channel_stats(&analytics.samples, &analytics.channels);
        Ok(AnalyzedExercise {
            analytics,
            channel_stats,
            segment_mode,
            segments,
        })
    }

    /// The untouched `GetHrSession` payload.
    pub async fn get_session_heart_rate_raw(
        &self,
        session_id: &str,
        date: Option<NaiveDate>,
    ) -> Result<Value> {
        let day = self.resolve_date(session_id, date).await?;
        // A NUMBER here, unlike the string partitionDate of GetPerformedWorkoutSessionByIdCr.
        let partition: i64 = to_partition_key(day)
            .parse()
            .map_err(|_| Error::InvalidDate(to_iso(day)))?;
        let data = self
            .services(
                HR_SESSION_PATH,
                json!({ "idCr": id_cr(session_id)?, "partitionDate": partition }),
            )
            .await?;
        if data.as_object().is_none_or(Map::is_empty) {
            return Err(Error::NotFound(format!(
                "no heart-rate data for session {session_id}"
            )));
        }
        Ok(data)
    }

    /// The whole-session heart-rate trace: timestamped samples, HR zones and avg/min/max HR.
    pub async fn get_session_heart_rate(
        &self,
        session_id: &str,
        date: Option<NaiveDate>,
    ) -> Result<HrSession> {
        let day = self.resolve_date(session_id, date).await?;
        let raw = self
            .get_session_heart_rate_raw(session_id, Some(day))
            .await?;
        Ok(parse_hr_session(&raw, session_id, &to_iso(day)))
    }
}

fn id_cr(session_id: &str) -> Result<i64> {
    session_id
        .trim()
        .parse()
        .map_err(|_| Error::Other(format!("session_id must be an integer, got {session_id:?}")))
}
