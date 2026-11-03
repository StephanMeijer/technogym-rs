//! A stateless OAuth 2.1 authorization server in front of `/mcp`. Nothing the client receives
//! is stored: client ids, sign-in form state, codes and tokens are sealed (see [`seal`](super::seal)).
//!
//! mywellness has no refresh token. Its bearer token lives one hour and is renewed by using it, so
//! each OAuth refresh makes one cheap mywellness call and seals the renewed token. Access tokens
//! last 20 minutes, so a client in use refreshes well inside mywellness's hour. After an hour with
//! no refresh the mywellness token is gone: the refresh fails and the user reconnects.
//!
//! shortcut: codes and refresh tokens are not single-use (there is no grant store), and nothing
//! revokes one connection. Rotate `SEAL_KEYS` to revoke everyone. Add a store if this is ever
//! hosted for strangers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Redirect, Response};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use technogym::{Error, Session};
use url::Url;

use super::app::{ClientIp, Hosted};
use super::pages::{
    CredentialsPage, credentials_page, error_page, is_trusted_redirect, page_headers,
    redirect_target,
};
use super::seal::{Purpose, now};

/// Well inside the one hour a mywellness token lives, so a refresh renews it before it expires.
pub const ACCESS_TTL_S: u64 = 20 * 60;
/// How long an authorization code may be exchanged.
pub const CODE_TTL_S: u64 = 60;
/// How long the sign-in page stays valid.
pub const FORM_TTL_S: u64 = 600;

/// Where the sign-in form posts.
pub const SIGN_IN_PATH: &str = "/mcp/oauth/signin";

const EXPIRED: &str = "This sign-in page has expired. Start again from your MCP client (remove and re-add the connector if needed).";

#[derive(Serialize, Deserialize)]
struct ClientRecord {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: String,
    client_secret: Option<String>,
    client_id_issued_at: u64,
}

#[derive(Serialize, Deserialize)]
struct AuthRequest {
    client_id: String,
    client_name: Option<String>,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CodePayload {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    session: Session,
}

/// What an access or refresh token carries.
#[derive(Serialize, Deserialize)]
pub struct TokenPayload {
    /// The client it was issued to.
    pub client_id: String,
    /// The mywellness session.
    pub session: Session,
}

fn oauth_error(status: StatusCode, error: &str, description: impl Into<String>) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "error": error, "error_description": description.into() })),
    )
        .into_response()
}

fn page(status: StatusCode, redirect_uri: Option<&str>, body: String) -> Response {
    let mut res = (status, body).into_response();
    for (k, v) in page_headers(redirect_uri) {
        res.headers_mut()
            .insert(k, v.parse().expect("header value"));
    }
    res
}

/// `GET /.well-known/oauth-authorization-server`
pub async fn authorization_server_metadata(State(h): State<Arc<Hosted>>) -> Json<Value> {
    Json(json!({
        "issuer": h.origin(),
        "service_documentation": h.docs_url(),
        "authorization_endpoint": h.url("/mcp/oauth/authorize"),
        "token_endpoint": h.url("/mcp/oauth/token"),
        "registration_endpoint": h.url("/mcp/oauth/register"),
        "revocation_endpoint": h.url("/mcp/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
    }))
}

/// `GET /.well-known/oauth-protected-resource[/mcp]`
pub async fn protected_resource_metadata(State(h): State<Arc<Hosted>>) -> Json<Value> {
    Json(json!({
        "resource": h.url("/mcp"),
        "authorization_servers": [h.origin()],
        "bearer_methods_supported": ["header"],
        "resource_name": "Technogym mywellness (unofficial)",
        "resource_documentation": h.docs_url(),
    }))
}

/// `POST /mcp/oauth/register`: dynamic client registration. The whole registration, secret
/// included, is sealed into the client id; a client id is never re-issued and never expires.
pub async fn register(
    State(h): State<Arc<Hosted>>,
    ClientIp(ip): ClientIp,
    body: axum::body::Bytes,
) -> Response {
    if !h.limits.register.allow(&ip) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "Too many registrations; try again later",
        );
    }
    let Ok(Value::Object(mut client)) = serde_json::from_slice::<Value>(&body) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "The body must be a JSON object",
        );
    };
    let redirect_uris: Vec<String> = client
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if redirect_uris.is_empty() || redirect_uris.iter().any(|u| Url::parse(u).is_err()) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris must be a non-empty list of absolute URIs",
        );
    }
    let method = client
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        .unwrap_or("client_secret_basic")
        .to_string();
    if !["none", "client_secret_basic", "client_secret_post"].contains(&method.as_str()) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "unsupported token_endpoint_auth_method",
        );
    }
    let client_secret = (method != "none").then(|| {
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    });
    let record = ClientRecord {
        redirect_uris,
        client_name: client
            .get("client_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        token_endpoint_auth_method: method.clone(),
        client_secret: client_secret.clone(),
        client_id_issued_at: now(),
    };
    client.insert(
        "client_id".into(),
        json!(h.sealer.seal(Purpose::Client, &record, None)),
    );
    client.insert(
        "client_id_issued_at".into(),
        json!(record.client_id_issued_at),
    );
    client.insert("token_endpoint_auth_method".into(), json!(method));
    if let Some(secret) = client_secret {
        client.insert("client_secret".into(), json!(secret));
        client.insert("client_secret_expires_at".into(), json!(0));
    }
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(Value::Object(client)),
    )
        .into_response()
}

fn client_of(h: &Hosted, client_id: &str) -> Option<ClientRecord> {
    h.sealer
        .unseal::<ClientRecord>(Purpose::Client, client_id)
        .map(|u| u.data)
}

/// `GET /mcp/oauth/authorize`: validates the request and shows the sign-in page.
pub async fn authorize(
    State(h): State<Arc<Hosted>>,
    ClientIp(ip): ClientIp,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !h.limits.authorize.allow(&ip) {
        return page(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            error_page("Too many requests from your network. Wait a minute and try again."),
        );
    }
    let get = |k: &str| q.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let Some(client_id) = get("client_id") else {
        return page(
            StatusCode::BAD_REQUEST,
            None,
            error_page("client_id is missing."),
        );
    };
    let Some(client) = client_of(&h, client_id) else {
        return page(
            StatusCode::BAD_REQUEST,
            None,
            error_page("Unknown client. Remove and re-add the connector in your MCP client."),
        );
    };
    // The redirect target is checked before anything is sent to it.
    let redirect_uri = match (get("redirect_uri"), client.redirect_uris.as_slice()) {
        (Some(r), list) if list.iter().any(|u| u == r) => r.to_string(),
        (None, [only]) => only.clone(),
        _ => {
            return page(
                StatusCode::BAD_REQUEST,
                None,
                error_page("The redirect_uri is not registered for this client."),
            );
        }
    };
    let state = get("state").map(str::to_string);
    let fail = |description: &str| {
        let mut location = Url::parse(&redirect_uri).expect("validated at registration");
        location
            .query_pairs_mut()
            .append_pair("error", "invalid_request")
            .append_pair("error_description", description);
        if let Some(s) = &state {
            location.query_pairs_mut().append_pair("state", s);
        }
        Redirect::to(location.as_str()).into_response()
    };
    if get("response_type") != Some("code") {
        return fail("response_type must be code");
    }
    if get("code_challenge_method").is_some_and(|m| m != "S256") {
        return fail("code_challenge_method must be S256");
    }
    let Some(code_challenge) = get("code_challenge") else {
        return fail("code_challenge is required");
    };
    let request = AuthRequest {
        client_id: client_id.to_string(),
        client_name: client.client_name.clone(),
        redirect_uri: redirect_uri.clone(),
        code_challenge: code_challenge.to_string(),
        state,
    };
    let sealed = h.sealer.seal(Purpose::AuthReq, &request, Some(FORM_TTL_S));
    page(
        StatusCode::OK,
        Some(&redirect_uri),
        credentials_page(&CredentialsPage {
            action: SIGN_IN_PATH,
            request: &sealed,
            client_name: client.client_name.as_deref(),
            redirect_uri: &redirect_uri,
            email: "",
            error: None,
        }),
    )
}

/// Milliseconds since `started`, for logs.
pub fn millis(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn sign_in_error(error: &Error) -> String {
    match error {
        Error::RateLimit { .. } => {
            "mywellness is limiting sign-ins right now. Wait a few minutes and try again.".into()
        }
        Error::Auth(_) => "mywellness didn't accept that email and password.".into(),
        Error::Connection { .. } => {
            "mywellness could not be reached. Try again in a minute.".into()
        }
        other => format!("Sign-in failed: {other}"),
    }
}

/// `POST /mcp/oauth/signin`: the sign-in form. Five failed tries per IP per 15 min and 100
/// sign-ins across all IPs also protect the server IP's standing with mywellness, which sees
/// every user's login come from here.
pub async fn sign_in(
    State(h): State<Arc<Hosted>>,
    ClientIp(ip): ClientIp,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let sealed_request = form.get("request").cloned().unwrap_or_default();
    let Some(request) = h
        .sealer
        .unseal::<AuthRequest>(Purpose::AuthReq, &sealed_request)
        .map(|u| u.data)
    else {
        return page(StatusCode::BAD_REQUEST, None, error_page(EXPIRED));
    };
    let email = form
        .get("email")
        .map(|e| e.trim().to_string())
        .unwrap_or_default();
    let password = form.get("password").cloned().unwrap_or_default();
    let retry = |status: StatusCode, error: &str| {
        page(
            status,
            Some(&request.redirect_uri),
            credentials_page(&CredentialsPage {
                action: SIGN_IN_PATH,
                request: &sealed_request,
                client_name: request.client_name.as_deref(),
                redirect_uri: &request.redirect_uri,
                email: &email,
                error: Some(error),
            }),
        )
    };
    if h.limits.sign_in_ip.exhausted(&ip) {
        return retry(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts from your network. Wait 15 minutes and try again.",
        );
    }
    if !h.limits.sign_in_global.allow("global") {
        return retry(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many people are signing in right now. Wait 15 minutes and try again.",
        );
    }
    if email.is_empty() || password.is_empty() {
        return retry(
            StatusCode::BAD_REQUEST,
            "Enter your mywellness email and password.",
        );
    }
    // Enforced here, before mywellness is called.
    if !is_trusted_redirect(&request.redirect_uri)
        && form.get("confirmRedirect").map(String::as_str) != Some("1")
    {
        return retry(
            StatusCode::BAD_REQUEST,
            &format!(
                "Tick the box to confirm you trust {} before signing in.",
                redirect_target(&request.redirect_uri)
            ),
        );
    }
    let started = std::time::Instant::now();
    // A fresh client per sign-in: the in-memory credentials `login()` keeps die with it.
    let session = match h.client().build().login(&email, &password).await {
        Ok(session) => session,
        Err(e) => {
            h.limits.sign_in_ip.hit(&ip);
            tracing::info!(ok = false, ms = millis(started), "sign-in");
            return retry(StatusCode::BAD_REQUEST, &sign_in_error(&e));
        }
    };
    tracing::info!(ok = true, ms = millis(started), "sign-in");
    let code = CodePayload {
        client_id: request.client_id,
        redirect_uri: request.redirect_uri.clone(),
        code_challenge: request.code_challenge,
        session,
    };
    let mut location = Url::parse(&request.redirect_uri).expect("validated at registration");
    location.query_pairs_mut().append_pair(
        "code",
        &h.sealer.seal(Purpose::Code, &code, Some(CODE_TTL_S)),
    );
    if let Some(s) = &request.state {
        location.query_pairs_mut().append_pair("state", s);
    }
    Redirect::to(location.as_str()).into_response()
}

/// The client named by the form or the Basic header, with its secret checked.
fn authenticate_client(
    h: &Hosted,
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Result<(String, ClientRecord), Response> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| STANDARD.decode(b.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| {
            pair.split_once(':')
                .map(|(id, secret)| (id.to_string(), secret.to_string()))
        });
    let (client_id, secret) = match basic {
        Some((id, secret)) => (id, Some(secret)),
        None => (
            form.get("client_id").cloned().unwrap_or_default(),
            form.get("client_secret").cloned(),
        ),
    };
    let client = client_of(h, &client_id)
        .ok_or_else(|| oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "Unknown client"))?;
    // shortcut: plain comparison of a random 32-byte secret; constant-time matters for guessable secrets only.
    if client.client_secret.is_some() && client.client_secret != secret {
        return Err(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Invalid client secret",
        ));
    }
    Ok((client_id, client))
}

fn issue(h: &Hosted, client_id: &str, session: Session) -> Response {
    let payload = TokenPayload {
        client_id: client_id.to_string(),
        session,
    };
    (
        StatusCode::OK,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(json!({
            "access_token": h.sealer.seal(Purpose::Access, &payload, Some(ACCESS_TTL_S)),
            "token_type": "bearer",
            "expires_in": ACCESS_TTL_S,
            "refresh_token": h.sealer.seal(Purpose::Refresh, &payload, None),
        })),
    )
        .into_response()
}

/// `POST /mcp/oauth/token`: exchanges a code (with PKCE) or a refresh token for tokens.
pub async fn token(
    State(h): State<Arc<Hosted>>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    if !h.limits.token.allow(&ip) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "Too many token requests; try again later",
        );
    }
    let (client_id, _client) = match authenticate_client(&h, &headers, &form) {
        Ok(c) => c,
        Err(res) => return res,
    };
    let get = |k: &str| form.get(k).map(String::as_str).filter(|v| !v.is_empty());
    match get("grant_type") {
        Some("authorization_code") => {
            let Some(payload) = get("code")
                .and_then(|c| h.sealer.unseal::<CodePayload>(Purpose::Code, c))
                .map(|u| u.data)
            else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "The authorization code is invalid or has expired",
                );
            };
            if payload.client_id != client_id {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "The authorization code was issued to another client",
                );
            }
            if get("redirect_uri").is_some_and(|r| r != payload.redirect_uri) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "redirect_uri does not match the authorization request",
                );
            }
            let Some(verifier) = get("code_verifier") else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code_verifier is required",
                );
            };
            if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != payload.code_challenge
            {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code_verifier does not match the code_challenge",
                );
            }
            issue(&h, &client_id, payload.session)
        }
        Some("refresh_token") => {
            let Some(payload) = get("refresh_token")
                .and_then(|t| h.sealer.unseal::<TokenPayload>(Purpose::Refresh, t))
                .map(|u| u.data)
            else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "The refresh token is invalid",
                );
            };
            if payload.client_id != client_id {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "The refresh token was issued to another client",
                );
            }
            // One cheap authenticated call renews the mywellness token while it is still valid.
            let api = h.client().build();
            api.set_session(payload.session.clone());
            match api.get_movergy().await {
                Ok(_) => issue(&h, &client_id, api.session().unwrap_or(payload.session)),
                Err(Error::Auth(_)) => oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "Your mywellness session ended after an hour without use; sign in again",
                ),
                Err(e) => {
                    tracing::warn!("refresh failed: {e}");
                    oauth_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "server_error",
                        "mywellness could not renew the session right now; try again shortly",
                    )
                }
            }
        }
        _ => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "grant_type must be authorization_code or refresh_token",
        ),
    }
}

/// `POST /mcp/oauth/revoke`: accepted and ignored. Tokens are stateless; rotating `SEAL_KEYS`
/// revokes everyone.
pub async fn revoke() -> StatusCode {
    StatusCode::OK
}

/// The access token's payload from an `Authorization: Bearer` header.
pub fn bearer_payload(h: &Hosted, headers: &HeaderMap) -> Option<TokenPayload> {
    let token = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?
        .trim();
    h.sealer
        .unseal::<TokenPayload>(Purpose::Access, token)
        .map(|u| u.data)
}
