use std::sync::Mutex;

use async_trait::async_trait;
use tokio::process::Command;

use crate::auth::{AuthError, TokenProvider};

/// Reuses the token managed by the GitHub CLI (`gh auth token`), cached until
/// a 401 triggers a refresh.
pub struct GhToken {
    cached: Mutex<Option<String>>,
}

impl GhToken {
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    async fn fetch() -> Result<String, AuthError> {
        let output = match Command::new("gh").args(["auth", "token"]).output().await {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AuthError::GhNotLoggedIn);
            }
            Err(e) => return Err(AuthError::Io(e)),
        };
        if !output.status.success() {
            return Err(AuthError::GhNotLoggedIn);
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl Default for GhToken {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenProvider for GhToken {
    fn name(&self) -> &'static str {
        "gh-token"
    }

    async fn token(&self) -> Result<String, AuthError> {
        if let Some(token) = self.cached.lock().expect("gh cache poisoned").clone() {
            return Ok(token);
        }
        let token = Self::fetch().await?;
        *self.cached.lock().expect("gh cache poisoned") = Some(token.clone());
        Ok(token)
    }

    async fn refresh(&self) -> Result<String, AuthError> {
        *self.cached.lock().expect("gh cache poisoned") = None;
        self.token().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh_logged_in() -> bool {
        std::process::Command::new("gh")
            .args(["auth", "status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn resolves_token_when_gh_present() {
        // CI runners ship gh but are not logged in; only run this when a real
        // session exists so the test stays deterministic in CI.
        if !gh_logged_in() {
            eprintln!("skipping: gh not logged in");
            return;
        }
        let provider = GhToken::new();
        let token = provider.token().await.expect("token from gh");
        assert!(!token.is_empty());
        // Second call is served from cache.
        let again = provider.token().await.expect("cached token");
        assert_eq!(token, again);
    }

    #[tokio::test]
    async fn fails_gracefully_when_gh_missing() {
        // Simulate a missing gh by running the fetch against an empty PATH.
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", "/nonexistent");
        let provider = GhToken::new();
        let result = provider.token().await;
        std::env::set_var("PATH", original.unwrap_or_default());
        assert!(matches!(result, Err(AuthError::GhNotLoggedIn)));
    }
}
