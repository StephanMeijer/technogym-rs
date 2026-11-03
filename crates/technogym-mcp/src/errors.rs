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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_kind_reads_as_a_sentence() {
        let auth = describe(&Error::Auth("expired".into()), "sign in");
        assert_eq!(
            auth,
            "mywellness login missing, expired or rejected (expired): sign in."
        );
        let limited = |retry_after| {
            describe(
                &Error::RateLimit {
                    url: "u".into(),
                    retry_after,
                },
                "",
            )
        };
        assert_eq!(
            limited(Some(30)),
            "mywellness is rate limiting requests; retry after 30 s."
        );
        assert_eq!(limited(None), "mywellness is rate limiting requests.");
        let http = describe(
            &Error::Http {
                status: 502,
                url: "u".into(),
                body: String::new(),
            },
            "",
        );
        assert!(http.starts_with("mywellness returned HTTP 502: "), "{http}");
        let api = describe(
            &Error::Api {
                action: "ActivityHistory".into(),
                message: "bad range".into(),
                errors: vec![],
            },
            "",
        );
        assert_eq!(
            api,
            "mywellness refused the call: ActivityHistory: bad range"
        );
        assert_eq!(describe(&Error::NotFound("gone".into()), ""), "gone");
    }
}
