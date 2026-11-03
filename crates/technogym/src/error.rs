//! The one error type every fallible call in this crate returns.

use serde_json::Value;

/// Everything that can go wrong talking to mywellness.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Login failed, there is no session, or mywellness rejected the session token (HTTP
    /// 401/403 or an auth-looking error in the response envelope). Sign in again.
    #[error("{0}")]
    Auth(String),
    /// Network failure or timeout, after retries.
    #[error("request to {url} failed after {attempts} attempt(s): {source}")]
    Connection {
        /// The URL without its query string.
        url: String,
        /// How many times the request was sent.
        attempts: u32,
        /// The last transport error.
        #[source]
        source: reqwest::Error,
    },
    /// HTTP 429. `retry_after` is seconds, from the `Retry-After` header when present.
    #[error("rate limited by mywellness at {url}")]
    RateLimit {
        /// The URL without its query string.
        url: String,
        /// Seconds to wait before retrying, when the server said.
        retry_after: Option<u64>,
    },
    /// Any other non-2xx response.
    #[error("request to {url} failed with HTTP {status}")]
    Http {
        /// The status code.
        status: u16,
        /// The URL without its query string.
        url: String,
        /// The response body as text.
        body: String,
    },
    /// mywellness answered 200 with a non-empty `errors` array that is not about the session.
    #[error("{action}: {message}")]
    Api {
        /// The endpoint's last path segment, e.g. `ActivityHistory`.
        action: String,
        /// The entries' messages, joined.
        message: String,
        /// The envelope's `errors` array, untouched.
        errors: Vec<Value>,
    },
    /// The requested session, exercise or analytics record does not exist.
    #[error("{0}")]
    NotFound(String),
    /// A date that is not `YYYY-MM-DD`, or not a real calendar day.
    #[error("invalid date {0:?}; expected YYYY-MM-DD")]
    InvalidDate(String),
    /// Reading or writing the session store failed.
    #[error("session store: {0}")]
    Store(#[from] std::io::Error),
    /// Anything else: a non-JSON 200, a malformed argument.
    #[error("{0}")]
    Other(String),
}

/// `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
