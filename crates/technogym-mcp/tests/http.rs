#![expect(
    clippy::too_many_lines,
    reason = "one flow test walks register, authorize, sign in, token and /mcp"
)]
//! The hosted mode end to end: register → authorize → sign in → token → /mcp, against a mocked
//! mywellness.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use technogym_mcp::http::app::{Hosted, HostedConfig, router};
use technogym_mcp::http::oauth::TokenPayload;
use technogym_mcp::http::seal::{Purpose, Sealer};
use tower::ServiceExt as _;
use wiremock::matchers::{header as header_is, method, path};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

const LOGIN: &str = "/v2/enduser/authentication/login";
const MOVERGY: &str = "/my-gym/Training/User/u1/MyMovergy";
const HISTORY: &str = "/my-gym/Training/User/u1/ActivityHistory";

fn login_body() -> Value {
    json!({ "data": { "token": "tok-1", "userContext": { "id": "u1", "firstName": "Ana", "lastName": "B" },
        "facilities": [{ "id": "f1", "url": "my-gym", "name": "My Gym" }] } })
}

async fn mywellness() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(LOGIN))
        .respond_with(|req: &MockRequest| {
            let body: Value = req.body_json().unwrap();
            if body["password"] == "right" {
                ResponseTemplate::new(200).set_body_json(login_body())
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "errors": [{ "errorMessage": "Wrong password" }] }))
            }
        })
        .mount(&server)
        .await;
    Mock::given(path(MOVERGY))
        .respond_with(|req: &MockRequest| {
            let bearer = req.headers.get("authorization").unwrap();
            if bearer == "Bearer dead" {
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "errors": [{ "details": "TokenExpired" }] }))
            } else if bearer == "Bearer broken" {
                // Not an auth error: mywellness itself is unwell.
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "errors": [{ "details": "ServiceUnavailable" }] }))
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": { "movergy": 42 } }))
                    .insert_header("authorization", "Bearer tok-2")
            }
        })
        .mount(&server)
        .await;
    Mock::given(path(HISTORY))
        .and(header_is("authorization", "Bearer tok-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "items": [
            { "idCr": 1001, "partitionDate": 20_260_901, "hour": 7, "minute": 5, "name": "Push day", "type": "WorkoutSession" }
        ] } })))
        .mount(&server)
        .await;
    server
}

fn app(server: &MockServer) -> Router {
    let key = STANDARD.encode([9u8; 32]);
    let config = HostedConfig::from_vars(|k| match k {
        "PUBLIC_URL" => Some("http://localhost:3000".to_string()),
        "SEAL_KEYS" => Some(key.clone()),
        "CLIENT_IP_HEADER" => Some("x-test-ip".to_string()),
        _ => None,
    })
    .unwrap();
    router(Arc::new(
        Hosted::new(config).with_base_urls(server.uri(), server.uri()),
    ))
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HashMap<String, String>, Bytes) {
    let res: Response<Body> = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, body)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("x-test-ip", "203.0.113.5")
        .body(Body::empty())
        .unwrap()
}

fn post_json(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("x-test-ip", "203.0.113.5")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post_form(uri: &str, fields: &[(&str, &str)]) -> Request<Body> {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("x-test-ip", "203.0.113.5")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn json_of(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn query_of(location: &str) -> HashMap<String, String> {
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A registered client and a sealed sign-in form, ready to post.
struct Flow {
    client_id: String,
    client_secret: String,
    request: String,
    verifier: String,
}

async fn start_flow(app: &Router, redirect_uri: &str, auth_method: &str) -> Flow {
    let (status, _, body) = send(
        app,
        post_json(
            "/mcp/oauth/register",
            &json!({ "client_name": "Test client", "redirect_uris": [redirect_uri], "token_endpoint_auth_method": auth_method }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let reg = json_of(&body);
    let client_id = reg["client_id"].as_str().unwrap().to_string();
    let client_secret = reg["client_secret"].as_str().unwrap_or("").to_string();

    let verifier = "a-very-long-code-verifier-for-pkce-0123456789".to_string();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let uri = format!(
        "/mcp/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=xyz",
        url_encode(&client_id),
        url_encode(redirect_uri)
    );
    let (status, headers, body) = send(app, get(&uri)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(headers["content-security-policy"].contains("form-action 'self' "));
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Test client"));
    let request = html
        .split("name=\"request\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    Flow {
        client_id,
        client_secret,
        request,
        verifier,
    }
}

#[tokio::test]
async fn metadata_and_health() {
    let server = mywellness().await;
    let app = app(&server);
    let (status, _, body) = send(&app, get("/mcp/healthz")).await;
    assert_eq!((status, body.as_ref()), (StatusCode::OK, b"ok".as_slice()));
    for uri in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-authorization-server/mcp",
    ] {
        let (status, _, body) = send(&app, get(uri)).await;
        assert_eq!(status, StatusCode::OK);
        let m = json_of(&body);
        assert_eq!(m["issuer"], "http://localhost:3000");
        assert_eq!(m["token_endpoint"], "http://localhost:3000/mcp/oauth/token");
        assert_eq!(m["code_challenge_methods_supported"], json!(["S256"]));
        assert!(
            m.get("revocation_endpoint").is_none(),
            "revoke is a no-op, so it must not be advertised"
        );
    }
    let (_, _, body) = send(&app, get("/.well-known/oauth-protected-resource/mcp")).await;
    assert_eq!(json_of(&body)["resource"], "http://localhost:3000/mcp");
    assert_eq!(
        json_of(&body)["authorization_servers"],
        json!(["http://localhost:3000"])
    );
}

#[tokio::test]
async fn mcp_without_a_bearer_points_at_the_resource_metadata() {
    let server = mywellness().await;
    let app = app(&server);
    let (status, headers, _) = send(
        &app,
        post_json(
            "/mcp",
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers["www-authenticate"].contains(
        r#"resource_metadata="http://localhost:3000/.well-known/oauth-protected-resource/mcp""#
    ));
    let (status, headers, _) = send(&app, get("/mcp")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "POST");
}

#[tokio::test]
async fn registration_is_validated() {
    let server = mywellness().await;
    let app = app(&server);
    for (body, error) in [
        (json!({ "redirect_uris": [] }), "invalid_redirect_uri"),
        (
            json!({ "redirect_uris": ["not a url"] }),
            "invalid_redirect_uri",
        ),
        (
            json!({ "redirect_uris": ["https://claude.ai/cb"], "token_endpoint_auth_method": "private_key_jwt" }),
            "invalid_client_metadata",
        ),
        (json!([]), "invalid_client_metadata"),
    ] {
        let (status, _, res) = send(&app, post_json("/mcp/oauth/register", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json_of(&res)["error"], error);
    }
    let (_, _, res) = send(&app, post_json("/mcp/oauth/register", &json!({ "redirect_uris": ["https://claude.ai/cb"], "token_endpoint_auth_method": "none" }))).await;
    assert!(json_of(&res).get("client_secret").is_none());
    assert_eq!(json_of(&res)["client_secret_expires_at"], Value::Null);
}

#[tokio::test]
async fn authorize_rejects_bad_clients_and_redirects_other_errors() {
    let server = mywellness().await;
    let app = app(&server);
    let (status, _, _) = send(
        &app,
        get("/mcp/oauth/authorize?client_id=bogus&response_type=code"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let flow = start_flow(&app, "https://claude.ai/api/mcp/auth_callback", "none").await;
    let (status, _, _) = send(&app, get(&format!("/mcp/oauth/authorize?client_id={}&redirect_uri=https://evil.example/cb&response_type=code", url_encode(&flow.client_id)))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unregistered redirect never redirects"
    );
    let (status, headers, _) = send(
        &app,
        get(&format!(
            "/mcp/oauth/authorize?client_id={}&response_type=code&state=s1",
            url_encode(&flow.client_id)
        )),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let q = query_of(&headers["location"]);
    assert_eq!(q["error"], "invalid_request");
    assert!(q["error_description"].contains("code_challenge"));
    assert_eq!(q["state"], "s1");
}

#[tokio::test]
async fn the_full_flow_signs_in_exchanges_the_code_and_serves_mcp() {
    let server = mywellness().await;
    let app = app(&server);
    let flow = start_flow(
        &app,
        "https://claude.ai/api/mcp/auth_callback",
        "client_secret_post",
    )
    .await;

    // A wrong password re-renders the form with the email kept.
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "wrong"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("didn&#39;t accept"), "{html}");
    assert!(html.contains("value=\"a@b.c\""));

    // An expired or forged form state is refused.
    let (status, _, _) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", "forged"),
                ("email", "a@b.c"),
                ("password", "right"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The right password redirects with a code and the state.
    let (status, headers, _) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "right"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers["location"].clone();
    assert!(location.starts_with("https://claude.ai/api/mcp/auth_callback?"));
    let q = query_of(&location);
    assert_eq!(q["state"], "xyz");
    let code = q["code"].clone();

    // PKCE and client secret are enforced at the token endpoint.
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("client_secret", &flow.client_secret),
                ("code", &code),
                ("code_verifier", "wrong-verifier"),
            ],
        ),
    )
    .await;
    assert_eq!(
        (status, json_of(&body)["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("client_secret", "nope"),
                ("code", &code),
                ("code_verifier", &flow.verifier),
            ],
        ),
    )
    .await;
    assert_eq!(
        (status, json_of(&body)["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_client"))
    );
    let (status, headers, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("client_secret", &flow.client_secret),
                ("code", &code),
                ("code_verifier", &flow.verifier),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["cache-control"], "no-store");
    let tokens = json_of(&body);
    assert_eq!(tokens["token_type"], "bearer");
    assert_eq!(tokens["expires_in"], 1200);
    let access = tokens["access_token"].as_str().unwrap().to_string();
    let refresh = tokens["refresh_token"].as_str().unwrap().to_string();
    let unsealed = Sealer::new(vec![[9u8; 32]])
        .unseal::<TokenPayload>(Purpose::Refresh, &refresh)
        .unwrap();
    assert!(unsealed.exp.is_some(), "a refresh token expires");

    // /mcp with the bearer: initialize and list tools, then call one.
    let mcp = |body: Value, token: &str| {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("x-test-ip", "203.0.113.5")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } });
    let (status, _, body) = send(&app, mcp(init, &access)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let (status, _, body) = send(
        &app,
        mcp(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &access,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let text = String::from_utf8(body.to_vec()).unwrap();
    let listed: Value = serde_json::from_str(
        text.lines()
            .find(|l| l.starts_with('{'))
            .or_else(|| text.lines().find_map(|l| l.strip_prefix("data: ")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        listed["result"]["tools"].as_array().unwrap().len(),
        7,
        "{text}"
    );

    let call = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "get_workout_sessions", "arguments": { "from_date": "2026-09-01", "to_date": "2026-09-30" } } });
    let (status, _, body) = send(&app, mcp(call, &access)).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("Push day"), "{text}");

    let (status, _, _) = send(
        &app,
        mcp(
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list" }),
            &refresh,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a refresh token is not an access token"
    );

    // Refresh renews the mywellness token and issues a new pair; a dead session is invalid_grant.
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &flow.client_id),
                ("client_secret", &flow.client_secret),
                ("refresh_token", &refresh),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(json_of(&body)["access_token"].as_str().is_some());

    let dead = technogym_mcp::http::seal::Sealer::new(vec![[9u8; 32]]).seal(
        technogym_mcp::http::seal::Purpose::Refresh,
        &json!({ "client_id": flow.client_id, "session": { "token": "dead", "user": { "userId": "u1", "facilities": [{ "url": "my-gym" }] } } }),
        None,
    );
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &flow.client_id),
                ("client_secret", &flow.client_secret),
                ("refresh_token", &dead),
            ],
        ),
    )
    .await;
    assert_eq!(
        (status, json_of(&body)["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_grant"))
    );

    // Basic client auth works too, and revoke is accepted.
    let basic = STANDARD.encode(format!("{}:{}", flow.client_id, flow.client_secret));
    let mut req = post_form(
        "/mcp/oauth/token",
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
    );
    req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Basic {basic}").parse().unwrap(),
    );
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, post_form("/mcp/oauth/revoke", &[("token", &refresh)])).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn untrusted_redirects_need_confirmation_before_mywellness_is_called() {
    let server = mywellness().await;
    let app = app(&server);
    let flow = start_flow(&app, "https://evil.example/cb", "none").await;
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "right"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("Tick the box"));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.url.path() != LOGIN),
        "no login attempted"
    );
    let (status, _, _) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "right"),
                ("confirmRedirect", "1"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn failed_sign_ins_are_limited_per_ip() {
    let server = mywellness().await;
    let app = app(&server);
    let flow = start_flow(&app, "https://claude.ai/cb", "none").await;
    for _ in 0..5 {
        let (status, _, _) = send(
            &app,
            post_form(
                "/mcp/oauth/signin",
                &[
                    ("request", &flow.request),
                    ("email", "a@b.c"),
                    ("password", "wrong"),
                ],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "right"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(String::from_utf8_lossy(&body).contains("Wait 15 minutes"));
    let logins = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == LOGIN)
        .count();
    assert_eq!(logins, 5, "the sixth try never reached mywellness");
}

/// A sealed refresh token for `client_id` whose mywellness session token is `token`.
fn refresh_token_for(client_id: &str, token: &str) -> String {
    Sealer::new(vec![[9u8; 32]]).seal(
        Purpose::Refresh,
        &json!({ "client_id": client_id, "session": { "token": token, "user": { "userId": "u1", "facilities": [{ "url": "my-gym" }] } } }),
        None,
    )
}

/// A token request that must be refused: why, the form, and the expected status and error.
type Rejection<'a> = (&'a str, Vec<(&'a str, &'a str)>, StatusCode, &'a str);

#[tokio::test]
async fn the_token_endpoint_rejects_every_bad_grant() {
    let server = mywellness().await;
    let app = app(&server);
    let flow = start_flow(&app, "https://claude.ai/api/mcp/auth_callback", "none").await;
    let other = start_flow(&app, "https://claude.ai/api/mcp/auth_callback", "none").await;
    let (status, headers, _) = send(
        &app,
        post_form(
            "/mcp/oauth/signin",
            &[
                ("request", &flow.request),
                ("email", "a@b.c"),
                ("password", "right"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let code = query_of(&headers["location"])["code"].clone();
    let refresh = refresh_token_for(&flow.client_id, "tok-1");
    let broken = refresh_token_for(&flow.client_id, "broken");

    let cases: Vec<Rejection> = vec![
        (
            "a code that never was",
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("code", "garbage"),
                ("code_verifier", &flow.verifier),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_grant",
        ),
        (
            "a code issued to another client",
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", &other.client_id),
                ("code", &code),
                ("code_verifier", &flow.verifier),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_grant",
        ),
        (
            "a redirect_uri that was not in the authorization request",
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("code", &code),
                ("redirect_uri", "https://claude.ai/other"),
                ("code_verifier", &flow.verifier),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_grant",
        ),
        (
            "no code_verifier",
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("code", &code),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "a refresh token that never was",
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", &flow.client_id),
                ("refresh_token", "garbage"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_grant",
        ),
        (
            "a refresh token issued to another client",
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", &other.client_id),
                ("refresh_token", &refresh),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_grant",
        ),
        (
            "a refresh while mywellness is unwell",
            vec![
                ("grant_type", "refresh_token"),
                ("client_id", &flow.client_id),
                ("refresh_token", &broken),
            ],
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
        ),
        (
            "a client that never registered",
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", "not-a-client"),
                ("code", &code),
                ("code_verifier", &flow.verifier),
            ],
            StatusCode::UNAUTHORIZED,
            "invalid_client",
        ),
        (
            "an unsupported grant type",
            vec![("grant_type", "password"), ("client_id", &flow.client_id)],
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
        ),
    ];
    for (case, fields, expected_status, expected_error) in cases {
        let (status, _, body) = send(&app, post_form("/mcp/oauth/token", &fields)).await;
        assert_eq!(
            (status, json_of(&body)["error"].as_str()),
            (expected_status, Some(expected_error)),
            "{case}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    // The same code and refresh token are still good for their own client.
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &flow.client_id),
                ("code", &code),
                ("code_verifier", &flow.verifier),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let (status, _, body) = send(
        &app,
        post_form(
            "/mcp/oauth/token",
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &flow.client_id),
                ("refresh_token", &refresh),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}
