use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Serialize;

use crate::{
    assets::{app_js, Assets},
    config::{AuthProvider, Config, Workspace},
    db::Database,
};

/// Shared application state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Database>,
}

/// Build the axum router for the local HTTP server.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/app.js", get(app_js_handler))
        .route("/api/state", get(state_handler))
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
}

#[derive(Serialize)]
struct SyncState {
    last_sync: Option<String>,
    running: bool,
}

async fn state_handler(State(state): State<AppState>) -> Response {
    let config = state.config.as_ref();
    let last_sync = state.db.get_sync_state("last_sync").unwrap_or_default();

    let response = StateResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspaces: config.workspaces.clone(),
        auth: AuthState {
            provider: provider_name(config.github.auth_provider),
            authenticated: is_authenticated(config),
        },
        sync: SyncState {
            last_sync,
            running: false,
        },
    };

    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap_or_default(),
    )
        .into_response()
}

fn provider_name(provider: AuthProvider) -> String {
    match provider {
        AuthProvider::Pat => "pat",
        AuthProvider::GhToken => "gh-token",
        AuthProvider::OAuthDevice => "oauth-device",
    }
    .to_string()
}

/// Phase 0 heuristic for whether a credential is available. Phase 1 replaces
/// this with real validation (token scopes / live calls).
fn is_authenticated(config: &Config) -> bool {
    match config.github.auth_provider {
        AuthProvider::Pat => !config.effective_token().is_empty(),
        AuthProvider::GhToken => true,
        AuthProvider::OAuthDevice => false,
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
        AppState {
            config: Arc::new(config),
            db: Arc::new(db),
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
