//! Plain-language text for a failure. Library messages never carry the token or password.

use technogym::Error;

/// Describes `error` for the user; `hint` says how to sign in again.
pub fn describe(error: &Error, hint: &str) -> String {
    match error {
        Error::Auth(m) => format!("mywellness login missing, expired or rejected ({m}): {hint}."),
        Error::RateLimit { retry_after, .. } => {
            let wait = retry_after
                .map(|s| format!("; retry after {s} s"))
                .unwrap_or_default();
            format!("mywellness is rate limiting requests{wait}.")
        }
        Error::Http { status, .. } => format!("mywellness returned HTTP {status}: {error}"),
        Error::Api { .. } => format!("mywellness refused the call: {error}"),
        other => other.to_string(),
    }
}
