//! Settings of the stdio server, from the environment.

use std::path::PathBuf;

use technogym::Credentials;

/// What the stdio server needs to find or make a session.
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// Where the session is saved: `TECHNOGYM_MCP_SESSION_DIR`, default `~/.technogym-mcp/session`.
    pub session_dir: PathBuf,
    /// Set only when BOTH `TECHNOGYM_EMAIL` and `TECHNOGYM_PASSWORD` (or the `MYWELLNESS_*` pair) are set.
    pub credentials: Option<Credentials>,
    /// `TECHNOGYM_FACILITY`: facility id or url slug; `None` means the account's first facility.
    pub facility: Option<String>,
}

/// A setting as typed; blank or an unexpanded `${VAR}` placeholder counts as unset.
fn setting(value: Option<String>) -> Option<String> {
    value
        .filter(|v| !v.trim().is_empty() && !v.contains("${"))
        .map(|v| v.trim().to_string())
}

impl McpConfig {
    /// From the process environment.
    pub fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// From any lookup, for tests.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let email = setting(get("TECHNOGYM_EMAIL")).or_else(|| setting(get("MYWELLNESS_EMAIL")));
        // The password is used as typed: spaces can be part of it.
        let password = [get("TECHNOGYM_PASSWORD"), get("MYWELLNESS_PASSWORD")]
            .into_iter()
            .flatten()
            .find(|p| setting(Some(p.clone())).is_some());
        let session_dir = setting(get("TECHNOGYM_MCP_SESSION_DIR")).map_or_else(
            || {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".technogym-mcp")
                    .join("session")
            },
            PathBuf::from,
        );
        Self {
            session_dir,
            credentials: email
                .zip(password)
                .map(|(email, password)| Credentials { email, password }),
            facility: setting(get("TECHNOGYM_FACILITY")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_need_both_halves_and_placeholders_do_not_count() {
        let get = |k: &str| match k {
            "TECHNOGYM_EMAIL" => Some("${TECHNOGYM_EMAIL}".to_string()),
            "MYWELLNESS_EMAIL" => Some(" a@b.c ".to_string()),
            "TECHNOGYM_PASSWORD" => Some(" p w ".to_string()),
            "TECHNOGYM_MCP_SESSION_DIR" => Some("/tmp/s".to_string()),
            _ => None,
        };
        let c = McpConfig::from_vars(get);
        assert_eq!(c.credentials, Some(Credentials::new("a@b.c", " p w ")));
        assert_eq!(c.session_dir, PathBuf::from("/tmp/s"));
        assert_eq!(c.facility, None);
        assert!(McpConfig::from_vars(|_| None).credentials.is_none());
        assert!(
            McpConfig::from_vars(|_| None)
                .session_dir
                .ends_with(".technogym-mcp/session")
        );
    }
}
