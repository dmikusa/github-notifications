use std::sync::Arc;

use axum::http::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde_json::Value;
use thiserror::Error;

use crate::auth::{AuthError, TokenProvider};

/// Base URL for the GitHub REST API.
const DEFAULT_BASE: &str = "https://api.github.com";

/// The GitHub API version we target.
const API_VERSION: &str = "2022-11-28";

/// Minimal authenticated GitHub REST API client.
///
/// All requests carry the provider's bearer token. A 401 triggers a provider
/// refresh and a single retry, which handles gh-token rotation and expired
/// OAuth tokens.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    token: Arc<dyn TokenProvider>,
    base: String,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("auth: {0}")]
    Auth(#[from] AuthError),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub returned {0}: {1}")]
    Github(StatusCode, String),
}

impl Client {
    pub fn new(token: Arc<dyn TokenProvider>) -> Self {
        Self::with_base(token, DEFAULT_BASE)
    }

    fn with_base(token: Arc<dyn TokenProvider>, base: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!(
                "github-notifications/{}",
                env!("CARGO_PKG_VERSION")
            ))
            .expect("valid user agent"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("building http client");
        Self {
            http,
            token,
            base: base.to_string(),
        }
    }

    /// Perform an authenticated `GET`, returning status, headers, and body
    /// bytes. On a 401 the token provider is refreshed once and the request
    /// retried.
    async fn get(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), ClientError> {
        let mut attempt = 0;
        loop {
            let token = self.token.token().await?;
            let mut request = self
                .http
                .get(format!("{}{}", self.base, path))
                .bearer_auth(&token);
            if !params.is_empty() {
                request = request.query(params);
            }
            let response = request.send().await?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await?.to_vec();

            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.token.refresh().await?;
                attempt += 1;
                continue;
            }
            return Ok((status, headers, body));
        }
    }

    /// Validate the current credential against `GET /user`, returning login,
    /// granted scopes (from `X-OAuth-Scopes`), and any missing requirements or
    /// SAML SSO hints.
    pub async fn validate(&self) -> Result<Validation, ClientError> {
        let (status, headers, body) = self.get("/user", &[]).await?;

        let scopes = parse_scopes(headers.get("x-oauth-scopes"));
        let sso_hint = headers
            .get("x-github-sso")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let required = ["notifications", "repo"];
        let missing: Vec<String> = required
            .iter()
            .filter(|scope| !scopes.iter().any(|granted| granted == *scope))
            .map(|scope| (*scope).to_string())
            .collect();
        // The inbox needs either scope, not both.
        let has_inbox_scope = scopes.iter().any(|s| s == "notifications" || s == "repo");

        match status {
            StatusCode::OK => {
                let login = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("login").and_then(Value::as_str).map(str::to_string));
                let ok = has_inbox_scope;
                let message = if ok {
                    None
                } else {
                    Some(
                        "token lacks the `notifications` or `repo` scope; the inbox is unavailable"
                            .to_string(),
                    )
                };
                Ok(Validation {
                    login,
                    scopes,
                    missing: if ok { Vec::new() } else { missing },
                    sso_hint,
                    ok,
                    message,
                })
            }
            StatusCode::FORBIDDEN => Ok(Validation {
                login: None,
                scopes,
                missing,
                sso_hint,
                ok: false,
                message: Some("token forbidden; it may need SAML SSO authorization".to_string()),
            }),
            other => Ok(Validation {
                login: None,
                scopes,
                missing: Vec::new(),
                sso_hint: None,
                ok: false,
                message: Some(format!("unexpected status {other}")),
            }),
        }
    }
}

/// Parse the comma-separated `X-OAuth-Scopes` header into a list.
fn parse_scopes(value: Option<&HeaderValue>) -> Vec<String> {
    value
        .and_then(|v| v.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Result of credential validation against the GitHub API.
#[derive(Debug, Clone, Default)]
pub struct Validation {
    pub login: Option<String>,
    pub scopes: Vec<String>,
    pub missing: Vec<String>,
    pub sso_hint: Option<String>,
    pub ok: bool,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use axum::{extract::State, routing::get, Router};
    use tokio::net::TcpListener;

    use super::*;
    use crate::auth::pat::ClassicPat;

    #[derive(Clone)]
    struct MockState {
        scopes: &'static str,
        fail_first: bool,
        calls: std::sync::Arc<AtomicUsize>,
    }

    async fn user(State(state): State<MockState>) -> impl axum::response::IntoResponse {
        let n = state.calls.fetch_add(1, Ordering::SeqCst);
        if state.fail_first && n == 0 {
            return (
                StatusCode::UNAUTHORIZED,
                [("X-OAuth-Scopes", state.scopes)],
                r#"{"message":"bad credentials"}"#,
            );
        }
        (
            StatusCode::OK,
            [("X-OAuth-Scopes", state.scopes)],
            r#"{"login":"octocat"}"#,
        )
    }

    async fn mock_github(scopes: &'static str, fail_first: bool) -> String {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let state = MockState {
            scopes,
            fail_first,
            calls,
        };
        let app = Router::new().route("/user", get(user)).with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        format!("http://{addr}")
    }

    /// A provider serving a stale token until `refresh()` flips it to fresh.
    struct FlipPat {
        refreshed: AtomicBool,
    }

    impl FlipPat {
        fn new() -> Self {
            Self {
                refreshed: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl TokenProvider for FlipPat {
        fn name(&self) -> &'static str {
            "flip"
        }
        async fn token(&self) -> Result<String, AuthError> {
            if self.refreshed.load(Ordering::SeqCst) {
                Ok("ghp_fresh".into())
            } else {
                Ok("ghp_stale".into())
            }
        }
        async fn refresh(&self) -> Result<String, AuthError> {
            self.refreshed.store(true, Ordering::SeqCst);
            self.token().await
        }
    }

    #[tokio::test]
    async fn validate_reports_ok_scopes() {
        let base = mock_github("notifications, repo, read:org", false).await;
        let client = Client::with_base(Arc::new(ClassicPat::new("ghp_x".into())), &base);
        let v = client.validate().await.expect("validate");
        assert!(v.ok);
        assert_eq!(v.login.as_deref(), Some("octocat"));
        assert!(v.scopes.contains(&"notifications".to_string()));
        assert!(v.missing.is_empty());
        assert!(v.sso_hint.is_none());
    }

    #[tokio::test]
    async fn validate_flags_missing_inbox_scope() {
        let base = mock_github("read:org", false).await;
        let client = Client::with_base(Arc::new(ClassicPat::new("ghp_x".into())), &base);
        let v = client.validate().await.expect("validate");
        assert!(!v.ok);
        assert!(v
            .missing
            .iter()
            .any(|s| s == "notifications" || s == "repo"));
        assert_eq!(v.login.as_deref(), Some("octocat"));
    }

    #[tokio::test]
    async fn get_retries_once_after_401() {
        let base = mock_github("repo", true).await;
        let client = Client::with_base(Arc::new(FlipPat::new()), &base);
        // The mock 401s on the first request; the client should refresh and
        // retry, landing on a successful validation.
        let v = client.validate().await.expect("validate");
        assert!(v.ok);
        assert_eq!(v.login.as_deref(), Some("octocat"));
    }
}
