pub mod gh;
pub mod oauth;
pub mod pat;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{AuthProvider as ProviderKind, Config};

/// A source of GitHub API bearer tokens.
///
/// Implementations may resolve the token from config, from the `gh` CLI, or by
/// running an interactive OAuth device flow. Tokens are opaque to callers; only
/// the provider knows how to obtain and refresh them.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// Provider name surfaced in the UI ("pat", "gh-token", "oauth-device").
    fn name(&self) -> &'static str;

    /// The current bearer token, running any one-time flows needed to obtain
    /// one (e.g. the OAuth device authorization flow).
    async fn token(&self) -> Result<String, AuthError>;

    /// Drop any cached token and re-resolve. Called when the API reports a 401
    /// so the next call picks up a fresh credential.
    async fn refresh(&self) -> Result<String, AuthError>;
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no token configured: {0}")]
    NotConfigured(String),
    #[error("GitHub CLI is not logged in; run `gh auth login`")]
    GhNotLoggedIn,
    #[error("OAuth device flow failed: {0}")]
    OAuth(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

/// Persisted OAuth tokens (gitignored, private permissions).
pub struct TokenStore {
    path: PathBuf,
}

/// A stored token file entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredToken {
    pub access_token: String,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenStore {
    /// A token store rooted at `path` (e.g. `<data_dir>/auth.toml`).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Read the stored token, if any.
    pub fn load(&self) -> Result<Option<StoredToken>, AuthError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&self.path)?;
        Ok(Some(toml::from_str(&raw)?))
    }

    /// Persist a token, creating parent directories and locking down
    /// permissions.
    pub fn store(&self, token: &StoredToken) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string(token)?;
        std::fs::write(&self.path, raw)?;
        set_private_permissions(&self.path);
        Ok(())
    }
}

/// Build the configured token provider for `config`.
pub fn from_config(config: &Config, store: TokenStore) -> Arc<dyn TokenProvider> {
    match config.github.auth_provider {
        ProviderKind::Pat => Arc::new(pat::ClassicPat::new(config.effective_token())),
        ProviderKind::GhToken => Arc::new(gh::GhToken::new()),
        ProviderKind::OAuthDevice => Arc::new(oauth::OAuthDevice::new(
            config.github.oauth_client_id.clone(),
            store,
        )),
    }
}

/// Restrict file permissions so only the owner can read the file. Best effort;
/// failures are ignored on platforms without POSIX permissions.
#[cfg(unix)]
fn set_private_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(
            "could not set private permissions on {}: {e}",
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_store_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path().join("auth.toml"));
        assert!(store.load().expect("load").is_none());

        store
            .store(&StoredToken {
                access_token: "gho_123".into(),
                scope: Some("notifications,repo".into()),
            })
            .expect("store");

        let loaded = store.load().expect("load").expect("present");
        assert_eq!(loaded.access_token, "gho_123");
        assert_eq!(loaded.scope.as_deref(), Some("notifications,repo"));
    }

    #[cfg(unix)]
    #[test]
    fn token_store_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path().join("auth.toml"));
        store
            .store(&StoredToken {
                access_token: "gho_123".into(),
                scope: None,
            })
            .expect("store");
        let mode = std::fs::metadata(store.path.clone())
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn from_config_builds_expected_provider() {
        let config = Config {
            github: crate::config::GithubConfig {
                auth_provider: ProviderKind::GhToken,
                ..Default::default()
            },
            ..Default::default()
        };
        let provider = from_config(&config, TokenStore::new("/tmp/ignored".into()));
        assert_eq!(provider.name(), "gh-token");
    }
}
