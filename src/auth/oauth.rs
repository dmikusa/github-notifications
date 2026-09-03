use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::auth::{AuthError, StoredToken, TokenProvider, TokenStore};
use crate::util::open_browser;

const DEFAULT_BASE: &str = "https://github.com";

/// OAuth device flow provider. Obtains a user access token by asking the user
/// to authorize in a browser, then persists the token in a `TokenStore`.
pub struct OAuthDevice {
    client_id: String,
    store: TokenStore,
    http: reqwest::Client,
    base: String,
    open_browser: bool,
}

impl OAuthDevice {
    pub fn new(client_id: String, store: TokenStore) -> Self {
        Self::with_base(client_id, store, DEFAULT_BASE)
    }

    fn with_base(client_id: String, store: TokenStore, base: &str) -> Self {
        Self {
            client_id,
            store,
            http: reqwest::Client::new(),
            base: base.to_string(),
            open_browser: true,
        }
    }

    /// Disable opening the browser during the flow (used in tests).
    #[cfg(test)]
    fn without_browser(mut self) -> Self {
        self.open_browser = false;
        self
    }

    async fn run_flow(&self) -> Result<String, AuthError> {
        // 1. Ask GitHub for a device code.
        let device: DeviceCodeResponse = self
            .http
            .post(format!("{}/login/device/code", self.base))
            .form(&[("client_id", self.client_id.as_str())])
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let verification = format!("{}?user_code={}", device.verification_uri, device.user_code);
        tracing::info!(
            "to authorize github-notifications, visit {verification} and enter code {}",
            device.user_code
        );
        if self.open_browser {
            open_browser(&verification);
        }

        // 2. Poll until the user authorizes (or the code expires).
        let mut interval = device.interval.max(1);
        loop {
            if interval > 0 {
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
            let params = [
                ("client_id", self.client_id.as_str()),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ];
            let body: Value = self
                .http
                .post(format!("{}/login/oauth/access_token", self.base))
                .form(&params)
                .header("Accept", "application/json")
                .send()
                .await?
                .json()
                .await?;

            if let Some(token) = body.get("access_token").and_then(Value::as_str) {
                return Ok(token.to_string());
            }

            match body.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += 5,
                Some("access_denied") => {
                    return Err(AuthError::OAuth("authorization was denied".into()));
                }
                Some("expired_token") => {
                    return Err(AuthError::OAuth(
                        "device code expired; please try again".into(),
                    ));
                }
                Some(other) => {
                    return Err(AuthError::OAuth(format!("unexpected error {other:?}")));
                }
                None => {
                    return Err(AuthError::OAuth(
                        "response contained neither access_token nor error".into(),
                    ));
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[allow(dead_code)]
    expires_in: u64,
    interval: u64,
}

#[async_trait]
impl TokenProvider for OAuthDevice {
    fn name(&self) -> &'static str {
        "oauth-device"
    }

    async fn token(&self) -> Result<String, AuthError> {
        if let Some(stored) = self.store.load()? {
            if !stored.access_token.is_empty() {
                return Ok(stored.access_token);
            }
        }
        if self.client_id.is_empty() {
            return Err(AuthError::NotConfigured(
                "set [github] oauth_client_id".into(),
            ));
        }
        let token = self.run_flow().await?;
        self.store.store(&StoredToken {
            access_token: token.clone(),
            scope: None,
        })?;
        Ok(token)
    }

    async fn refresh(&self) -> Result<String, AuthError> {
        self.token().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{routing::post, Router};
    use tokio::net::TcpListener;

    use super::*;
    /// A tiny mock GitHub that serves the device flow endpoints.
    async fn mock_github() -> String {
        let polls = std::sync::Arc::new(AtomicUsize::new(0));

        async fn device_code() -> impl axum::response::IntoResponse {
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                r#"{"device_code":"dev-1","user_code":"ABCD-EFGH","verification_uri":"http://localhost/verify","expires_in":900,"interval":0}"#,
            )
        }

        async fn access_token(
            polls: std::sync::Arc<AtomicUsize>,
        ) -> impl axum::response::IntoResponse {
            let n = polls.fetch_add(1, Ordering::SeqCst);
            let body = if n == 0 {
                r#"{"error":"authorization_pending"}"#
            } else {
                r#"{"access_token":"tok-42","token_type":"bearer"}"#
            };
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
        }

        let polls_for_token = polls.clone();
        let app = Router::new()
            .route("/login/device/code", post(device_code))
            .route(
                "/login/oauth/access_token",
                post(move || {
                    let polls = polls_for_token.clone();
                    async move { access_token(polls).await }
                }),
            );

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn device_flow_obtains_and_stores_token() {
        let base = mock_github().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path().join("auth.toml"));

        let provider = OAuthDevice::with_base("client-123".into(), store, &base).without_browser();
        let token = provider.token().await.expect("token");
        assert_eq!(token, "tok-42");

        // The token is persisted and reused without re-running the flow.
        let again = provider.token().await.expect("stored token");
        assert_eq!(again, "tok-42");
    }

    #[tokio::test]
    async fn device_flow_requires_client_id() {
        let base = mock_github().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenStore::new(dir.path().join("auth.toml"));
        let provider = OAuthDevice::with_base(String::new(), store, &base);
        assert!(matches!(
            provider.token().await,
            Err(AuthError::NotConfigured(_))
        ));
    }
}
