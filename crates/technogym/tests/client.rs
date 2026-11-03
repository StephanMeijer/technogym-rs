mod common;

use std::sync::Arc;
use std::time::Duration;

use common::fixture;
use serde_json::json;
use technogym::{
    Credentials, Error, MemorySessionStore, Session, SessionStore, Technogym, TechnogymBuilder,
    TransportOptions,
};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LOGIN: &str = "/v2/enduser/authentication/login";
const MOVERGY: &str = "/my-gym/Training/User/u1/MyMovergy";

fn session(token: &str) -> Session {
    let mut s = Session {
        token: token.into(),
        user: technogym::parse::parse_user(&fixture("login")["data"]).unwrap(),
        saved_at: 0,
    };
    s.token = token.into();
    s
}

fn builder(server: &MockServer) -> TechnogymBuilder {
    Technogym::builder()
        .core_url(server.uri())
        .services_url(server.uri())
        .transport(TransportOptions {
            backoff: Duration::from_millis(1),
            ..TransportOptions::default()
        })
}

/// A login that hands out `tok-1`, `tok-2`, ... and refuses requests without the app id.
async fn mount_login(server: &MockServer) {
    let n = Arc::new(std::sync::atomic::AtomicU32::new(0));
    Mock::given(method("POST"))
        .and(path(LOGIN))
        .respond_with(move |req: &Request| {
            if req
                .headers
                .get("x-mwapps-appid")
                .map(|v| v.to_str().unwrap())
                != Some(technogym::constants::APP_ID)
            {
                return ResponseTemplate::new(200).set_body_json(
                    json!({ "errors": [{ "details": "ClientApplicationNotTrusted" }] }),
                );
            }
            let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let mut body = fixture("login");
            body["data"]["token"] = json!(format!("tok-{i}"));
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(server)
        .await;
}

fn ok(data: &serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
}

#[tokio::test]
async fn login_sends_app_headers_and_saves_the_session() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    let store = Arc::new(MemorySessionStore::default());
    let tg = builder(&server)
        .session_store(SharedStore(store.clone()))
        .build();
    let session = tg.login("a@b.c", "pw").await.unwrap();
    assert_eq!(session.token, "tok-1");
    assert_eq!(session.user.first_name, "Ana");
    assert!(session.saved_at > 0);
    assert_eq!(store.load().await.unwrap().unwrap().token, "tok-1");
    let req = &server.received_requests().await.unwrap()[0];
    assert_eq!(
        req.body_json::<serde_json::Value>().unwrap(),
        json!({ "username": "a@b.c", "password": "pw", "keepMeLoggedIn": true })
    );
    assert_eq!(req.headers.get("x-mwapps-client").unwrap(), "enduserweb");
    assert_eq!(
        req.headers.get("x-mwapps-clientversion").unwrap(),
        technogym::constants::CLIENT_VERSION
    );
}

#[tokio::test]
async fn login_failures_are_auth_errors() {
    let server = MockServer::start().await;
    Mock::given(path(LOGIN))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "errors": [{ "errorMessage": "Wrong password" }] })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    let err = tg.login("a@b.c", "pw").await.unwrap_err();
    assert!(
        matches!(&err, Error::Auth(m) if m.contains("Wrong password")),
        "{err}"
    );

    Mock::given(path(LOGIN))
        .respond_with(ok(&json!({ "result": "NoToken" })))
        .mount(&server)
        .await;
    let err = tg.login("a@b.c", "pw").await.unwrap_err();
    assert!(
        matches!(&err, Error::Auth(m) if m.contains("unexpected response")),
        "{err}"
    );
}

#[tokio::test]
async fn a_failed_login_does_not_keep_the_credentials_for_renewal() {
    let server = MockServer::start().await;
    Mock::given(path(LOGIN))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "errors": [{ "errorMessage": "Wrong password" }] })),
        )
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    assert!(tg.login("a@b.c", "wrong").await.is_err());
    let err = tg.get_movergy().await.unwrap_err();
    assert!(
        matches!(&err, Error::Auth(m) if m.contains("not signed in")),
        "{err}"
    );
    let logins = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == LOGIN)
        .count();
    assert_eq!(logins, 1, "the rejected password is not tried again");
}

#[tokio::test]
async fn login_is_never_retried() {
    let server = MockServer::start().await;
    Mock::given(path(LOGIN))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let err = builder(&server)
        .build()
        .login("a@b.c", "pw")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 503, .. }), "{err}");
}

#[tokio::test]
async fn services_without_session_or_credentials_is_an_auth_error() {
    let server = MockServer::start().await;
    let err = builder(&server).build().get_movergy().await.unwrap_err();
    assert!(matches!(err, Error::Auth(_)), "{err}");
}

#[tokio::test]
async fn services_posts_to_the_facility_with_the_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(MOVERGY))
        .and(header("authorization", "Bearer tok-1"))
        .and(header("x-mwapps-appid", technogym::constants::APP_ID))
        .respond_with(ok(&json!({ "movergy": 123.5 })))
        .expect(1)
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert_eq!(tg.get_movergy().await.unwrap(), Some(123.5));
}

#[tokio::test]
async fn a_given_http_client_is_the_one_requests_go_through() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .and(header("x-test", "1"))
        .respond_with(ok(&json!({ "movergy": 1.0 })))
        .expect(1)
        .mount(&server)
        .await;
    let mut defaults = reqwest::header::HeaderMap::new();
    defaults.insert("x-test", "1".parse().unwrap());
    let http = reqwest::Client::builder()
        .default_headers(defaults)
        .build()
        .unwrap();
    let tg = builder(&server).http_client(http).build();
    tg.set_session(session("tok-1"));
    assert_eq!(tg.get_movergy().await.unwrap(), Some(1.0));
}

#[tokio::test]
async fn envelope_without_data_is_returned_whole_and_api_errors_are_typed() {
    let server = MockServer::start().await;
    Mock::given(path("/my-gym/x/Whole"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "movergy": 1 })))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/x/Bad"))
        .respond_with(ok(&json!(null)).set_body_json(
            json!({ "errors": [{ "details": "SomethingElse", "errorMessage": "Nope" }] }),
        ))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/x/Text"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert_eq!(
        tg.services("/x/Whole", json!({})).await.unwrap(),
        json!({ "movergy": 1 })
    );
    let err = tg.services("/x/Bad", json!({})).await.unwrap_err();
    assert!(
        matches!(&err, Error::Api { action, message, .. } if action == "Bad" && message == "Nope"),
        "{err}"
    );
    let err = tg.services("/x/Text", json!({})).await.unwrap_err();
    assert!(matches!(err, Error::Other(_)), "{err}");
}

#[tokio::test]
async fn a_rejected_token_without_credentials_is_an_auth_error() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(
            ok(&json!(null)).set_body_json(json!({ "errors": [{ "details": "TokenExpired" }] })),
        )
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    let err = tg.get_movergy().await.unwrap_err();
    assert!(
        matches!(&err, Error::Auth(m) if m.contains("TokenExpired")),
        "{err}"
    );
}

#[tokio::test]
async fn an_api_error_that_merely_mentions_a_token_does_not_sign_in_again() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "errors": [{ "details": "InvalidTokenCount", "message": "token count must be positive" }] }),
        ))
        .mount(&server)
        .await;
    let tg = builder(&server)
        .credentials(Credentials::new("a@b.c", "pw"))
        .build();
    tg.set_session(session("tok-1"));
    let err = tg.get_movergy().await.unwrap_err();
    assert!(matches!(err, Error::Api { .. }), "{err}");
    let logins = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == LOGIN)
        .count();
    assert_eq!(logins, 0, "no renewal for a non-auth error");
}

/// Rejects `stale`, answers for anything else.
async fn mount_movergy_rejecting_stale(server: &MockServer) {
    Mock::given(path(MOVERGY))
        .respond_with(|req: &Request| {
            if req.headers.get("authorization").unwrap() == "Bearer stale" {
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "errors": [{ "message": "InvalidToken" }] }))
            } else {
                ok(&json!({ "movergy": 7 }))
            }
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_rejected_token_is_renewed_once_and_concurrent_renewals_collapse() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    mount_movergy_rejecting_stale(&server).await;
    let tg = Arc::new(
        builder(&server)
            .credentials(Credentials::new("a@b.c", "pw"))
            .build(),
    );
    tg.set_session(session("stale"));
    let (a, b) = tokio::join!(tg.get_movergy(), tg.get_movergy());
    assert_eq!(a.unwrap(), Some(7.0));
    assert_eq!(b.unwrap(), Some(7.0));
    let logins = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == LOGIN)
        .count();
    assert_eq!(logins, 1);
    assert_eq!(tg.session().unwrap().token, "tok-1");
}

#[tokio::test]
async fn signs_in_lazily_when_given_credentials() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(path(MOVERGY))
        .respond_with(ok(&json!({})))
        .mount(&server)
        .await;
    let tg = builder(&server)
        .credentials(Credentials::new("a@b.c", "pw"))
        .build();
    assert_eq!(tg.get_movergy().await.unwrap(), None);
    assert_eq!(tg.user().await.unwrap().user_id, "u1");
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
}

#[tokio::test]
async fn adopts_a_rotated_token_from_the_header_and_saves_it() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ok(&json!({ "movergy": 1 })).insert_header("authorization", "Bearer tok-9"))
        .mount(&server)
        .await;
    let store = Arc::new(MemorySessionStore::default());
    let tg = builder(&server)
        .session_store(SharedStore(store.clone()))
        .build();
    tg.set_session(session("tok-1"));
    tg.get_movergy().await.unwrap();
    assert_eq!(tg.session().unwrap().token, "tok-9");
    assert_eq!(store.load().await.unwrap().unwrap().token, "tok-9");
}

#[tokio::test]
async fn reads_are_retried_on_503() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path(MOVERGY))
        .respond_with(ok(&json!({ "movergy": 2 })))
        .expect(1)
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert_eq!(tg.get_movergy().await.unwrap(), Some(2.0));
}

#[tokio::test]
async fn rate_limits_and_http_errors_are_typed() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/x/Gone"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/x/Forbidden"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let tg = builder(&server)
        .transport(TransportOptions {
            retries: 0,
            ..TransportOptions::default()
        })
        .build();
    tg.set_session(session("tok-1"));
    let err = tg.get_movergy().await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimit {
                retry_after: Some(30),
                ..
            }
        ),
        "{err}"
    );
    let err = tg
        .services("/x/Gone?secret=1", json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Http { status: 404, url, body } if !url.contains("secret") && body == "nope"),
        "{err}"
    );
    let err = tg.services("/x/Forbidden", json!({})).await.unwrap_err();
    assert!(matches!(err, Error::Auth(_)), "{err}");
}

#[tokio::test]
async fn a_rate_limit_with_a_short_retry_after_is_waited_out() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path(MOVERGY))
        .respond_with(ok(&json!({ "movergy": 5 })))
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert_eq!(tg.get_movergy().await.unwrap(), Some(5.0));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // A wait longer than the cap, or no Retry-After at all, is reported, not slept on.
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert!(matches!(
        tg.get_movergy().await.unwrap_err(),
        Error::RateLimit {
            retry_after: Some(3600),
            ..
        }
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    assert!(matches!(
        tg.get_movergy().await.unwrap_err(),
        Error::RateLimit {
            retry_after: None,
            ..
        }
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_large_retry_budget_does_not_overflow() {
    let server = MockServer::start().await;
    Mock::given(path(MOVERGY))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let tg = builder(&server)
        .transport(TransportOptions {
            retries: 40,
            backoff: Duration::ZERO,
            ..TransportOptions::default()
        })
        .build();
    tg.set_session(session("tok-1"));
    let err = tg.get_movergy().await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 503, .. }), "{err}");
    assert_eq!(server.received_requests().await.unwrap().len(), 41);
}

#[tokio::test]
async fn facility_selection() {
    let server = MockServer::start().await;
    Mock::given(path("/Training/User/u1/MyMovergy"))
        .respond_with(ok(&json!({ "movergy": 0 })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/other/Training/User/u1/MyMovergy"))
        .respond_with(ok(&json!({ "movergy": 5 })))
        .expect(2)
        .mount(&server)
        .await;

    let mut none = session("tok-1");
    none.user.facilities.clear();
    let tg = builder(&server).build();
    tg.set_session(none);
    assert_eq!(
        tg.get_movergy().await.unwrap(),
        Some(0.0),
        "no facility: unprefixed path"
    );

    let mut two = session("tok-1");
    two.user.facilities.push(technogym::Facility {
        id: "f2".into(),
        url: "other".into(),
        name: "Other".into(),
    });
    for pick in ["f2", "other"] {
        let tg = builder(&server).facility(pick).build();
        tg.set_session(two.clone());
        assert_eq!(tg.get_movergy().await.unwrap(), Some(5.0));
    }
    let tg = builder(&server).facility("nowhere").build();
    tg.set_session(two);
    assert!(matches!(
        tg.get_movergy().await.unwrap_err(),
        Error::Other(_)
    ));
}

#[tokio::test]
async fn endpoints_send_the_documented_bodies() {
    let server = MockServer::start().await;
    Mock::given(path("/my-gym/Training/User/u1/ActivityHistory"))
        .and(body_partial_json(json!({ "startDay": "20260901", "endDay": "20260930", "justThisType": "WorkoutSession" })))
        .respond_with(ok(&fixture("activity_history")))
        .mount(&server)
        .await;
    Mock::given(path(
        "/my-gym/Training/User/u1/GetPerformedWorkoutSessionByIdCr",
    ))
    .and(body_partial_json(
        json!({ "idCr": 1001, "partitionDate": "20260901" }),
    ))
    .respond_with(ok(&fixture("workout_session")))
    .mount(&server)
    .await;
    Mock::given(path("/my-gym/Training/User/u1/GetHrSession"))
        .and(body_partial_json(
            json!({ "idCr": 1001, "partitionDate": 20_260_901 }),
        ))
        .respond_with(ok(&fixture("hr_session")))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/Training/CardioLog/an-run/Details"))
        .respond_with(ok(&fixture("exercise_analytics")))
        .mount(&server)
        .await;
    Mock::given(path("/my-gym/Training/CardioLog/missing/Details"))
        .respond_with(ok(&json!({})))
        .mount(&server)
        .await;

    let tg = builder(&server).build();
    tg.set_session(session("tok-1"));
    let from = technogym::dates::parse_iso("2026-09-30").unwrap();
    let to = technogym::dates::parse_iso("2026-09-01").unwrap();
    let sessions = tg.list_sessions(from, to, true).await.unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["1002", "1001"],
        "newest first, bounds in either order"
    );

    let ws = tg.get_workout_session("1001", Some(to)).await.unwrap();
    assert_eq!(ws.exercises.len(), 2);
    let hr = tg.get_session_heart_rate("1001", Some(to)).await.unwrap();
    assert_eq!(hr.hr_samples.len(), 2);
    let analyzed = tg
        .analyze_exercise("an-run", technogym::SegmentOptions::default())
        .await
        .unwrap();
    assert_eq!(analyzed.analytics.channels.len(), 3);
    assert_eq!(analyzed.segment_mode, technogym::SegmentMode::Laps);
    assert!(matches!(
        tg.get_exercise_analytics("missing").await.unwrap_err(),
        Error::NotFound(_)
    ));
    assert!(matches!(
        tg.get_exercise_analytics("").await.unwrap_err(),
        Error::Other(_)
    ));
    assert!(matches!(
        tg.get_workout_session("abc", Some(to)).await.unwrap_err(),
        Error::Other(_)
    ));
}

#[tokio::test]
async fn logout_clears_the_session_and_the_store() {
    let server = MockServer::start().await;
    let store = Arc::new(MemorySessionStore::default());
    store.save(&session("tok-1")).await.unwrap();
    let tg = builder(&server)
        .session_store(SharedStore(store.clone()))
        .build();
    assert!(tg.load_session().await.unwrap());
    tg.logout().await.unwrap();
    assert!(tg.session().is_none());
    assert!(store.load().await.unwrap().is_none());
    assert!(!tg.load_session().await.unwrap());
}

/// Lets a test keep a handle on the store the client owns.
struct SharedStore(Arc<MemorySessionStore>);

#[async_trait::async_trait]
impl SessionStore for SharedStore {
    async fn load(&self) -> technogym::Result<Option<Session>> {
        self.0.load().await
    }
    async fn save(&self, session: &Session) -> technogym::Result<()> {
        self.0.save(session).await
    }
    async fn clear(&self) -> technogym::Result<()> {
        self.0.clear().await
    }
}
