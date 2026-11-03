//! The HTTP transport: JSON POSTs with timeout, retries and the error mapping.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Response, StatusCode};
use serde_json::Value;

use crate::error::{Error, Result};

const RETRY_STATUSES: &[u16] = &[408, 500, 502, 503, 504];

/// Timeout, retries and backoff for every request.
#[derive(Clone, Debug)]
pub struct TransportOptions {
    /// Per-request timeout. Default 30 s.
    pub timeout: Duration,
    /// How many times a transient failure is retried. Default 3.
    pub retries: u32,
    /// Delay before the first retry, doubled on each retry after it. Default 500 ms.
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
    /// up to `retries` times; the login POST passes `false`.
    pub(crate) async fn post_json(
        &self,
        url: &str,
        headers: HeaderMap,
        body: &Value,
        retry: bool,
    ) -> Result<Response> {
        let safe_url = redact(url);
        let mut attempt = 0;
        loop {
            if attempt > 0 {
                tokio::time::sleep(self.options.backoff * 2u32.pow(attempt - 1)).await;
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
                attempt += 1;
                continue;
            }
            return Err(to_error(res, status, safe_url).await);
        }
    }
}

async fn to_error(res: Response, status: StatusCode, url: String) -> Error {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = res
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok()?.trim().parse().ok());
        return Error::RateLimit { url, retry_after };
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
