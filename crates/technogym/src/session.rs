//! A signed-in session and where it is kept between runs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::types::UserInfo;

/// A signed-in mywellness session: the bearer token and the account it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The bearer token sent with every request.
    pub token: String,
    /// The account, as the login response described it.
    pub user: UserInfo,
    /// When the session was created or its token last rotated, in epoch milliseconds.
    #[serde(default)]
    pub saved_at: u64,
}

/// Persists a client's session between runs. Implement it to keep sessions in a database or a
/// secret store; [`FileSessionStore`] and [`MemorySessionStore`] are ready-made.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// The saved session, or `None` when there is none (or it is unreadable).
    async fn load(&self) -> Result<Option<Session>>;
    /// Saves a session, replacing any saved before.
    async fn save(&self, session: &Session) -> Result<()>;
    /// Removes the saved session.
    async fn clear(&self) -> Result<()>;
}

/// Keeps the session in memory only, so it is lost when the process exits. The default.
#[derive(Debug, Default)]
pub struct MemorySessionStore(Mutex<Option<Session>>);

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn load(&self) -> Result<Option<Session>> {
        Ok(self.0.lock().expect("session mutex").clone())
    }

    async fn save(&self, session: &Session) -> Result<()> {
        *self.0.lock().expect("session mutex") = Some(session.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        *self.0.lock().expect("session mutex") = None;
        Ok(())
    }
}

const SESSION_FILE: &str = "session.json";

/// Stores the session as `session.json` (mode 0600) in a directory, created on the first save.
#[derive(Clone, Debug)]
pub struct FileSessionStore {
    dir: PathBuf,
}

impl FileSessionStore {
    /// A store in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The file the session is kept in.
    pub fn path(&self) -> PathBuf {
        self.dir.join(SESSION_FILE)
    }

    /// The directory given to [`FileSessionStore::new`].
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A file missing the token or the user id is rejected whole, so the caller signs in again
/// rather than carrying a half-valid session around.
fn valid(session: Session) -> Option<Session> {
    (!session.token.is_empty() && !session.user.user_id.is_empty()).then_some(session)
}

#[async_trait]
impl SessionStore for FileSessionStore {
    async fn load(&self) -> Result<Option<Session>> {
        let Ok(text) = tokio::fs::read(self.path()).await else {
            return Ok(None);
        };
        Ok(serde_json::from_slice::<Session>(&text)
            .ok()
            .and_then(valid))
    }

    async fn save(&self, session: &Session) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let json =
            serde_json::to_vec_pretty(session).map_err(|e| crate::Error::Other(e.to_string()))?;
        let dir = self.dir.clone();
        let target = self.path();
        // Unique temp file per call so two processes sharing the directory never collide.
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::io::Write as _;
            let mut tmp = tempfile::Builder::new()
                .prefix(".session-")
                .suffix(".tmp")
                .tempfile_in(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                tmp.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            tmp.write_all(&json)?;
            tmp.as_file().sync_all()?;
            tmp.persist(target).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(|e| crate::Error::Other(e.to_string()))??;
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        match tokio::fs::remove_file(self.path()).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}
