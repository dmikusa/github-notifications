use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    assets::{app_js, Assets},
    auth::TokenProvider,
    config::{self, AuthProvider, Config, Workspace},
    db::Database,
    github::{self, RateLimit, Validation},
    models,
    sync::SyncStatus,
    views,
};

/// Shared application state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    /// Path of the config file, used for write-back.
    pub config_path: PathBuf,
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
        .route("/api/sync/status", get(sync_status_handler))
        .route("/api/views/queue", get(queue_view))
        .route("/api/views/inbox", get(inbox_view))
        .route("/api/views/repos", get(repos_view))
        .route("/api/views/settings", get(settings_view))
        .route("/api/threads/mark-read", post(threads_mark_read))
        .route("/api/issues/mark-read", post(issues_mark_read))
        .route("/api/threads/mute", post(thread_mute))
        .route("/api/repos/{owner}/{repo}/watch", post(repo_watch))
        .route("/api/repos/{owner}/{repo}/unwatch", post(repo_unwatch))
        .route("/api/workspaces/{name}/repo-sets", get(workspace_repo_sets))
        .route("/api/workspaces/{name}/repo-sets", post(repo_set_create))
        .route(
            "/api/workspaces/{name}/repo-sets/{set}",
            delete(repo_set_delete),
        )
        .route(
            "/api/workspaces/{name}/repo-sets/{set}/repos/{repo}",
            delete(repo_delete),
        )
        .route("/api/orgs/{org}/repos", get(org_repos))
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
    let (workspaces, provider) = {
        let config = state.config.read().expect("config lock poisoned");
        (config.workspaces.clone(), config.github.auth_provider)
    };
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
        provider: provider_name(provider).to_string(),
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
        workspaces,
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

fn html_response(html: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn json_response(result: Result<impl Serialize>) -> Response {
    match result {
        Ok(value) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&value).unwrap_or_else(|_| "{}".into()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/json")],
            format!(r#"{{"error":{:?}}}"#, e.to_string()),
        )
            .into_response(),
    }
}

async fn queue_view(
    State(state): State<AppState>,
    Query(params): Query<views::QueueParams>,
) -> Response {
    let config = state.config.read().expect("config lock poisoned");
    let ws = views::resolve_workspace(&config, &params.ws);
    match views::render_queue(&state.db, ws, &params) {
        Ok(html) => html_response(html),
        Err(e) => json_response(Err::<(), _>(e)),
    }
}

async fn inbox_view(
    State(state): State<AppState>,
    Query(params): Query<views::InboxParams>,
) -> Response {
    let config = state.config.read().expect("config lock poisoned");
    let ws = views::resolve_workspace(&config, &params.ws);
    match views::render_inbox(&state.db, ws, &params) {
        Ok(html) => html_response(html),
        Err(e) => json_response(Err::<(), _>(e)),
    }
}

async fn repos_view(
    State(state): State<AppState>,
    Query(params): Query<views::RepoParams>,
) -> Response {
    let config = state.config.read().expect("config lock poisoned");
    let ws = views::resolve_workspace(&config, &params.ws);
    match views::render_repos(&state.db, ws, &params) {
        Ok(html) => html_response(html),
        Err(e) => json_response(Err::<(), _>(e)),
    }
}

async fn settings_view(
    State(state): State<AppState>,
    Query(params): Query<views::RepoParams>,
) -> Response {
    let config = state.config.read().expect("config lock poisoned");
    let ws = views::resolve_workspace(&config, &params.ws);
    match views::render_settings(ws) {
        Ok(html) => html_response(html),
        Err(e) => json_response(Err::<(), _>(e)),
    }
}

#[derive(Serialize)]
struct SyncStatusResponse {
    /// Whether the cache has been populated by at least one completed sync.
    populated: bool,
    running: bool,
    last_sync: Option<String>,
    last_error: Option<String>,
    /// Set when the cache was rebuilt (e.g. after a schema change).
    rebuild: Option<String>,
}

async fn sync_status_handler(State(state): State<AppState>) -> Response {
    let status = state
        .sync_status
        .lock()
        .expect("sync status poisoned")
        .clone();
    let populated = status.last_sync.is_some()
        || state
            .db
            .get_sync_state("last_sync")
            .map(|v| v.is_some())
            .unwrap_or(false);
    let response = SyncStatusResponse {
        populated,
        running: status.running,
        last_sync: status.last_sync,
        last_error: status.last_error,
        rebuild: state.db.get_sync_state("last_rebuild").unwrap_or_default(),
    };
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&response).unwrap_or_default(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ThreadsMarkReadBody {
    ids: Option<Vec<String>>,
    all: Option<bool>,
    ws: Option<String>,
}

async fn threads_mark_read(
    State(state): State<AppState>,
    Json(body): Json<ThreadsMarkReadBody>,
) -> Response {
    let result = async {
        if body.all == Some(true) {
            let ws = {
                let config = state.config.read().expect("config lock poisoned");
                let ws = views::resolve_workspace(&config, body.ws.as_deref().unwrap_or(""));
                ws.tracked_repos()
            };
            let res = state.github.put("/notifications").await?;
            ensure_success(&res.status, "mark all notifications read")?;
            state.db.set_unread_for_repos(&ws, false)?;
            return Ok::<usize, anyhow::Error>(ws.len());
        }
        let ids = body.ids.unwrap_or_default();
        mark_threads_read(&state, &ids).await
    }
    .await;
    json_response(result)
}

#[derive(Deserialize)]
struct IssuesMarkReadBody {
    ids: Vec<i64>,
}

async fn issues_mark_read(
    State(state): State<AppState>,
    Json(body): Json<IssuesMarkReadBody>,
) -> Response {
    let result = async {
        let mut thread_ids = Vec::new();
        for id in body.ids {
            if let Some(api_url) = state.db.issue_api_url(id)? {
                for (thread_id, _api) in state.db.threads_for_subject(&api_url)? {
                    thread_ids.push(thread_id);
                }
            }
        }
        mark_threads_read(&state, &thread_ids).await
    }
    .await;
    json_response(result)
}

#[derive(Deserialize)]
struct MuteBody {
    id: String,
}

async fn thread_mute(State(state): State<AppState>, Json(body): Json<MuteBody>) -> Response {
    let result = async {
        let Some(api_url) = state.db.thread_api_url(&body.id)? else {
            return Ok::<(), anyhow::Error>(());
        };
        let Some(numeric) = api_url.rsplit('/').next().map(str::to_string) else {
            return Ok(());
        };
        let res = state
            .github
            .delete(&format!("/notifications/threads/{numeric}/subscription"))
            .await?;
        ensure_success(&res.status, "mute thread")?;
        Ok(())
    }
    .await;
    json_response(result)
}

async fn repo_watch(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> Response {
    let full = format!("{owner}/{repo}");
    let result = async {
        let res = state
            .github
            .put(&format!("/user/subscriptions/{owner}/{repo}"))
            .await?;
        ensure_success(&res.status, "watch repo")?;
        state.db.set_repo_watched(&full, true)?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json_response(result)
}

async fn repo_unwatch(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> Response {
    let full = format!("{owner}/{repo}");
    let result = async {
        let res = state
            .github
            .delete(&format!("/user/subscriptions/{owner}/{repo}"))
            .await?;
        ensure_success(&res.status, "unwatch repo")?;
        state.db.set_repo_watched(&full, false)?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json_response(result)
}

#[derive(Serialize)]
struct RepoSetInfo {
    name: String,
    repos: Vec<String>,
}

async fn workspace_repo_sets(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let result = async {
        let config = state.config.read().expect("config lock poisoned");
        let ws = views::resolve_workspace(&config, &name);
        let sets = ws
            .repo_sets
            .iter()
            .map(|s| RepoSetInfo {
                name: s.name.clone(),
                repos: s.repos.clone(),
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(sets)
    }
    .await;
    json_response(result)
}

#[derive(Deserialize)]
struct RepoSetBody {
    name: String,
    #[serde(default)]
    repos: Vec<String>,
}

async fn repo_set_create(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<RepoSetBody>,
) -> Response {
    let result = async {
        config::add_repo_set(&state.config_path, &name, &body.name, &body.repos)?;
        reload_config(&state)?;
        let _ = state.sync_trigger.send(()).await;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json_response(result)
}

async fn repo_set_delete(
    State(state): State<AppState>,
    Path((name, set)): Path<(String, String)>,
) -> Response {
    let result = async {
        config::remove_repo_set(&state.config_path, &name, &set)?;
        reload_config(&state)?;
        let _ = state.sync_trigger.send(()).await;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json_response(result)
}

async fn repo_delete(
    State(state): State<AppState>,
    Path((name, set, repo)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        config::remove_repo(&state.config_path, &name, &set, &repo)?;
        reload_config(&state)?;
        let _ = state.sync_trigger.send(()).await;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    json_response(result)
}

#[derive(Deserialize)]
struct OrgReposParams {
    page: Option<u32>,
    q: Option<String>,
}

#[derive(Serialize)]
struct OrgRepo {
    full_name: String,
    html_url: String,
}

#[derive(Serialize)]
struct OrgReposResponse {
    repos: Vec<OrgRepo>,
    has_more: bool,
}

async fn org_repos(
    State(state): State<AppState>,
    Path(org): Path<String>,
    Query(params): Query<OrgReposParams>,
) -> Response {
    let result = async {
        let page = params.page.unwrap_or(1).max(1);
        let page_str = page.to_string();
        let res = state
            .github
            .get(
                &format!("/orgs/{org}/repos"),
                &[("per_page", "100"), ("page", page_str.as_str())],
                None,
            )
            .await?;
        ensure_success(&res.status, "list org repos")?;
        let repos: Vec<models::WatchedRepo> = serde_json::from_slice(&res.body)?;
        let has_more = github::has_next(&res.headers);
        let q = params.q.unwrap_or_default();
        let repos = repos
            .into_iter()
            .filter(|r| q.is_empty() || r.full_name.contains(&q))
            .map(|r| OrgRepo {
                full_name: r.full_name,
                html_url: r.html_url,
            })
            .collect();
        Ok::<_, anyhow::Error>(OrgReposResponse { repos, has_more })
    }
    .await;
    json_response(result)
}

/// Reload the in-memory config from disk after a write-back so UI edits take
/// effect without a restart.
fn reload_config(state: &AppState) -> Result<()> {
    let config = config::read_file(&state.config_path)?;
    *state.config.write().expect("config lock poisoned") = config;
    Ok(())
}

/// Mark a set of threads read on GitHub and locally. Returns the count marked.
async fn mark_threads_read(state: &AppState, thread_ids: &[String]) -> Result<usize> {
    let mut count = 0;
    for id in thread_ids {
        let Some(api_url) = state.db.thread_api_url(id)? else {
            continue;
        };
        let Some(numeric) = api_url.rsplit('/').next().map(str::to_string) else {
            continue;
        };
        let res = state
            .github
            .patch(&format!("/notifications/threads/{numeric}"))
            .await?;
        if matches!(
            res.status,
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT
        ) {
            state
                .db
                .set_threads_unread(std::slice::from_ref(id), false)?;
            count += 1;
        }
    }
    Ok(count)
}

fn ensure_success(status: &StatusCode, what: &str) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        anyhow::bail!("{what} returned {status}")
    }
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
            config: Arc::new(RwLock::new(config)),
            config_path: dir.path().join("config.toml"),
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

    fn state_with_client(db: Arc<Database>, client: github::Client) -> AppState {
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
                repo_sets: vec![crate::config::RepoSet {
                    name: "mine".into(),
                    repos: vec!["o/r".into()],
                }],
            }],
        };
        let token: Arc<dyn TokenProvider> = Arc::new(ClassicPat::new("ghp_test".into()));
        let (sync_trigger, _rx) = tokio::sync::mpsc::channel(8);
        AppState {
            config: Arc::new(RwLock::new(config)),
            config_path: dir.path().join("config.toml"),
            db,
            github: client,
            token,
            validation: Arc::new(Mutex::new(None)),
            sync_status: Arc::new(Mutex::new(SyncStatus::default())),
            sync_trigger,
        }
    }

    async fn mock_github_actions() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::{delete, patch, put};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let patch_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/notifications/threads/111",
                patch({
                    let calls = patch_calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            StatusCode::RESET_CONTENT
                        }
                    }
                }),
            )
            .route(
                "/user/subscriptions/o/r",
                put(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/user/subscriptions/o/r",
                delete(|| async { StatusCode::NO_CONTENT }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (base, patch_calls)
    }

    #[tokio::test]
    async fn issues_mark_read_calls_github_and_updates_local() {
        let (base, patch_calls) = mock_github_actions().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(Database::open(&dir.path().join("data.db")).expect("db"));

        let repo_id = db
            .upsert_repo("o/r", Some("https://github.com/o/r"))
            .expect("repo");
        let issue: crate::models::GithubIssue = serde_json::from_value(serde_json::json!({
            "id": 1, "number": 3, "title": "t", "state": "open",
            "user": {"login": "a"},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            "closed_at": null,
            "html_url": "https://github.com/o/r/issues/3",
            "url": "https://api.github.com/repos/o/r/issues/3"
        }))
        .expect("issue");
        db.upsert_issue(repo_id, &issue, "issue", None)
            .expect("issue row");

        let thread = crate::models::NotificationThread {
            id: "1:111".into(),
            unread: true,
            reason: "mention".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            last_read_at: None,
            subject: crate::models::ThreadSubject {
                title: "t".into(),
                kind: "Issue".into(),
                url: Some("https://api.github.com/repos/o/r/issues/3".into()),
                latest_comment_url: None,
            },
            repository: Some(crate::models::ThreadRepository {
                full_name: "o/r".into(),
                html_url: "https://github.com/o/r".into(),
            }),
            url: "https://api.github.com/notifications/threads/111".into(),
        };
        db.upsert_thread(&thread).expect("thread row");

        let issue_id = db.with_conn(|conn| {
            conn.query_row("SELECT id FROM issues WHERE number = 3", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("issue id")
        });

        let client = github::Client::with_base(Arc::new(ClassicPat::new("ghp_x".into())), &base);
        let app = router(state_with_client(db.clone(), client));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/issues/mark-read")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"ids":[{issue_id}]}}"#)))
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(patch_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        let unread = db.unread_thread_count().expect("unread");
        assert_eq!(unread, 0);
    }

    #[tokio::test]
    async fn repo_watch_and_unwatch_update_local_flag() {
        let (base, _patch_calls) = mock_github_actions().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(Database::open(&dir.path().join("data.db")).expect("db"));
        db.upsert_repo("o/r", Some("https://github.com/o/r"))
            .expect("repo");

        let client = github::Client::with_base(Arc::new(ClassicPat::new("ghp_x".into())), &base);
        let app = router(state_with_client(db.clone(), client));

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/repos/o/r/watch")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        let watched = db.with_conn(|conn| {
            conn.query_row(
                "SELECT is_watched FROM repos WHERE full_name='o/r'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .expect("watched")
        });
        assert_eq!(watched, 1);

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/repos/o/r/unwatch")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        let watched = db.with_conn(|conn| {
            conn.query_row(
                "SELECT is_watched FROM repos WHERE full_name='o/r'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .expect("watched")
        });
        assert_eq!(watched, 0);
    }

    fn state_with_config_file(body: &str) -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, body).expect("write config");
        let config = config::read_file(&config_path).expect("read config");
        let db = Arc::new(Database::open(&dir.path().join("data.db")).expect("db"));
        let token: Arc<dyn TokenProvider> = Arc::new(ClassicPat::new("ghp_test".into()));
        let (sync_trigger, _rx) = tokio::sync::mpsc::channel(8);
        let state = AppState {
            config: Arc::new(RwLock::new(config)),
            config_path,
            db,
            github: github::Client::new(token.clone()),
            token,
            validation: Arc::new(Mutex::new(None)),
            sync_status: Arc::new(Mutex::new(SyncStatus::default())),
            sync_trigger,
        };
        (state, dir)
    }

    #[tokio::test]
    async fn repo_set_endpoints_update_config() {
        let (state, _dir) = state_with_config_file(
            "[github]\nauth_provider = \"gh-token\"\n\n[[workspaces]]\nname = \"personal\"\n",
        );
        let app = router(state.clone());

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/workspaces/personal/repo-sets")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"mine","repos":["a/r","b/r"]}"#))
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);

        // Config file was updated, comments aside.
        let raw = std::fs::read_to_string(&state.config_path).expect("read config");
        assert!(raw.contains("a/r"));

        // In-memory shared config was reloaded.
        {
            let cfg = state.config.read().expect("lock");
            assert_eq!(cfg.workspaces[0].repo_sets[0].name, "mine");
            assert_eq!(
                cfg.workspaces[0].repo_sets[0].repos,
                vec!["a/r".to_string(), "b/r".to_string()]
            );
        }

        // The list endpoint reflects it.
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/workspaces/personal/repo-sets")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("mine"));
        assert!(body.contains("a/r"));
    }

    #[tokio::test]
    async fn repo_set_delete_updates_config() {
        let (state, _dir) = state_with_config_file(
            "[github]\nauth_provider = \"gh-token\"\n\n[[workspaces]]\nname = \"personal\"\n",
        );
        let app = router(state.clone());

        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/workspaces/personal/repo-sets")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"mine","repos":["a/r"]}"#))
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/api/workspaces/personal/repo-sets/mine")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        let cfg = state.config.read().expect("lock");
        assert!(cfg.workspaces[0].repo_sets.is_empty());
    }

    #[tokio::test]
    async fn org_repos_browses_and_filters() {
        use axum::routing::get;

        let app = Router::new().route(
            "/orgs/o/repos",
            get(|| async {
                (
                    [(
                        "link",
                        "<https://api.github.com/orgs/o/repos?page=2>; rel=\"next\"",
                    )],
                    r#"[
                        {"full_name":"o/repo-a","html_url":"https://github.com/o/repo-a"},
                        {"full_name":"o/repo-b","html_url":"https://github.com/o/repo-b"}
                    ]"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(Database::open(&dir.path().join("data.db")).expect("db"));
        let client = github::Client::with_base(Arc::new(ClassicPat::new("ghp_x".into())), &base);
        let app = router(state_with_client(db, client));

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/orgs/o/repos?q=repo-a")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("dispatch");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("o/repo-a"));
        assert!(!body.contains("o/repo-b"));
        assert!(body.contains("\"has_more\":true"));
    }
}
