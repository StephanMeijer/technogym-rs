//! Where the tools get their [`Technogym`] from: one saved session on stdio, one per request
//! when hosted.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use technogym::{Credentials, Error, FileSessionStore, Technogym, TechnogymBuilder};

/// What to tell the user when there is no usable login, on stdio.
pub const LOGIN_HINT: &str = "run `technogym-mcp login` in a terminal, or set TECHNOGYM_EMAIL and TECHNOGYM_PASSWORD in the server's environment";

/// What to tell the user when there is no usable login, when hosted.
pub const RECONNECT_HINT: &str = "reconnect the Technogym connector in your MCP client (in claude.ai: Settings → Connectors), then try again";

/// Hands the tools a signed-in client.
#[async_trait]
pub trait ApiSource: Send + Sync {
    /// A client with a session. [`Error::Auth`] when there is no usable login.
    async fn get(&self) -> technogym::Result<Arc<Technogym>>;
    /// mywellness rejected the session: drop whatever is cached so the next `get` starts over.
    fn reset(&self);
    /// What to tell the user after an auth failure.
    fn login_hint(&self) -> &'static str;
}

/// The saved session wins. Configured credentials sign in when there is none, and at most ONCE
/// per process if that fails: a wrong password retried on every tool call would get the account
/// locked. After one successful sign-in the client keeps the credentials and renews an expired
/// token on its own.
pub struct FileSource {
    dir: PathBuf,
    credentials: Option<Credentials>,
    facility: Option<String>,
    base_urls: Option<(String, String)>,
    current: tokio::sync::Mutex<Option<Arc<Technogym>>>,
    attempted: AtomicBool,
    failure: std::sync::Mutex<Option<String>>,
}

impl FileSource {
    /// Sessions in `dir`; `credentials` sign in when there is none saved.
    pub fn new(
        dir: impl Into<PathBuf>,
        credentials: Option<Credentials>,
        facility: Option<String>,
    ) -> Self {
        Self {
            dir: dir.into(),
            credentials,
            facility,
            base_urls: None,
            current: tokio::sync::Mutex::new(None),
            attempted: AtomicBool::new(false),
            failure: std::sync::Mutex::new(None),
        }
    }

    /// Point the client at other hosts (tests, proxies).
    pub fn with_base_urls(
        mut self,
        core_url: impl Into<String>,
        services_url: impl Into<String>,
    ) -> Self {
        self.base_urls = Some((core_url.into(), services_url.into()));
        self
    }

    fn builder(&self) -> TechnogymBuilder {
        let mut b = Technogym::builder().session_store(FileSessionStore::new(&self.dir));
        if let Some(f) = &self.facility {
            b = b.facility(f.clone());
        }
        if let Some((core, services)) = &self.base_urls {
            b = b.core_url(core.clone()).services_url(services.clone());
        }
        b
    }

    async fn open(&self) -> technogym::Result<Arc<Technogym>> {
        let failure = self.failure.lock().expect("failure").clone();
        let mut builder = self.builder();
        // Credentials that already failed once are never handed to the client for automatic renewal.
        if let (Some(c), None) = (&self.credentials, &failure) {
            builder = builder.credentials(c.clone());
        }
        let api = builder.build();
        if api.load_session().await? {
            return Ok(Arc::new(api));
        }
        if let Some(c) = &self.credentials
            && !self.attempted.swap(true, Ordering::SeqCst)
        {
            match api.login(&c.email, &c.password).await {
                Ok(_) => return Ok(Arc::new(api)),
                Err(e) => {
                    *self.failure.lock().expect("failure") = Some(format!(
                        "signing in with the configured email and password failed: {e}"
                    ));
                }
            }
        }
        let reason = self
            .failure
            .lock()
            .expect("failure")
            .clone()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        Err(Error::Auth(format!(
            "no saved mywellness login in {}{reason}",
            self.dir.display()
        )))
    }
}

#[async_trait]
impl ApiSource for FileSource {
    async fn get(&self) -> technogym::Result<Arc<Technogym>> {
        let mut current = self.current.lock().await;
        if let Some(api) = &*current {
            return Ok(api.clone());
        }
        // A failure is never cached: a login made later is picked up on the next call.
        let api = self.open().await?;
        *current = Some(api.clone());
        Ok(api)
    }

    fn reset(&self) {
        if let Ok(mut current) = self.current.try_lock() {
            *current = None;
        }
    }

    fn login_hint(&self) -> &'static str {
        LOGIN_HINT
    }
}

/// One client, fixed for the lifetime of the source: the hosted server makes one per request.
pub struct FixedSource(pub Arc<Technogym>);

#[async_trait]
impl ApiSource for FixedSource {
    async fn get(&self) -> technogym::Result<Arc<Technogym>> {
        Ok(self.0.clone())
    }

    fn reset(&self) {}

    fn login_hint(&self) -> &'static str {
        RECONNECT_HINT
    }
}
