//! The six tools over a saved session, driven by a real MCP client across an in-memory pipe,
//! against a mocked mywellness.

use std::sync::Arc;

use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use technogym::{Credentials, FileSessionStore, Session, SessionStore as _};
use technogym_mcp::{FileSource, LOGIN_HINT, TechnogymMcp};
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LOGIN: &str = "/v2/enduser/authentication/login";
const USER: &str = "/my-gym/Training/User/u1";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../technogym/tests/fixtures/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).unwrap()
}

fn saved_session(token: &str) -> Session {
    Session {
        token: token.into(),
        user: technogym::parse::parse_user(&fixture("login")["data"]).unwrap(),
        saved_at: 1,
    }
}

fn data(v: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "data": v }))
}

/// mywellness: `tok-1` is good, `stale` is rejected, and the login hands out `tok-1`.
async fn mywellness() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path(LOGIN))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("login")))
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer stale"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "errors": [{ "details": "TokenExpired" }] })),
        )
        .mount(&server)
        .await;
    Mock::given(path(format!("{USER}/MyMovergy")))
        .respond_with(data(&json!({ "movergy": 77 })))
        .mount(&server)
        .await;
    Mock::given(path(format!("{USER}/ActivityHistory")))
        .respond_with(|req: &Request| {
            let body: Value = req.body_json().unwrap();
            if body["startDay"] == "20260901" {
                data(&fixture("activity_history"))
            } else {
                data(&json!({ "items": [] }))
            }
        })
        .mount(&server)
        .await;
    Mock::given(path(format!("{USER}/GetPerformedWorkoutSessionByIdCr")))
        .respond_with(data(&fixture("workout_session")))
        .mount(&server)
        .await;
    Mock::given(path(format!("{USER}/GetHrSession")))
        .respond_with(data(&fixture("hr_session")))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/Training/CardioLog/an-run/Details"))
        .respond_with(data(&fixture("exercise_analytics")))
        .mount(&server)
        .await;
    server
}

struct Harness {
    client: RunningService<RoleClient, ()>,
    source: Arc<FileSource>,
    dir: tempfile::TempDir,
}

async fn harness(
    server: &MockServer,
    saved: Option<Session>,
    credentials: Option<Credentials>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    if let Some(s) = saved {
        FileSessionStore::new(dir.path()).save(&s).await.unwrap();
    }
    let source = Arc::new(
        FileSource::new(dir.path(), credentials, None).with_base_urls(server.uri(), server.uri()),
    );
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let handler = TechnogymMcp::new(source.clone());
    tokio::spawn(async move {
        let running = handler.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();
    Harness {
        client,
        source,
        dir,
    }
}

async fn call(h: &Harness, name: &'static str, args: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name);
    params.arguments = args.as_object().cloned();
    h.client.call_tool(params).await.unwrap()
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

fn parsed(result: &CallToolResult) -> Value {
    serde_json::from_str(&text(result)).unwrap_or_else(|e| panic!("{e}: {}", text(result)))
}

#[tokio::test]
async fn every_tool_answers_over_a_saved_session() {
    let server = mywellness().await;
    let h = harness(&server, Some(saved_session("tok-1")), None).await;

    let tools = h.client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 6);

    let account = parsed(&call(&h, "get_account_info", json!({})).await);
    assert_eq!(account["userId"], "u1");
    assert_eq!(account["movergy"], 77.0);

    let sessions = parsed(
        &call(
            &h,
            "get_workout_sessions",
            json!({ "from_date": "2026-09-01", "to_date": "2026-09-30" }),
        )
        .await,
    );
    assert_eq!(sessions.as_array().unwrap().len(), 2);
    assert_eq!(sessions[0]["sessionId"], "1002", "newest first");

    let none = call(
        &h,
        "get_workout_sessions",
        json!({ "from_date": "2020-01-01", "to_date": "2020-01-02" }),
    )
    .await;
    assert_eq!(
        text(&none),
        "No workout sessions found between 2020-01-01 and 2020-01-02"
    );

    let session = parsed(
        &call(
            &h,
            "get_workout_session",
            json!({ "session_id": "1001", "date": "2026-09-01", "include_raw": true }),
        )
        .await,
    );
    assert_eq!(session["exercises"].as_array().unwrap().len(), 2);
    assert_eq!(session["raw"]["id"], "ws-1");

    let analytics = call(
        &h,
        "get_exercise_analytics",
        json!({ "analytics_id": "an-run", "sample_every_seconds": 1, "include_raw": true }),
    )
    .await;
    let a = parsed(&analytics);
    assert_eq!(a["sampleCount"], 3);
    assert_eq!(a["segmentMode"], "laps");
    assert_eq!(a["channelStats"]["Speed"]["max"], 12.0);
    assert_eq!(
        a["raw"]["analitics"]["samples"],
        "<4 samples omitted; see samples>"
    );
    assert!(
        text(&analytics)
            .contains("\n    {\"Grade\":1.0,\"HDistance\":3.0,\"Speed\":10.0,\"t\":1},\n"),
        "rows stay compact"
    );
    let windowed = parsed(
        &call(
            &h,
            "get_exercise_analytics",
            json!({ "analytics_id": "an-run", "from_sec": 2, "to_sec": 2, "segment_by": "FIXED" }),
        )
        .await,
    );
    assert_eq!(windowed["sampleCount"], 1);
    assert_eq!(windowed["window"], json!({ "fromSec": 2, "toSec": 2 }));
    assert_eq!(windowed["segmentMode"], "fixed");

    let hr = parsed(
        &call(
            &h,
            "get_session_heart_rate",
            json!({ "session_id": "1001", "date": "2026-09-01", "sample_every_seconds": 1 }),
        )
        .await,
    );
    assert_eq!(hr["sampleCount"], 2);
    assert_eq!(hr["hrSamples"][1]["hr"], 101);

    let requests = server.received_requests().await.unwrap();
    let hits = |suffix: &str| {
        requests
            .iter()
            .filter(|r| r.url.path().ends_with(suffix))
            .count()
    };
    assert_eq!(
        hits("GetPerformedWorkoutSessionByIdCr"),
        1,
        "include_raw must not fetch the session twice"
    );
    assert_eq!(hits("an-run/Details"), 2, "one per analytics call");
}

#[tokio::test]
async fn bad_arguments_are_error_results_not_protocol_errors() {
    let server = mywellness().await;
    let h = harness(&server, Some(saved_session("tok-1")), None).await;
    for (tool, args, message) in [
        ("get_workout_session", json!({}), "session_id is required"),
        (
            "get_workout_session",
            json!({ "session_id": "1001", "date": "yesterday" }),
            "invalid date",
        ),
        (
            "get_exercise_analytics",
            json!({ "analytics_id": "x", "segment_by": "sometimes" }),
            "segment_by must be",
        ),
        (
            "get_session_heart_rate",
            json!({ "session_id": "abc", "date": "2026-09-01" }),
            "must be an integer",
        ),
    ] {
        let result = call(&h, tool, args).await;
        assert_eq!(result.is_error, Some(true), "{tool}");
        assert!(text(&result).contains(message), "{tool}: {}", text(&result));
    }
}

#[tokio::test]
async fn without_a_login_every_call_says_how_to_sign_in_and_a_later_login_is_picked_up() {
    let server = mywellness().await;
    let h = harness(&server, None, None).await;
    let result = call(&h, "get_account_info", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        text(&result).contains("no saved mywellness login"),
        "{}",
        text(&result)
    );
    assert!(text(&result).contains(LOGIN_HINT));

    // `technogym-mcp login` in another terminal: the next call sees it.
    FileSessionStore::new(h.dir.path())
        .save(&saved_session("tok-1"))
        .await
        .unwrap();
    let result = call(&h, "get_account_info", json!({})).await;
    assert_ne!(result.is_error, Some(true), "{}", text(&result));
}

#[tokio::test]
async fn a_rejected_session_resets_the_source_so_the_next_call_reloads() {
    let server = mywellness().await;
    let h = harness(&server, Some(saved_session("stale")), None).await;
    let result = call(&h, "get_account_info", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(text(&result).contains("rejected"), "{}", text(&result));
    FileSessionStore::new(h.dir.path())
        .save(&saved_session("tok-1"))
        .await
        .unwrap();
    let result = call(&h, "get_account_info", json!({})).await;
    assert_ne!(
        result.is_error,
        Some(true),
        "the reset dropped the stale client: {}",
        text(&result)
    );
    drop(h.source);
}

#[tokio::test]
async fn configured_credentials_sign_in_once_and_a_wrong_password_is_tried_once_per_process() {
    let server = mywellness().await;
    let h = harness(&server, None, Some(Credentials::new("a@b.c", "pw"))).await;
    assert_eq!(
        parsed(&call(&h, "get_account_info", json!({})).await)["movergy"],
        77.0
    );
    assert_eq!(
        parsed(&call(&h, "get_account_info", json!({})).await)["movergy"],
        77.0
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == LOGIN)
            .count(),
        1
    );
    assert!(
        FileSessionStore::new(h.dir.path())
            .load()
            .await
            .unwrap()
            .is_some(),
        "the session was saved"
    );

    let refusing = MockServer::start().await;
    Mock::given(path(LOGIN))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "errors": [{ "errorMessage": "Wrong password" }] })),
        )
        .mount(&refusing)
        .await;
    let h = harness(&refusing, None, Some(Credentials::new("a@b.c", "wrong"))).await;
    for _ in 0..3 {
        let result = call(&h, "get_account_info", json!({})).await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Wrong password"),
            "{}",
            text(&result)
        );
    }
    assert_eq!(
        refusing.received_requests().await.unwrap().len(),
        1,
        "a wrong password is never retried"
    );
}
