//! The HTTP transport: JSON POSTs with timeout, retries and the error mapping.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Response, StatusCode};
use serde_json::Value;

use crate::error::{Error, Result};

const RETRY_STATUSES: &[u16] = &[408, 500, 502, 503, 504];
/// The longest a `Retry-After` is honoured; anything above becomes an [`Error::RateLimit`].
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// The longest the doubling backoff grows to, whatever `retries` is.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Timeout, retries and backoff for every request.
#[derive(Clone, Debug)]
pub struct TransportOptions {
    /// Per-request timeout. Default 30 s.
    pub timeout: Duration,
    /// How many times a transient failure is retried. Default 3.
    pub retries: u32,
    /// Delay before the first retry, doubled on each retry after it and capped at 30 s. Default 500 ms.
    pub backoff: Duration,
}

impl Default for TransportOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retries: 3,
            backoff: Duration::from_millis(500),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Fetcher {
    http: reqwest::Client,
    options: TransportOptions,
}

impl Fetcher {
    pub(crate) fn new(http: reqwest::Client, options: TransportOptions) -> Self {
        Self { http, options }
    }

    /// POSTs `body` as JSON. With `retry`, a network error or a retryable status is tried again
    /// up to `retries` times, and a 429 after its `Retry-After` (when it is at most
    /// [`MAX_RETRY_AFTER`]); the login POST passes `false`.
    pub(crate) async fn post_json(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &Value,
        retry: bool,
    ) -> Result<Response> {
        let safe_url = redact(url);
        let mut attempt = 0;
        // What to wait before the next attempt: the doubling backoff, or the server's own ask.
        let mut pause = None;
        loop {
            if let Some(wait) = pause.take() {
                tokio::time::sleep(wait).await;
            }
            let can_retry = retry && attempt < self.options.retries;
            let sent = self
                .http
                .post(url)
                .headers(headers.clone())
                .timeout(self.options.timeout)
                .json(body)
                .send()
                .await;
            let res = match sent {
                Ok(res) => res,
                Err(source) if can_retry => {
                    tracing::debug!(url = %safe_url, attempt, "retrying after transport error: {source}");
                    pause = Some(backoff_delay(self.options.backoff, attempt));
                    attempt += 1;
                    continue;
                }
                Err(source) => {
                    return Err(Error::Connection {
                        url: safe_url,
                        attempts: attempt + 1,
                        source,
                    });
                }
            };
            let status = res.status();
            if status.is_success() {
                return Ok(res);
            }
            if can_retry && RETRY_STATUSES.contains(&status.as_u16()) {
                tracing::debug!(url = %safe_url, attempt, status = status.as_u16(), "retrying");
                pause = Some(backoff_delay(self.options.backoff, attempt));
                attempt += 1;
                continue;
            }
            if can_retry
                && status == StatusCode::TOO_MANY_REQUESTS
                && let Some(wait) = retry_after(&res).map(Duration::from_secs)
                && wait <= MAX_RETRY_AFTER
            {
                tracing::debug!(url = %safe_url, attempt, wait_s = wait.as_secs(), "rate limited, waiting");
                pause = Some(wait);
                attempt += 1;
                continue;
            }
            return Err(to_error(res, status, safe_url).await);
        }
    }
}

/// `base` doubled `attempt` times, never above [`MAX_BACKOFF`] and never overflowing.
fn backoff_delay(base: Duration, attempt: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(attempt))
        .min(MAX_BACKOFF)
}

/// `Retry-After` in seconds, when the response carries one as a number.
fn retry_after(res: &Response) -> Option<u64> {
    res.headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok()?.trim().parse().ok())
}

async fn to_error(res: Response, status: StatusCode, url: String) -> Error {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Error::RateLimit {
            url,
            retry_after: retry_after(&res),
        };
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Error::Auth(format!("not authorized for {url} ({status})"));
    }
    let body = res.text().await.unwrap_or_default();
    Error::Http {
        status: status.as_u16(),
        url,
        body,
    }
}

/// Drops the query string, so an error message can never carry one.
fn redact(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_and_never_overflows() {
        let half = Duration::from_millis(500);
        assert_eq!(backoff_delay(half, 0), half);
        assert_eq!(backoff_delay(half, 1), Duration::from_secs(1));
        assert_eq!(backoff_delay(half, 5), Duration::from_secs(16));
        assert_eq!(backoff_delay(half, 6), MAX_BACKOFF);
        assert_eq!(backoff_delay(half, 40), MAX_BACKOFF);
        assert_eq!(backoff_delay(Duration::MAX, 40), MAX_BACKOFF);
        assert_eq!(backoff_delay(Duration::ZERO, 40), Duration::ZERO);
    }
}
