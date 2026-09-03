use async_trait::async_trait;

use crate::auth::{AuthError, TokenProvider};

/// A classic personal access token from config or `GITHUB_TOKEN`.
pub struct ClassicPat {
    token: String,
}

impl ClassicPat {
    pub fn new(token: String) -> Self {
        Self { token }
    }
}

#[async_trait]
impl TokenProvider for ClassicPat {
    fn name(&self) -> &'static str {
        "pat"
    }

    async fn token(&self) -> Result<String, AuthError> {
        if self.token.is_empty() {
            return Err(AuthError::NotConfigured(
                "set [github] auth_token or the GITHUB_TOKEN environment variable".into(),
            ));
        }
        Ok(self.token.clone())
    }

    async fn refresh(&self) -> Result<String, AuthError> {
        self.token().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_configured_token() {
        let provider = ClassicPat::new("ghp_abc".into());
        assert_eq!(provider.token().await.expect("token"), "ghp_abc");
        assert_eq!(provider.refresh().await.expect("refresh"), "ghp_abc");
    }

    #[tokio::test]
    async fn empty_token_is_error() {
        let provider = ClassicPat::new(String::new());
        assert!(matches!(
            provider.token().await,
            Err(AuthError::NotConfigured(_))
        ));
    }
}
