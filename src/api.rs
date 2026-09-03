use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::{
    assets::{app_js, Assets},
    auth::TokenProvider,
    config::{AuthProvider, Config, Workspace},
    db::Database,
    github::{self, RateLimit, Validation},
    sync::SyncStatus,
};

/// Shared application state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
    pub token: Arc<dyn TokenProvider>,
    pub github: github::Client,
    /// Cached credential validation, filled at startup (non-OAuth) or on the
    /// first `/api/state` call.
    pub validation: Arc<Mutex<Option<Validation>>>,
    pub sync_status: Arc<Mutex<SyncStatus>>,
    pub sync_trigger: mpsc::Sender<()>,
}

/// Build the axum router for the local HTTP server.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/app.js", get(app_js_handler))
        .route("/api/state", get(state_handler))
        .route("/api/sync", post(sync_handler))
        .fallback(static_handler)
        .with_state(state)
}

async fn root(State(state): State<AppState>) -> Response {
    let _ = state;
    serve_embedded("index.html")
}

async fn app_js_handler() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        app_js(),
    )
        .into_response()
}

#[derive(Serialize)]
struct StateResponse {
    version: String,
    workspaces: Vec<Workspace>,
    auth: AuthState,
    sync: SyncState,
}

#[derive(Serialize)]
struct AuthState {
    provider: String,
    authenticated: bool,
    login: Option<String>,
    scopes: Vec<String>,
    missing: Vec<String>,
    ok: bool,
}

#[derive(Serialize)]
struct SyncState {
    running: bool,
    last_sync: Option<String>,
    last_error: Option<String>,
    rate_limit: Option<RateLimit>,
}

async fn state_handler(State(state): State<AppState>) -> Response {
    let config = state.config.as_ref();
    let last_sync_db = state.db.get_sync_state("last_sync").unwrap_or_default();

    // Resolving the token may run the OAuth device flow on first use.
    let authenticated = state.token.token().await.is_ok();

    let validation = {
        let needs_validate = state
            .validation
            .lock()
            .expect("validation lock poisoned")
            .is_none();
        if needs_validate {
            if let Ok(v) = state.github.validate().await {
                *state.validation.lock().expect("validation lock poisoned") = Some(v);
            }
        }
        state
            .validation
            .lock()
            .expect("validation lock poisoned")
            .clone()
    };

    let sync = {
        let s = state.sync_status.lock().expect("sync status poisoned");
        SyncState {
            running: s.running,
            last_sync: s.last_sync.clone().or(last_sync_db),
            last_error: s.last_error.clone(),
            rate_limit: s.rate_limit.clone(),
        }
    };

    let auth = AuthState {
        provider: provider_name(config.github.auth_provider).to_string(),
        authenticated,
        login: validation.as_ref().and_then(|v| v.login.clone()),
        scopes: validation
            .as_ref()
            .map(|v| v.scopes.clone())
            .unwrap_or_default(),
        missing: validation
            .as_ref()
            .map(|v| v.missing.clone())
            .unwrap_or_default(),
        ok: validation.as_ref().map(|v| v.ok).unwrap_or(false),
    };

    let response = StateResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspaces: config.workspaces.clone(),
        auth,
        sync,
    };

    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap_or_default(),
    )
        .into_response()
}

async fn sync_handler(State(state): State<AppState>) -> Response {
    let _ = state.sync_trigger.send(()).await;
    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"accepted"}"#,
    )
        .into_response()
}

fn provider_name(provider: AuthProvider) -> &'static str {
    match provider {
        AuthProvider::Pat => "pat",
        AuthProvider::GhToken => "gh-token",
        AuthProvider::OAuthDevice => "oauth-device",
    }
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    serve_embedded(if path.is_empty() { "index.html" } else { path })
}

fn serve_embedded(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::pat::ClassicPat;
    use crate::config::GithubConfig;

    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = Config {
            github: GithubConfig {
                auth_provider: AuthProvider::Pat,
                auth_token: "ghp_test".into(),
                ..Default::default()
            },
            workspaces: vec![Workspace {
                name: "personal".into(),
                auto_dismiss_closed_merged: false,
                repo_sets: Default::default(),
            }],
        };
        let db = Database::open(&dir.path().join("data.db")).expect("open db");
        let token: Arc<dyn TokenProvider> = Arc::new(ClassicPat::new("ghp_test".into()));
        let (sync_trigger, _rx) = tokio::sync::mpsc::channel(8);
        AppState {
            config: Arc::new(config),
            db: Arc::new(db),
            github: github::Client::new(token.clone()),
            token,
            validation: Arc::new(Mutex::new(Some(Validation {
                login: Some("octocat".into()),
                scopes: vec!["notifications".into(), "repo".into()],
                missing: Vec::new(),
                sso_hint: None,
                ok: true,
                message: None,
            }))),
            sync_status: Arc::new(Mutex::new(SyncStatus::default())),
            sync_trigger,
        }
    }

    async fn get_body(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("dispatch");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn state_endpoint_reports_config() {
        let app = router(test_state());
        let (status, body) = get_body(app, "/api/state").await;
        assert_eq!(status, StatusCode::OK);
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["workspaces"][0]["name"], "personal");
        assert_eq!(value["auth"]["provider"], "pat");
        assert_eq!(value["auth"]["authenticated"], true);
        assert_eq!(value["auth"]["login"], "octocat");
        assert_eq!(value["auth"]["ok"], true);
    }

    #[tokio::test]
    async fn sync_endpoint_accepts_request() {
        let app = router(test_state());
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/sync")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn root_serves_index() {
        let app = router(test_state());
        let (status, body) = get_body(app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("github-notifications"));
    }

    #[tokio::test]
    async fn app_js_is_served() {
        let app = router(test_state());
        let (status, body) = get_body(app, "/app.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn unknown_path_is_404() {
        let app = router(test_state());
        let (status, _) = get_body(app, "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
