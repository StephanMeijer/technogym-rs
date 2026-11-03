//! The hosted server's HTTP surface: health, the OAuth endpoints and `/mcp`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use technogym::{Technogym, TechnogymBuilder};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::limit::RateLimit;
use super::oauth;
use super::seal::Sealer;
use crate::source::{BaseUrls, FixedSource};
use crate::tools::TechnogymMcp;

/// Settings of the hosted server, from the environment.
#[derive(Clone, Debug)]
pub struct HostedConfig {
    /// `PUBLIC_URL`: where clients reach this server, e.g. `https://technogym.example.com`.
    pub public_url: Url,
    /// `SEAL_KEYS`, parsed.
    pub keys: Vec<[u8; 32]>,
    /// `PORT`, default 3000.
    pub port: u16,
    /// `CLIENT_IP_HEADER`: lower-case name of a header carrying the real client IP
    /// (e.g. `cf-connecting-ip`). Safe only when the server is reachable solely through the
    /// proxy that sets it.
    pub client_ip_header: Option<String>,
}

impl HostedConfig {
    /// From the process environment.
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// From any lookup, for tests.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let raw = get("PUBLIC_URL")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let public_url =
            Url::parse(&raw.ok_or("PUBLIC_URL is not set (e.g. https://technogym.example.com)")?)
                .map_err(|e| format!("PUBLIC_URL is not a URL: {e}"))?;
        let local = matches!(public_url.host_str(), Some("localhost" | "127.0.0.1"));
        if public_url.scheme() != "https" && !local {
            return Err(
                "PUBLIC_URL must be https (plain http is allowed only for localhost)".into(),
            );
        }
        let keys = Sealer::parse_keys(&get("SEAL_KEYS").unwrap_or_default())?;
        let port = match get("PORT")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
        {
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| format!("PORT must be a port number, got {p}"))?,
            None => 3000,
        };
        let client_ip_header = get("CLIENT_IP_HEADER")
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty());
        if let Some(h) = &client_ip_header
            && !h.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(format!(
                "CLIENT_IP_HEADER must be a header name (letters, digits, hyphens), got {h}"
            ));
        }
        Ok(Self {
            public_url,
            keys,
            port,
            client_ip_header,
        })
    }
}

/// The per-endpoint limiters.
#[derive(Debug)]
pub struct Limits {
    /// Failed sign-ins per IP.
    pub sign_in_ip: RateLimit,
    /// Sign-ins across all IPs.
    pub sign_in_global: RateLimit,
    /// Authorize pages per IP.
    pub authorize: RateLimit,
    /// Token requests per IP. claude.ai exchanges tokens from Anthropic's servers, so this is
    /// shared by every claude.ai user: it only stops a runaway client.
    pub token: RateLimit,
    /// Registrations per IP.
    pub register: RateLimit,
    /// MCP requests per user.
    pub mcp: RateLimit,
}

impl Default for Limits {
    fn default() -> Self {
        let min = Duration::from_secs(60);
        Self {
            sign_in_ip: RateLimit::new(5, 15 * min),
            sign_in_global: RateLimit::new(100, 15 * min),
            authorize: RateLimit::new(60, min),
            token: RateLimit::new(300, min),
            register: RateLimit::new(1000, 60 * min),
            mcp: RateLimit::new(120, min),
        }
    }
}

/// Everything the handlers share.
pub struct Hosted {
    /// The settings.
    pub config: HostedConfig,
    /// Seals tokens.
    pub sealer: Sealer,
    /// The limiters.
    pub limits: Limits,
    /// Stops in-flight MCP work on shutdown.
    pub cancel: CancellationToken,
    /// One connection pool for every mywellness call this process makes.
    http: reqwest::Client,
    base_urls: Option<BaseUrls>,
}

impl Hosted {
    /// With fresh limiters and the configured keys.
    pub fn new(config: HostedConfig) -> Self {
        let sealer = Sealer::new(config.keys.clone());
        Self {
            config,
            sealer,
            limits: Limits::default(),
            cancel: CancellationToken::new(),
            http: reqwest::Client::new(),
            base_urls: None,
        }
    }

    /// Point the mywellness client at other hosts (tests, proxies).
    pub fn with_base_urls(
        mut self,
        core_url: impl Into<String>,
        services_url: impl Into<String>,
    ) -> Self {
        self.base_urls = Some(BaseUrls::new(core_url, services_url));
        self
    }

    /// Replace the limiters, for tests.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// A mywellness client builder.
    pub fn client(&self) -> TechnogymBuilder {
        let b = Technogym::builder().http_client(self.http.clone());
        match &self.base_urls {
            Some(urls) => urls.apply(b),
            None => b,
        }
    }

    /// The public origin, e.g. `https://technogym.example.com`.
    pub fn origin(&self) -> String {
        self.config.public_url.origin().ascii_serialization()
    }

    /// A public URL for `path`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    /// Where users read about this server.
    pub fn docs_url(&self) -> &'static str {
        "https://github.com/StephanMeijer/technogym-rs/blob/main/crates/technogym-mcp/README.md"
    }

    fn public_host(&self) -> String {
        let host = self.config.public_url.host_str().unwrap_or("localhost");
        self.config
            .public_url
            .port()
            .map_or_else(|| host.to_string(), |p| format!("{host}:{p}"))
    }
}

/// The caller's IP: the configured header when set, else the socket's peer, else `unknown`.
pub struct ClientIp(pub String);

impl FromRequestParts<Arc<Hosted>> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        h: &Arc<Hosted>,
    ) -> Result<Self, Self::Rejection> {
        let from_header = h
            .config
            .client_ip_header
            .as_deref()
            .and_then(|name| parts.headers.get(name))
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .and_then(|v| v.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string());
        let from_socket = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string());
        Ok(Self(
            from_header
                .or(from_socket)
                .unwrap_or_else(|| "unknown".into()),
        ))
    }
}

async fn healthz() -> &'static str {
    "ok"
}

fn unauthorized(h: &Hosted, description: &str) -> Response {
    let metadata = h.url("/.well-known/oauth-protected-resource/mcp");
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, format!(r#"Bearer resource_metadata="{metadata}", error="invalid_token", error_description="{description}""#))],
        Json(json!({ "error": "invalid_token", "error_description": description })),
    )
        .into_response()
}

/// Only the JSON-RPC method and tool name are logged: never arguments, results or tokens.
fn describe_rpc(body: &[u8]) -> (String, String) {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let msg = value.as_array().and_then(|a| a.first()).unwrap_or(&value);
    let method = msg["method"].as_str().unwrap_or("").to_string();
    let tool = if method == "tools/call" {
        msg["params"]["name"].as_str().unwrap_or("").to_string()
    } else {
        String::new()
    };
    (method, tool)
}

/// `POST /mcp`: one stateless rmcp service per request, over the session sealed in the bearer.
async fn mcp(State(h): State<Arc<Hosted>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(payload) = oauth::bearer_payload(&h, &headers) else {
        return unauthorized(&h, "The access token is missing, invalid or expired");
    };
    let user = h.sealer.pseudonym(&payload.session.user.user_id);
    if !h.limits.mcp.allow(&user) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "jsonrpc": "2.0", "error": { "code": -32000, "message": "Too many requests; slow down." }, "id": null })))
            .into_response();
    }
    let started = std::time::Instant::now();
    let (rpc, tool) = describe_rpc(&body);

    let api = Arc::new(h.client().build());
    api.set_session(payload.session);
    let handler = TechnogymMcp::new(Arc::new(FixedSource(api)));
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([h.public_host()])
        .with_cancellation_token(h.cancel.child_token());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // Built here rather than forwarded: the bearer already proved who is asking, so the inner
    // request carries our own host, not whatever the proxy sent.
    let mut inner = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::HOST, h.public_host());
    for name in [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::ACCEPT_ENCODING,
    ] {
        if let Some(v) = headers.get(&name) {
            inner = inner.header(name, v);
        }
    }
    if let Some(v) = headers.get("mcp-protocol-version") {
        inner = inner.header("mcp-protocol-version", v);
    }
    let inner = inner.body(Body::from(body)).expect("valid request");
    let res = service.handle(inner).await;
    let status = res.status();
    tracing::info!(
        user = &user[..8],
        rpc,
        tool,
        status = status.as_u16(),
        ms = oauth::millis(started),
        "mcp"
    );
    res.map(Body::new)
}

async fn not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        Json(json!({ "jsonrpc": "2.0", "error": { "code": -32000, "message": "Method not allowed." }, "id": null })),
    )
        .into_response()
}

/// The router: health, OAuth metadata and endpoints, and `/mcp`.
pub fn router(hosted: Arc<Hosted>) -> Router {
    Router::new()
        .route("/mcp/healthz", get(healthz))
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(oauth::authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::protected_resource_metadata),
        )
        .route("/mcp/oauth/register", post(oauth::register))
        .route("/mcp/oauth/authorize", get(oauth::authorize))
        .route(oauth::SIGN_IN_PATH, post(oauth::sign_in))
        .route("/mcp/oauth/token", post(oauth::token))
        .route("/mcp/oauth/revoke", post(oauth::revoke))
        .route("/mcp", post(mcp).get(not_allowed).delete(not_allowed))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .with_state(hosted)
}

/// Serves on `0.0.0.0:PORT` until SIGTERM or Ctrl-C.
pub async fn serve_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let config = HostedConfig::from_env()?;
    let port = config.port;
    let hosted = Arc::new(Hosted::new(config));
    let cancel = hosted.cancel.clone();
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, version = env!("CARGO_PKG_VERSION"), "listening");
    axum::serve(
        listener,
        router(hosted).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        cancel.cancel();
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_validated() {
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [1u8; 32]);
        let get = |k: &str| match k {
            "PUBLIC_URL" => Some("https://tg.example.com".to_string()),
            "SEAL_KEYS" => Some(key.clone()),
            "PORT" => Some("8080".to_string()),
            "CLIENT_IP_HEADER" => Some("CF-Connecting-IP".to_string()),
            _ => None,
        };
        let c = HostedConfig::from_vars(get).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.client_ip_header.as_deref(), Some("cf-connecting-ip"));
        assert!(
            HostedConfig::from_vars(|_| None)
                .unwrap_err()
                .contains("PUBLIC_URL")
        );
        let http = |k: &str| {
            if k == "PUBLIC_URL" {
                Some("http://tg.example.com".to_string())
            } else {
                get(k)
            }
        };
        assert!(HostedConfig::from_vars(http).unwrap_err().contains("https"));
        let local = |k: &str| match k {
            "PUBLIC_URL" => Some("http://localhost:3000".to_string()),
            "PORT" => None,
            k => get(k),
        };
        assert_eq!(HostedConfig::from_vars(local).unwrap().port, 3000);
        let bad_port = |k: &str| {
            if k == "PORT" {
                Some("http".to_string())
            } else {
                get(k)
            }
        };
        assert!(
            HostedConfig::from_vars(bad_port)
                .unwrap_err()
                .contains("PORT")
        );
    }

    #[test]
    fn rpc_description_never_includes_arguments() {
        let (rpc, tool) = describe_rpc(br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_recent_workouts","arguments":{"days":3}}}"#);
        assert_eq!(
            (rpc.as_str(), tool.as_str()),
            ("tools/call", "get_recent_workouts")
        );
        assert_eq!(
            describe_rpc(b"[{\"method\":\"initialize\"}]").0,
            "initialize"
        );
        assert_eq!(describe_rpc(b"nope"), (String::new(), String::new()));
    }
}
