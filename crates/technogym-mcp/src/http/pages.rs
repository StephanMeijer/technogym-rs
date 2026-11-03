//! The hosted sign-in pages.

use url::Url;

const REPO_URL: &str = "https://github.com/StephanMeijer/technogym-rs";

/// HTML-escapes text.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Redirect targets the sign-in page trusts without asking: the known MCP clients. Anyone can
/// register a client under any name, so for any other target the user must confirm where the
/// authorization code goes.
pub fn is_trusted_redirect(redirect_uri: &str) -> bool {
    let Ok(url) = Url::parse(redirect_uri) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    match url.scheme() {
        "https" if ["claude.ai", "claude.com"].contains(&host) => true,
        "http" | "https" => ["127.0.0.1", "localhost", "[::1]", "::1"].contains(&host),
        scheme => ["cursor", "vscode", "vscode-insiders"].contains(&scheme),
    }
}

/// What the user is told the code goes to: the host for a web URL, the scheme for an app link.
pub fn redirect_target(redirect_uri: &str) -> String {
    match Url::parse(redirect_uri) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url
            .host_str()
            .map(|h| {
                url.port()
                    .map_or_else(|| h.to_string(), |p| format!("{h}:{p}"))
            })
            .unwrap_or_default(),
        Ok(url) => format!("{}:", url.scheme()),
        Err(_) => redirect_uri.to_string(),
    }
}

/// A CSP `form-action` source for the client's redirect: its origin, or its scheme for app links.
fn form_action_source(redirect_uri: &str) -> Option<String> {
    let url = Url::parse(redirect_uri).ok()?;
    Some(if matches!(url.scheme(), "http" | "https") {
        url.origin().ascii_serialization()
    } else {
        format!("{}:", url.scheme())
    })
}

/// Browsers apply `form-action` to the redirect after a submit, so the client's origin must be in it.
pub fn page_headers(redirect_uri: Option<&str>) -> Vec<(&'static str, String)> {
    let mut form_action = "'self'".to_string();
    if let Some(source) = redirect_uri.and_then(form_action_source) {
        form_action.push(' ');
        form_action.push_str(&source);
    }
    vec![
        ("content-type", "text/html; charset=utf-8".into()),
        (
            "content-security-policy",
            format!("default-src 'none'; style-src 'unsafe-inline'; form-action {form_action}"),
        ),
        ("cache-control", "no-store".into()),
        ("x-frame-options", "DENY".into()),
        ("referrer-policy", "no-referrer".into()),
    ]
}

const STYLE: &str = r#"<style>
:root{color-scheme:light dark;--bg:#f6f8fa;--card:#fff;--line:#d0d7de;--text:#1f2328;--muted:#59636e;--input:#fff;--err-bg:#ffebe9;--err:#82071e;--accent:#d4202c;--btn-text:#fff}
@media (prefers-color-scheme: dark){:root{--bg:#0d1117;--card:#161b22;--line:#30363d;--text:#e6edf3;--muted:#9198a1;--input:#0d1117;--err-bg:#3d1214;--err:#ffb3ad;--accent:#ff6b6b;--btn-text:#0d1117}}
*{box-sizing:border-box}
body{font-family:system-ui,-apple-system,"Segoe UI",sans-serif;background:var(--bg);color:var(--text);margin:0;min-height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;padding:16px}
main{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:24px;width:100%;max-width:400px}
.brand{font-weight:700;font-size:1.05rem;letter-spacing:.02em;margin:0 0 18px;color:var(--accent)}
h1{font-size:1.25rem;margin:0 0 6px}p{color:var(--muted);margin:0 0 16px;line-height:1.45}
label{display:block;font-size:.9rem;margin:14px 0 4px}
input[type=text],input[type=password]{width:100%;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:var(--input);color:var(--text);font-size:16px}
input:focus-visible,button:focus-visible,a:focus-visible{outline:3px solid var(--accent);outline-offset:2px}
button{margin-top:20px;width:100%;padding:12px;border:0;border-radius:8px;background:var(--accent);color:var(--btn-text);font-size:1rem;font-weight:600;cursor:pointer}
.error{background:var(--err-bg);color:var(--err);border-radius:8px;padding:10px;margin-bottom:8px}
.check{display:flex;gap:8px;align-items:center;margin-top:14px}
footer{margin-top:16px;font-size:.8rem;color:var(--muted);text-align:center;max-width:400px}footer a{color:inherit}
</style>"#;

/// The page frame.
pub fn shell(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{}</title>{STYLE}</head><body><main>
<div class="brand">Technogym · mywellness</div>
{body}</main><footer>Open source · <a href="{REPO_URL}" target="_blank" rel="noopener noreferrer">technogym-rs on GitHub</a> · Not affiliated with Technogym</footer></body></html>"#,
        escape(title)
    )
}

/// What the sign-in form needs.
pub struct CredentialsPage<'a> {
    /// Where the form posts.
    pub action: &'a str,
    /// The sealed authorization request.
    pub request: &'a str,
    /// The client's self-declared name.
    pub client_name: Option<&'a str>,
    /// Where the code goes.
    pub redirect_uri: &'a str,
    /// The email typed last time.
    pub email: &'a str,
    /// Why the last try failed.
    pub error: Option<&'a str>,
}

/// The sign-in form.
pub fn credentials_page(o: &CredentialsPage<'_>) -> String {
    let target = escape(&redirect_target(o.redirect_uri));
    let client = o.client_name.map_or_else(
        || "your MCP client".to_string(),
        |n| format!("an app that calls itself <strong>{}</strong>", escape(n)),
    );
    let trust = if is_trusted_redirect(o.redirect_uri) {
        String::new()
    } else {
        format!(
            r#"<div class="error">{target} is not a known MCP client. Only continue if you started this from an app you trust.</div>
<label class="check"><input type="checkbox" name="confirmRedirect" value="1" required>I trust {target} with access to my mywellness data</label>"#
        )
    };
    let error = o
        .error
        .map(|e| format!(r#"<div class="error" role="alert">{}</div>"#, escape(e)))
        .unwrap_or_default();
    shell(
        "Sign in to mywellness",
        &format!(
            r#"<h1>Sign in to mywellness</h1>
<p>Sign in with your Technogym mywellness account to use your gym training data in {client}. Your password goes only to mywellness and is never stored.</p>
<p>After you sign in, you'll be sent to <strong>{target}</strong>.</p>
{error}<form method="post" action="{}"><input type="hidden" name="request" value="{}">
<label for="email">Email</label><input id="email" name="email" type="text" autocomplete="username" autocapitalize="none" spellcheck="false" required value="{}">
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" required>
{trust}
<button type="submit">Sign in</button></form>"#,
            escape(o.action),
            escape(o.request),
            escape(o.email)
        ),
    )
}

/// A page with no form, e.g. an expired sign-in. Plain text in, escaped here.
pub fn error_page(message: &str) -> String {
    shell(
        "mywellness sign-in",
        &format!("<h1>mywellness sign-in</h1><p>{}</p>", escape(message)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_redirects() {
        assert!(is_trusted_redirect(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(is_trusted_redirect("http://localhost:3334/callback"));
        assert!(is_trusted_redirect("http://127.0.0.1:1234/cb"));
        assert!(is_trusted_redirect(
            "cursor://anysphere.cursor-mcp/oauth/callback"
        ));
        assert!(!is_trusted_redirect("http://claude.ai/x"));
        assert!(!is_trusted_redirect("https://evil.example/cb"));
        assert!(!is_trusted_redirect("not a url"));
        assert_eq!(
            redirect_target("https://evil.example:8443/cb"),
            "evil.example:8443"
        );
        assert_eq!(redirect_target("cursor://x/y"), "cursor:");
    }

    #[test]
    fn pages_escape_and_set_csp() {
        let page = credentials_page(&CredentialsPage {
            action: "/mcp/oauth/signin",
            request: "r",
            client_name: Some("<b>Evil</b>"),
            redirect_uri: "https://evil.example/cb",
            email: "a@b.c",
            error: Some("Nope <"),
        });
        assert!(page.contains("&lt;b&gt;Evil&lt;/b&gt;"));
        assert!(page.contains("confirmRedirect"));
        assert!(page.contains("Nope &lt;"));
        let headers = page_headers(Some("https://evil.example/cb"));
        assert!(headers.iter().any(|(k, v)| *k == "content-security-policy"
            && v.ends_with("form-action 'self' https://evil.example")));
        assert!(
            !credentials_page(&CredentialsPage {
                action: "/a",
                request: "r",
                client_name: None,
                redirect_uri: "https://claude.ai/cb",
                email: "",
                error: None,
            })
            .contains("confirmRedirect")
        );
    }
}
