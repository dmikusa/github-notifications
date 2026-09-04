use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::header::HeaderMap;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::db::Database;
use crate::github::{self, next_link, Client, RateLimit};
use crate::models::{GithubIssue, GithubPullRequest, NotificationThread, WatchedRepo};

/// Live view of sync state shared with the API handlers.
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    pub running: bool,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub rate_limit: Option<RateLimit>,
}

/// Background sync engine handle.
pub struct SyncEngine {
    pub status: Arc<Mutex<SyncStatus>>,
    pub trigger: mpsc::Sender<()>,
}

impl SyncEngine {
    /// Spawn the background sync loop. Runs an initial full sync immediately,
    /// then refreshes on the configured poll interval; a manual sync can be
    /// requested via [`SyncEngine::request_sync`].
    pub fn spawn(client: Client, db: Arc<Database>, config: Arc<Config>) -> Self {
        let (trigger, mut rx) = mpsc::channel(8);
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        {
            let client = client.clone();
            let db = db.clone();
            let config = config.clone();
            let status = status.clone();
            tokio::spawn(async move {
                run_sync(&client, &db, &config, &status, true).await;
                let interval = Duration::from_secs(config.github.poll_interval_seconds.max(30));
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await; // consume the immediate first tick
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {
                            run_sync(&client, &db, &config, &status, false).await;
                        }
                        _ = rx.recv() => {
                            run_sync(&client, &db, &config, &status, true).await;
                        }
                    }
                }
            });
        }

        Self { status, trigger }
    }

    /// Ask the engine to run a full sync as soon as possible.
    pub async fn request_sync(&self) {
        let _ = self.trigger.send(()).await;
    }
}

/// Run a single full sync pass (notifications, repo refresh, watches,
/// auto-dismiss) with a throwaway status. Returns the sync timestamp.
///
/// Public so integration tests (e.g. the online smoke check in `tests/`) can
/// run one pass against live GitHub.
pub async fn sync_all(client: &Client, db: &Database, config: &Config) -> Result<String> {
    let status = Arc::new(Mutex::new(SyncStatus::default()));
    sync_once(client, db, config, &status, true).await
}

/// One pass of the sync engine. `force` bypasses the repo-refresh cadence
/// (used for the initial sync and manual triggers).
async fn run_sync(
    client: &Client,
    db: &Database,
    config: &Config,
    status: &Arc<Mutex<SyncStatus>>,
    force: bool,
) {
    {
        let mut s = status.lock().expect("sync status poisoned");
        if s.running {
            return;
        }
        s.running = true;
        s.last_error = None;
    }

    let result = sync_once(client, db, config, status, force).await;

    let mut s = status.lock().expect("sync status poisoned");
    s.running = false;
    match result {
        Ok(now) => s.last_sync = Some(now),
        Err(e) => {
            s.last_error = Some(e.to_string());
            tracing::warn!("sync failed: {e}");
        }
    }
}

async fn sync_once(
    client: &Client,
    db: &Database,
    config: &Config,
    status: &Arc<Mutex<SyncStatus>>,
    force: bool,
) -> Result<String> {
    let now = now_utc();

    sync_notifications(client, db, status).await?;

    let repo_due = {
        let last = db.get_sync_state("last_repo_refresh")?;
        if force {
            true
        } else {
            due(&last, config.github.repo_refresh_interval_seconds)
        }
    };
    if repo_due {
        sync_repos(client, db, status, config).await?;
        sync_watches(client, db, status).await?;
        db.set_sync_state("last_repo_refresh", &now)?;
    }

    maybe_auto_dismiss(client, db, config).await?;

    db.set_sync_state("last_sync", &now)?;
    Ok(now)
}

async fn sync_notifications(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
) -> Result<()> {
    let etag_key = "etag:notifications";
    let etag = db.get_sync_state(etag_key)?;
    let since = db.get_sync_state("last_notification_sync")?;

    let mut params: Vec<(&str, &str)> = vec![("per_page", "50"), ("all", "true")];
    if let Some(s) = &since {
        params.push(("since", s.as_str()));
    }

    let mut page_url: Option<String> = None;
    let mut first = true;
    loop {
        let response = match &page_url {
            Some(url) => client.get_url(url, None).await?,
            None => {
                client
                    .get(
                        "/notifications",
                        &params,
                        if first { etag.as_deref() } else { None },
                    )
                    .await?
            }
        };

        record_rate_limit(status, &response.headers);

        if response.status == axum::http::StatusCode::NOT_MODIFIED {
            break;
        }
        if response.status != axum::http::StatusCode::OK {
            return Err(anyhow::anyhow!(
                "notifications endpoint returned {}",
                response.status
            ));
        }

        let threads: Vec<NotificationThread> =
            serde_json::from_slice(&response.body).context("parsing notifications response")?;
        for thread in &threads {
            db.upsert_thread(thread)?;
        }

        if let Some(etag) = response.headers.get("etag").and_then(|v| v.to_str().ok()) {
            db.set_sync_state(etag_key, etag)?;
        }

        page_url = next_link(&response.headers);
        first = false;
        if page_url.is_none() {
            break;
        }
    }

    db.set_sync_state("last_notification_sync", &now_utc())?;
    Ok(())
}

async fn sync_repos(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    config: &Config,
) -> Result<()> {
    for full_name in tracked_repos(config) {
        let Some((owner, name)) = full_name.split_once('/') else {
            continue;
        };
        let etag_key = format!("etag:issues:{full_name}");
        let etag = db.get_sync_state(&etag_key)?;
        let path = format!("/repos/{owner}/{name}/issues");

        let mut page_url: Option<String> = None;
        let mut first = true;
        loop {
            let response = match &page_url {
                Some(url) => client.get_url(url, None).await?,
                None => {
                    client
                        .get(
                            &path,
                            &[("state", "open"), ("per_page", "100")],
                            if first { etag.as_deref() } else { None },
                        )
                        .await?
                }
            };

            record_rate_limit(status, &response.headers);

            if response.status == axum::http::StatusCode::NOT_MODIFIED {
                break;
            }
            if response.status != axum::http::StatusCode::OK {
                return Err(anyhow::anyhow!(
                    "issues endpoint for {full_name} returned {}",
                    response.status
                ));
            }

            let issues: Vec<GithubIssue> = serde_json::from_slice(&response.body)
                .with_context(|| format!("parsing issues for {full_name}"))?;

            let repo_id =
                db.upsert_repo(&full_name, Some(&format!("https://github.com/{full_name}")))?;
            for issue in issues {
                let kind = if issue.pull_request.is_some() {
                    "pr"
                } else {
                    "issue"
                };
                db.upsert_issue(repo_id, &issue, kind, None)?;
            }

            if let Some(etag) = response.headers.get("etag").and_then(|v| v.to_str().ok()) {
                db.set_sync_state(&etag_key, etag)?;
            }

            page_url = next_link(&response.headers);
            first = false;
            if page_url.is_none() {
                break;
            }
        }

        db.set_repo_refreshed(&full_name, &now_utc())?;
    }
    Ok(())
}

async fn sync_watches(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
) -> Result<()> {
    db.clear_watched()?;
    let mut page_url: Option<String> = None;
    loop {
        let response = match &page_url {
            Some(url) => client.get_url(url, None).await?,
            None => {
                client
                    .get("/user/subscriptions", &[("per_page", "100")], None)
                    .await?
            }
        };

        record_rate_limit(status, &response.headers);

        if response.status != axum::http::StatusCode::OK {
            return Err(anyhow::anyhow!(
                "subscriptions endpoint returned {}",
                response.status
            ));
        }
        let repos: Vec<WatchedRepo> =
            serde_json::from_slice(&response.body).context("parsing subscriptions response")?;
        for repo in &repos {
            db.upsert_watched_repo(&repo.full_name, &repo.html_url)?;
        }
        page_url = next_link(&response.headers);
        if page_url.is_none() {
            break;
        }
    }
    Ok(())
}

async fn maybe_auto_dismiss(client: &Client, db: &Database, config: &Config) -> Result<()> {
    if !config
        .workspaces
        .iter()
        .any(|w| w.auto_dismiss_closed_merged)
    {
        return Ok(());
    }

    let mut seen = BTreeSet::new();
    for (thread_id, thread_api_url, subject_api_url) in db.get_unread_pr_threads()? {
        if !seen.insert(subject_api_url.clone()) {
            continue;
        }
        // The numeric thread id used by the GitHub thread endpoints (the
        // notification `id` like "1:111" is the local key instead).
        let Some(numeric_id) = thread_api_url.rsplit('/').next().map(str::to_string) else {
            continue;
        };

        let etag_key = format!("etag:pr:{subject_api_url}");
        let etag = db.get_sync_state(&etag_key)?;
        let response = client.get_url(&subject_api_url, etag.as_deref()).await?;
        if response.status == axum::http::StatusCode::NOT_MODIFIED {
            continue;
        }
        if response.status != axum::http::StatusCode::OK {
            continue; // e.g. deleted subject; skip
        }
        if let Some(etag) = response.headers.get("etag").and_then(|v| v.to_str().ok()) {
            db.set_sync_state(&etag_key, etag)?;
        }

        let pr: GithubPullRequest = match serde_json::from_slice(&response.body) {
            Ok(pr) => pr,
            Err(_) => continue,
        };
        if pr.state == "closed" && pr.merged_at.is_some() {
            let path = format!("/notifications/threads/{numeric_id}");
            let res = client.patch(&path).await?;
            if matches!(
                res.status,
                axum::http::StatusCode::OK
                    | axum::http::StatusCode::NO_CONTENT
                    | axum::http::StatusCode::RESET_CONTENT
            ) {
                db.set_thread_unread(&thread_id, false)?;
                tracing::info!("auto-dismissed merged PR thread {thread_id}");
            }
        }
    }
    Ok(())
}

fn record_rate_limit(status: &Arc<Mutex<SyncStatus>>, headers: &HeaderMap) {
    let rl = github::rate_limit_from(headers);
    status.lock().expect("sync status poisoned").rate_limit = Some(rl);
}

/// All repos across all workspaces' repo sets, deduplicated and sorted.
fn tracked_repos(config: &Config) -> Vec<String> {
    let mut set = BTreeSet::new();
    for workspace in &config.workspaces {
        for repo in workspace.tracked_repos() {
            set.insert(repo);
        }
    }
    set.into_iter().collect()
}

/// Whether the stored timestamp (RFC3339) is older than `interval_seconds`.
fn due(last: &Option<String>, interval_seconds: u64) -> bool {
    match last {
        None => true,
        Some(raw) => {
            let last = DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&Utc))
                .ok();
            match last {
                Some(last) => {
                    Utc::now() - last >= chrono::Duration::seconds(interval_seconds as i64)
                }
                None => true,
            }
        }
    }
}

/// Current UTC time in RFC3339 with milliseconds.
pub fn now_utc() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_repos_dedupes_and_sorts() {
        let config = Config {
            workspaces: vec![
                crate::config::Workspace {
                    name: "a".into(),
                    auto_dismiss_closed_merged: false,
                    repo_sets: vec![crate::config::RepoSet {
                        name: "s1".into(),
                        repos: vec!["b/repo".into(), "a/repo".into()],
                    }],
                },
                crate::config::Workspace {
                    name: "b".into(),
                    auto_dismiss_closed_merged: false,
                    repo_sets: vec![crate::config::RepoSet {
                        name: "s2".into(),
                        repos: vec!["a/repo".into(), String::new()],
                    }],
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            tracked_repos(&config),
            vec!["a/repo".to_string(), "b/repo".to_string()]
        );
    }

    #[test]
    fn due_respects_interval() {
        assert!(due(&None, 60));
        assert!(due(&Some("not-a-date".into()), 60));
        let recent = now_utc();
        assert!(!due(&Some(recent), 60));
    }

    /// A mock GitHub serving notifications, open issues, subscriptions, a PR
    /// detail, and thread mark-read. Subject/thread URLs embed the mock base.
    async fn mock_github() -> String {
        use axum::{routing::get, routing::patch, Router};
        use serde_json::json;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let notifications = json!([
            {
                "id": "1:111",
                "unread": true,
                "reason": "mention",
                "updated_at": "2026-09-03T12:00:00Z",
                "last_read_at": null,
                "subject": {
                    "title": "PR title",
                    "type": "PullRequest",
                    "url": format!("{base}/repos/o/r/pulls/7"),
                    "latest_comment_url": null
                },
                "repository": {"full_name": "o/r", "html_url": "https://github.com/o/r"},
                "url": format!("{base}/notifications/threads/111")
            },
            {
                "id": "2:222",
                "unread": true,
                "reason": "assign",
                "updated_at": "2026-09-03T12:00:00Z",
                "last_read_at": null,
                "subject": {
                    "title": "Issue title",
                    "type": "Issue",
                    "url": format!("{base}/repos/o/r/issues/3"),
                    "latest_comment_url": null
                },
                "repository": {"full_name": "o/r", "html_url": "https://github.com/o/r"},
                "url": format!("{base}/notifications/threads/222")
            }
        ])
        .to_string();

        let issues = json!([
            {
                "id": 1, "number": 3, "title": "an issue", "state": "open",
                "user": {"login": "a"},
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
                "closed_at": null,
                "html_url": format!("{base}/repos/o/r/issues/3"),
                "url": format!("{base}/repos/o/r/issues/3")
            },
            {
                "id": 2, "number": 7, "title": "a pr", "state": "open",
                "user": {"login": "b"},
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
                "closed_at": null,
                "html_url": format!("{base}/repos/o/r/pull/7"),
                "url": format!("{base}/repos/o/r/pulls/7"),
                "pull_request": {"url": format!("{base}/repos/o/r/pulls/7")}
            }
        ])
        .to_string();

        let app = Router::new()
            .route(
                "/notifications",
                get(move || async move { ([("ETag", "\"n1\"")], notifications.clone()) }),
            )
            .route(
                "/repos/o/r/issues",
                get(move || async move { ([("ETag", "\"i1\"")], issues.clone()) }),
            )
            .route(
                "/user/subscriptions",
                get(|| async { r#"[{"full_name":"o/r","html_url":"https://github.com/o/r"}]"# }),
            )
            .route(
                "/repos/o/r/pulls/7",
                get(|| async { r#"{"state":"closed","merged_at":"2026-02-01T00:00:00Z"}"# }),
            )
            .route(
                "/notifications/threads/111",
                patch(|| async { axum::http::StatusCode::RESET_CONTENT }),
            );

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        base
    }

    #[tokio::test]
    async fn sync_once_caches_notifications_repos_and_auto_dismisses() {
        let base = mock_github().await;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");

        let config = Config {
            github: crate::config::GithubConfig {
                auth_provider: crate::config::AuthProvider::Pat,
                auth_token: "ghp_x".into(),
                poll_interval_seconds: 60,
                repo_refresh_interval_seconds: 60,
                ..Default::default()
            },
            workspaces: vec![crate::config::Workspace {
                name: "test".into(),
                auto_dismiss_closed_merged: true,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["o/r".into()],
                }],
            }],
        };

        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        let result = sync_once(&client, &db, &config, &status, true).await;
        assert!(result.is_ok(), "sync failed: {:?}", result.err());

        assert!(db.count("repos").expect("count") >= 1);
        assert_eq!(db.count("issues").expect("count"), 2);
        assert_eq!(db.count("threads").expect("count"), 2);
        // Auto-dismiss marks the merged PR thread read; the issue thread stays.
        assert_eq!(db.unread_thread_count().expect("unread"), 1);
    }
}
