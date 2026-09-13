use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::header::HeaderMap;
use tokio::sync::mpsc;

use crate::config::{Config, Workspace};
use crate::db::Database;
use crate::github::{self, next_link, Client, RateLimit};
use crate::models::{
    ActionsRuns, CheckRun, GithubIssue, GithubPullRequest, NotificationThread, RepoSubscription,
    WatchedRepo,
};

/// The workspace shown when the config has no workspaces yet.
static EMPTY_WORKSPACE: LazyLock<Workspace> = LazyLock::new(Workspace::default);

/// Resolve the workspace named `name`, falling back to the first configured
/// workspace, or a static empty workspace when none exist.
fn resolve_workspace<'a>(config: &'a Config, name: &str) -> &'a Workspace {
    if let Some(ws) = config.workspaces.iter().find(|w| w.name == name) {
        return ws;
    }
    if let Some(ws) = config.workspaces.first() {
        return ws;
    }
    &EMPTY_WORKSPACE
}

/// Live view of sync state shared with the API handlers.
#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    pub running: bool,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub rate_limit: Option<RateLimit>,
    /// Whether a manual "dismiss closed/merged" pass is in flight.
    pub dismiss_running: bool,
    /// Count from the last completed manual dismiss pass.
    pub last_dismiss: Option<usize>,
}

/// Background sync engine handle.
pub struct SyncEngine {
    pub status: Arc<Mutex<SyncStatus>>,
    pub trigger: mpsc::Sender<()>,
    /// The workspace the UI is currently viewing; the sync loop syncs its repos.
    pub current_workspace: Arc<Mutex<String>>,
}

impl SyncEngine {
    /// Spawn the background sync loop. Runs an initial sync for the first
    /// workspace immediately, then refreshes the current workspace on the
    /// configured poll interval; a sync can be requested via
    /// [`SyncEngine::request_sync`]. Only the current workspace's repos are
    /// fetched, so switching workspaces (via `/api/workspaces/{name}/activate`)
    /// syncs a different set. The shared config is read fresh each pass so
    /// UI-driven edits take effect without a restart. Requests are queued
    /// (never cancelled), so switching mid-sync just syncs the new workspace
    /// after the current pass finishes.
    pub fn spawn(client: Client, db: Arc<Database>, config: Arc<RwLock<Config>>) -> Self {
        let (trigger, mut rx) = mpsc::channel(8);
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let current_workspace = Arc::new(Mutex::new(
            config
                .read()
                .expect("config lock poisoned")
                .workspaces
                .first()
                .map(|w| w.name.clone())
                .unwrap_or_default(),
        ));

        {
            let client = client.clone();
            let db = db.clone();
            let config = config.clone();
            let status = status.clone();
            let current_workspace = current_workspace.clone();
            tokio::spawn(async move {
                let initial_config = config.read().expect("config lock poisoned").clone();
                let initial_ws =
                    resolve_workspace(&initial_config, &current_workspace.lock().expect("lock"));
                run_sync(&client, &db, &initial_config, &status, initial_ws, true).await;
                let interval =
                    Duration::from_secs(initial_config.github.poll_interval_seconds.max(30));
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await; // consume the immediate first tick
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {
                            let cfg = config.read().expect("config lock poisoned").clone();
                            let ws = resolve_workspace(&cfg, &current_workspace.lock().expect("lock"));
                            run_sync(&client, &db, &cfg, &status, ws, false).await;
                        }
                        _ = rx.recv() => {
                            let cfg = config.read().expect("config lock poisoned").clone();
                            let ws = resolve_workspace(&cfg, &current_workspace.lock().expect("lock"));
                            run_sync(&client, &db, &cfg, &status, ws, true).await;
                        }
                    }
                }
            });
        }

        Self {
            status,
            trigger,
            current_workspace,
        }
    }

    /// Ask the engine to run a full sync as soon as possible.
    pub async fn request_sync(&self) {
        let _ = self.trigger.send(()).await;
    }

    /// Record the active workspace (called when the UI switches workspaces).
    pub fn set_current_workspace(&self, name: &str) {
        *self.current_workspace.lock().expect("lock") = name.to_string();
    }
}

/// Run a single sync pass (notifications, repo refresh, watches, auto-dismiss)
/// for the first configured workspace with a throwaway status. Public so
/// integration tests (e.g. the online smoke check in `tests/`) can run one
/// pass against live GitHub.
pub async fn sync_all(client: &Client, db: &Database, config: &Config) -> Result<String> {
    let status = Arc::new(Mutex::new(SyncStatus::default()));
    let name = config
        .workspaces
        .first()
        .map(|w| w.name.clone())
        .unwrap_or_default();
    let ws = resolve_workspace(config, &name);
    sync_once(client, db, config, &status, ws, true).await
}

/// One pass of the sync engine for `workspace`. `force` bypasses the
/// repo-refresh cadence (used for the initial sync and manual triggers).
async fn run_sync(
    client: &Client,
    db: &Database,
    config: &Config,
    status: &Arc<Mutex<SyncStatus>>,
    workspace: &Workspace,
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

    let result = sync_once(client, db, config, status, workspace, force).await;

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
    workspace: &Workspace,
    force: bool,
) -> Result<String> {
    let now = now_utc();

    sync_notifications(client, db, status).await?;

    // Repo freshness is tracked per workspace: each workspace only fetches its
    // own repos, and it's refreshed when that workspace becomes active again.
    let repo_refresh_key = format!("last_repo_refresh:{}", workspace.name);
    let repo_due = {
        let last = db.get_sync_state(&repo_refresh_key)?;
        if force {
            true
        } else {
            due(&last, config.github.repo_refresh_interval_seconds)
        }
    };
    if repo_due {
        sync_repos(client, db, status, workspace, config).await?;
        sync_watches(client, db, status).await?;
        sync_repo_subscriptions(client, db, status, workspace, config).await?;
        refresh_subject_states(client, db, status, workspace, config).await?;
        db.set_sync_state(&repo_refresh_key, &now)?;
    }

    maybe_auto_dismiss(client, db, config, workspace).await?;

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
        // Dismissed merged-PR threads (subject API URLs) are never re-added.
        let dismissed = db.dismissed_subjects()?;
        for thread in &threads {
            if thread
                .subject
                .url
                .as_deref()
                .is_some_and(|u| dismissed.contains(u))
            {
                continue;
            }
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
    workspace: &Workspace,
    config: &Config,
) -> Result<()> {
    use futures::stream::{self, StreamExt};

    let results: Vec<Result<()>> = stream::iter(workspace.tracked_repos())
        .map(|full_name| Box::pin(sync_one_repo(client, db, status, full_name)))
        .buffer_unordered(config.github.effective_sync_concurrency())
        .collect()
        .await;
    for result in results {
        result?;
    }
    Ok(())
}

/// Fetch the open issues/PRs for a single tracked repo and cache them.
async fn sync_one_repo(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    full_name: String,
) -> Result<()> {
    let Some((owner, name)) = full_name.split_once('/') else {
        return Ok(());
    };
    let etag_key = format!("etag:issues:{full_name}");
    let etag = db.get_sync_state(&etag_key)?;
    let path = format!("/repos/{owner}/{name}/issues");
    let repo_id = db.upsert_repo(&full_name, Some(&format!("https://github.com/{full_name}")))?;
    // The cached open set lets us clean stale rows even on a 304. Until we
    // have one, fetch unconditionally so existing stale rows get cleaned.
    let cached_open = db.issue_open_set(&full_name)?;
    let use_conditional = !cached_open.is_empty();

    let mut page_url: Option<String> = None;
    let mut first = true;
    let mut fetched_urls: Vec<String> = Vec::new();
    let mut fetched_any = false;
    loop {
        let response = match &page_url {
            Some(url) => client.get_url(url, None).await?,
            None => {
                client
                    .get(
                        &path,
                        &[("state", "open"), ("per_page", "100")],
                        if first && use_conditional {
                            etag.as_deref()
                        } else {
                            None
                        },
                    )
                    .await?
            }
        };

        record_rate_limit(status, &response.headers);

        if response.status == axum::http::StatusCode::NOT_MODIFIED {
            // The open list is unchanged; clean stale rows against the cached set.
            db.delete_stale_issues_not_in(repo_id, &cached_open)?;
            break;
        }
        if response.status != axum::http::StatusCode::OK {
            // A missing or inaccessible repo (404/403) shouldn't abort the
            // whole sync; skip it and continue with the rest.
            tracing::warn!(
                "skipping {full_name}: issues endpoint returned {}",
                response.status
            );
            break;
        }

        fetched_any = true;
        let issues: Vec<GithubIssue> = serde_json::from_slice(&response.body)
            .with_context(|| format!("parsing issues for {full_name}"))?;
        for issue in issues {
            let kind = if issue.pull_request.is_some() {
                "pr"
            } else {
                "issue"
            };
            fetched_urls.push(issue.url.clone());
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

    // Drop cached open issues that are no longer open on GitHub (closed or
    // merged since the last fetch) so the queue never shows stale items, and
    // remember the current open set for 304-time cleanup.
    if fetched_any {
        db.set_issue_open_set(&full_name, &fetched_urls)?;
        db.delete_stale_issues_not_in(repo_id, &fetched_urls)?;
    }

    db.set_repo_refreshed(&full_name, &now_utc())?;
    Ok(())
}

async fn sync_watches(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
) -> Result<()> {
    let etag_key = "etag:subscriptions";
    let etag = db.get_sync_state(etag_key)?;
    let mut page_url: Option<String> = None;
    let mut first = true;
    loop {
        let response = match &page_url {
            Some(url) => client.get_url(url, None).await?,
            None => {
                client
                    .get(
                        "/user/subscriptions",
                        &[("per_page", "100")],
                        if first { etag.as_deref() } else { None },
                    )
                    .await?
            }
        };

        record_rate_limit(status, &response.headers);

        if response.status == axum::http::StatusCode::NOT_MODIFIED {
            // The watched list is unchanged; keep the existing flags.
            break;
        }
        if response.status != axum::http::StatusCode::OK {
            return Err(anyhow::anyhow!(
                "subscriptions endpoint returned {}",
                response.status
            ));
        }

        // Only reset the flags once we have a fresh list.
        if first {
            db.clear_watched()?;
        }
        let repos: Vec<WatchedRepo> =
            serde_json::from_slice(&response.body).context("parsing subscriptions response")?;
        for repo in &repos {
            db.upsert_watched_repo(&repo.full_name, &repo.html_url)?;
        }

        if first {
            if let Some(etag) = response.headers.get("etag").and_then(|v| v.to_str().ok()) {
                db.set_sync_state(etag_key, etag)?;
            }
        }

        page_url = next_link(&response.headers);
        first = false;
        if page_url.is_none() {
            break;
        }
    }
    Ok(())
}

/// Refresh the per-repo subscription state (`ignored` vs `participating`) for
/// every tracked repo. Watched repos are skipped — the watched list already
/// knows them. Unchanged subscriptions return 304 and only bump the check
/// time; a 404 means "participating and @mentions" (no explicit subscription).
async fn sync_repo_subscriptions(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    workspace: &Workspace,
    config: &Config,
) -> Result<()> {
    use futures::stream::{self, StreamExt};

    let results: Vec<Result<()>> = stream::iter(workspace.tracked_repos())
        .map(|full_name| Box::pin(sync_one_subscription(client, db, status, full_name)))
        .buffer_unordered(config.github.effective_sync_concurrency())
        .collect()
        .await;
    for result in results {
        result?;
    }
    Ok(())
}

/// Fetch a single repo's subscription state and cache it.
async fn sync_one_subscription(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    full_name: String,
) -> Result<()> {
    let Some((owner, name)) = full_name.split_once('/') else {
        return Ok(());
    };
    if db.is_repo_watched(&full_name)? {
        return Ok(());
    }
    db.upsert_repo(&full_name, Some(&format!("https://github.com/{full_name}")))?;

    let etag = db.repo_subscription_etag(&full_name)?;
    let path = format!("/repos/{owner}/{name}/subscription");
    let response = client.get(&path, &[], etag.as_deref()).await?;
    record_rate_limit(status, &response.headers);

    let checked_at = now_utc();
    match response.status {
        axum::http::StatusCode::NOT_MODIFIED => {
            db.touch_repo_subscription(&full_name, &checked_at)?;
        }
        axum::http::StatusCode::OK => {
            let sub: RepoSubscription = serde_json::from_slice(&response.body)
                .with_context(|| format!("parsing subscription for {full_name}"))?;
            let state = if sub.ignored {
                "ignored"
            } else if sub.subscribed {
                "watched"
            } else {
                "participating"
            };
            let etag = response
                .headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            db.set_repo_subscription(&full_name, state, etag.as_deref(), &checked_at)?;
        }
        axum::http::StatusCode::NOT_FOUND => {
            db.set_repo_subscription(&full_name, "participating", None, &checked_at)?;
        }
        other => {
            tracing::warn!("skipping {full_name}: subscription endpoint returned {other}");
        }
    }
    Ok(())
}

/// Fetch the current state of PR and check-run subjects so the inbox can show
/// open/closed/merged and pass/fail. ETag-cached per thread; unchanged subjects
/// return 304 and only bump the check time.
async fn refresh_subject_states(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    workspace: &Workspace,
    config: &Config,
) -> Result<()> {
    use futures::stream::{self, StreamExt};

    let results: Vec<Result<()>> =
        stream::iter(db.subject_threads_needing_refresh(&workspace.tracked_repos())?)
            .map(|thread| Box::pin(sync_one_subject_state(client, db, status, thread)))
            .buffer_unordered(config.github.effective_sync_concurrency())
            .collect()
            .await;
    for result in results {
        result?;
    }
    Ok(())
}

/// Fetch one subject's status and cache it.
async fn sync_one_subject_state(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    thread: crate::db::SubjectThread,
) -> Result<()> {
    let url: Option<String> =
        if thread.subject_type == "PullRequest" || thread.subject_type == "Issue" {
            thread.subject_api_url.clone()
        } else {
            thread
                .subject_check_url
                .clone()
                .or(thread.subject_api_url.clone())
        };
    let Some(url) = url else {
        // CheckSuite notifications carry no subject URL, so resolve the
        // workflow run from the title (one-shot; the outcome is final).
        if thread.subject_type == "CheckSuite" {
            resolve_check_from_title(client, db, status, &thread).await?;
        }
        return Ok(());
    };

    let etag = db.subject_state_etag(&thread.thread_id)?;
    let response = client.get_url(&url, etag.as_deref()).await?;
    record_rate_limit(status, &response.headers);

    let checked_at = now_utc();
    match response.status {
        axum::http::StatusCode::NOT_MODIFIED => {
            db.touch_subject_state(&thread.thread_id, &checked_at)?;
        }
        axum::http::StatusCode::OK => {
            let etag = response
                .headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if thread.subject_type == "PullRequest" {
                let pr: GithubPullRequest = serde_json::from_slice(&response.body)
                    .with_context(|| format!("parsing PR subject for {}", thread.thread_id))?;
                let state = if pr.merged_at.is_some() {
                    "merged"
                } else if pr.state == "closed" {
                    "closed"
                } else {
                    "open"
                };
                db.set_subject_state(
                    &thread.thread_id,
                    state,
                    Some(&pr.html_url),
                    etag.as_deref(),
                    &checked_at,
                )?;
                db.set_subject_author(
                    &thread.thread_id,
                    pr.user.as_ref().map(|u| u.login.as_str()),
                )?;
            } else if thread.subject_type == "Issue" {
                let issue: GithubIssue = serde_json::from_slice(&response.body)
                    .with_context(|| format!("parsing issue subject for {}", thread.thread_id))?;
                let state = if issue.state == "closed" {
                    "closed"
                } else {
                    "open"
                };
                db.set_subject_state(
                    &thread.thread_id,
                    state,
                    Some(&issue.html_url),
                    etag.as_deref(),
                    &checked_at,
                )?;
                db.set_subject_author(
                    &thread.thread_id,
                    issue.user.as_ref().map(|u| u.login.as_str()),
                )?;
            } else {
                let run: CheckRun = serde_json::from_slice(&response.body)
                    .with_context(|| format!("parsing check run for {}", thread.thread_id))?;
                let state = run
                    .conclusion
                    .as_deref()
                    .or(run.status.as_deref())
                    .unwrap_or("unknown");
                db.set_subject_state(
                    &thread.thread_id,
                    state,
                    run.html_url.as_deref(),
                    etag.as_deref(),
                    &checked_at,
                )?;
            }
        }
        other => {
            tracing::warn!(
                "skipping subject for {}: {} returned {other}",
                thread.thread_id,
                url
            );
        }
    }
    Ok(())
}

/// Resolve a CheckSuite thread's workflow run from its title, e.g.
/// "Renovate workflow run failed for main branch". The notifications API
/// provides no URL for these, so we parse the title and look the run up via
/// `GET /repos/{o}/{r}/actions/runs`. The outcome is final, so this runs once
/// per thread.
async fn resolve_check_from_title(
    client: &Client,
    db: &Database,
    status: &Arc<Mutex<SyncStatus>>,
    thread: &crate::db::SubjectThread,
) -> Result<()> {
    let checked_at = now_utc();
    let Some((name, status_word, branch)) = parse_workflow_title(&thread.subject_title) else {
        // Not a workflow-run notification (e.g. a dependency name); nothing to show.
        db.set_subject_state(&thread.thread_id, "unresolved", None, None, &checked_at)?;
        return Ok(());
    };
    let conclusion = status_word_to_conclusion(&status_word);
    let run_url = match (&thread.repo, &thread.updated_at) {
        (repo, Some(updated_at)) => {
            resolve_workflow_run(client, status, repo, &name, conclusion, &branch, updated_at).await
        }
        _ => None,
    };
    let state = conclusion.unwrap_or(status_word.as_str());
    db.set_subject_state(
        &thread.thread_id,
        state,
        run_url.as_deref(),
        None,
        &checked_at,
    )?;
    Ok(())
}

/// Parse `"<workflow> workflow run[, Attempt #N] <status> for <branch> branch"`
/// into (workflow name, status word, branch).
fn parse_workflow_title(title: &str) -> Option<(String, String, String)> {
    let (name, rest) = title.split_once(" workflow run")?;
    let mut rest = rest.trim_start();
    if let Some(r) = rest.strip_prefix(", Attempt #") {
        let digits = r.bytes().take_while(|b| b.is_ascii_digit()).count();
        rest = r[digits..].trim_start();
    }
    let (status, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let branch = rest.strip_prefix("for ")?.strip_suffix(" branch")?.trim();
    if name.trim().is_empty() || branch.is_empty() {
        return None;
    }
    Some((
        name.trim().to_string(),
        status.to_string(),
        branch.to_string(),
    ))
}

/// Map the status word in a title ("failed", "succeeded", ...) to the GitHub
/// run conclusion.
fn status_word_to_conclusion(word: &str) -> Option<&'static str> {
    match word {
        "failed" => Some("failure"),
        "succeeded" => Some("success"),
        "cancelled" => Some("cancelled"),
        "timed_out" => Some("timed_out"),
        "skipped" => Some("skipped"),
        _ => None,
    }
}

/// Find the workflow run for a repo/branch whose name and conclusion match the
/// parsed title, choosing the one nearest to the notification time.
async fn resolve_workflow_run(
    client: &Client,
    status: &Arc<Mutex<SyncStatus>>,
    repo: &str,
    workflow_name: &str,
    conclusion: Option<&str>,
    branch: &str,
    updated_at: &str,
) -> Option<String> {
    let (owner, name) = repo.split_once('/')?;
    let thread_time = DateTime::parse_from_rfc3339(updated_at).ok()?;
    // Runs are newest-first; the notification fires shortly after completion.
    let window_start = thread_time - chrono::Duration::minutes(15);
    let window_end = thread_time + chrono::Duration::minutes(5);

    let path = format!("/repos/{owner}/{name}/actions/runs");
    let mut page_url: Option<String> = None;
    let mut best: Option<(String, chrono::Duration)> = None;
    for _ in 0..10 {
        let response = match &page_url {
            Some(u) => client.get_url(u, None).await.ok()?,
            None => client
                .get(&path, &[("branch", branch), ("per_page", "100")], None)
                .await
                .ok()?,
        };
        record_rate_limit(status, &response.headers);
        if response.status != axum::http::StatusCode::OK {
            return None;
        }
        let runs: ActionsRuns = serde_json::from_slice(&response.body).ok()?;
        for run in runs.workflow_runs {
            let created = match DateTime::parse_from_rfc3339(&run.created_at) {
                Ok(dt) => dt,
                Err(_) => continue,
            };
            if created < window_start {
                // Past the time window; newest-first, so we're done.
                return best.map(|(url, _)| url);
            }
            if created > window_end || run.name != workflow_name {
                continue;
            }
            if let Some(c) = conclusion {
                if run.conclusion.as_deref() != Some(c) {
                    continue;
                }
            }
            let dist = (created - thread_time).abs();
            if best.as_ref().map(|(_, d)| dist < *d).unwrap_or(true) {
                best = Some((run.html_url.clone(), dist));
            }
        }
        page_url = next_link(&response.headers);
        if page_url.is_none() {
            break;
        }
    }
    best.map(|(url, _)| url)
}

async fn maybe_auto_dismiss(
    client: &Client,
    db: &Database,
    config: &Config,
    workspace: &Workspace,
) -> Result<()> {
    if !config
        .workspaces
        .iter()
        .any(|w| w.auto_dismiss_closed_merged)
    {
        return Ok(());
    }
    let _ = dismiss_closed_merged(client, db, workspace).await?;
    Ok(())
}
/// Remove every pull-request notification thread whose PR is closed (whether
/// merged or not), limited to the given workspace's repos. Unread threads
/// are marked read on GitHub; all such threads are dropped from the local
/// cache and remembered so the next notification sync doesn't re-add them.
/// Returns the number dismissed. Used by the auto-dismiss option and by a
/// manual "dismiss closed/merged" action in the UI.
pub async fn dismiss_closed_merged(
    client: &Client,
    db: &Database,
    workspace: &Workspace,
) -> Result<usize> {
    let mut count = 0;
    let mut dismissed_ids = Vec::new();
    let mut dismissed_subjects = Vec::new();
    // Whether each subject PR is closed; dedupes the per-PR fetch across
    // the (possibly multiple) threads that reference the same PR.
    let mut closed: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for thread in db.get_pr_threads(&workspace.tracked_repos())? {
        let Some(numeric_id) = thread.api_url.rsplit('/').next().map(str::to_string) else {
            continue;
        };
        let subject_closed = match closed.get(&thread.subject_api_url) {
            Some(m) => *m,
            None => {
                let m = match thread.subject_state.as_deref() {
                    Some("merged") | Some("closed") => true,
                    _ => {
                        let etag_key = format!("etag:pr:{}", thread.subject_api_url);
                        let etag = db.get_sync_state(&etag_key)?;
                        let response = client
                            .get_url(&thread.subject_api_url, etag.as_deref())
                            .await?;
                        if response.status != axum::http::StatusCode::OK {
                            false // 304 (unchanged) or deleted subject; skip
                        } else {
                            if let Some(etag) =
                                response.headers.get("etag").and_then(|v| v.to_str().ok())
                            {
                                db.set_sync_state(&etag_key, etag)?;
                            }
                            match serde_json::from_slice::<GithubPullRequest>(&response.body) {
                                Ok(pr) => pr.state == "closed",
                                Err(_) => false,
                            }
                        }
                    }
                };
                closed.insert(thread.subject_api_url.clone(), m);
                m
            }
        };
        if !subject_closed {
            continue;
        }
        if thread.unread {
            let path = format!("/notifications/threads/{numeric_id}");
            let res = client.patch(&path).await?;
            if !matches!(
                res.status,
                axum::http::StatusCode::OK
                    | axum::http::StatusCode::NO_CONTENT
                    | axum::http::StatusCode::RESET_CONTENT
            ) {
                continue;
            }
        }
        count += 1;
        dismissed_ids.push(thread.thread_id.clone());
        dismissed_subjects.push(thread.subject_api_url.clone());
        tracing::info!("dismissed merged PR thread {}", thread.thread_id);
    }
    if !dismissed_ids.is_empty() {
        db.record_dismissed_subjects(&dismissed_subjects)?;
        db.delete_threads(&dismissed_ids)?;
    }
    Ok(count)
}

fn record_rate_limit(status: &Arc<Mutex<SyncStatus>>, headers: &HeaderMap) {
    let rl = github::rate_limit_from(headers);
    status.lock().expect("sync status poisoned").rate_limit = Some(rl);
}

/// Whether the stored timestamp (RFC3339) is older than `interval_seconds`.
pub fn due(last: &Option<String>, interval_seconds: u64) -> bool {
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
    fn parse_workflow_title_parses() {
        let (n, s, b) =
            parse_workflow_title("Renovate workflow run failed for main branch").expect("parse");
        assert_eq!(
            (n.as_str(), s.as_str(), b.as_str()),
            ("Renovate", "failed", "main")
        );

        let (n, s, b) =
            parse_workflow_title("Build and Release workflow run failed for main branch")
                .expect("parse");
        assert_eq!(
            (n.as_str(), s.as_str(), b.as_str()),
            ("Build and Release", "failed", "main")
        );

        let (n, s, b) = parse_workflow_title(
            "Java PR Test Build workflow run, Attempt #2 failed for java/spring-boot branch",
        )
        .expect("parse");
        assert_eq!(
            (n.as_str(), s.as_str(), b.as_str()),
            ("Java PR Test Build", "failed", "java/spring-boot")
        );

        assert!(parse_workflow_title("org.apache.artemis:artemis-core-client:2.50.0").is_none());
        assert_eq!(status_word_to_conclusion("failed"), Some("failure"));
        assert_eq!(status_word_to_conclusion("succeeded"), Some("success"));
        assert_eq!(status_word_to_conclusion("weird"), None);
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
            )
            .route(
                "/repos/o/missing/issues",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
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

        let result = sync_once(&client, &db, &config, &status, &config.workspaces[0], true).await;
        assert!(result.is_ok(), "sync failed: {:?}", result.err());

        assert!(db.count("repos").expect("count") >= 1);
        assert_eq!(db.count("issues").expect("count"), 2);
        // Auto-dismiss removes the merged PR thread; the issue thread stays.
        assert_eq!(db.count("threads").expect("count"), 1);
        assert_eq!(db.unread_thread_count().expect("unread"), 1);
    }

    #[tokio::test]
    async fn sync_repo_subscriptions_caches_states() {
        use axum::response::IntoResponse;
        use axum::{routing::get, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        // o/r is in the watched list; o/ignored is explicitly ignored; o/plain
        // has no subscription (404 => participating).
        let ignored_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/notifications",
                get(|| async { ([("ETag", "\"n1\"")], r#"[]"#).into_response() }),
            )
            .route(
                "/user/subscriptions",
                get({
                    let seen = std::sync::Arc::new(Mutex::new(false));
                    move || {
                        let seen = seen.clone();
                        async move {
                            if *seen.lock().expect("seen") {
                                return axum::http::StatusCode::NOT_MODIFIED.into_response();
                            }
                            *seen.lock().expect("seen") = true;
                            (
                                [("ETag", "\"w1\"")],
                                r#"[{"full_name":"o/r","html_url":"https://github.com/o/r"}]"#,
                            )
                                .into_response()
                        }
                    }
                }),
            )
            .route(
                "/repos/o/ignored/subscription",
                get({
                    let calls = ignored_calls.clone();
                    move |req: axum::http::Request<axum::body::Body>| {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            let inm = req
                                .headers()
                                .get("if-none-match")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string);
                            if inm.as_deref() == Some("\"ig1\"") {
                                axum::http::StatusCode::NOT_MODIFIED.into_response()
                            } else {
                                (
                                    [("ETag", "\"ig1\"")],
                                    r#"{"subscribed":false,"ignored":true}"#,
                                )
                                    .into_response()
                            }
                        }
                    }
                }),
            )
            .route(
                "/repos/o/plain/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND.into_response() }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

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
                auto_dismiss_closed_merged: false,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["o/r".into(), "o/ignored".into(), "o/plain".into()],
                }],
            }],
        };
        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        sync_once(&client, &db, &config, &status, &config.workspaces[0], true)
            .await
            .expect("first sync");

        let states: Vec<(String, String)> = db
            .list_repos(&crate::db::RepoFilter {
                workspace_repos: &["o/r".into(), "o/ignored".into(), "o/plain".into()],
                show: "all",
                search: None,
            })
            .expect("list")
            .into_iter()
            .map(|i| (i.full_name, i.subscription_state.unwrap_or_default()))
            .collect();
        assert!(states.iter().any(|(n, s)| n == "o/r" && s == "watched"));
        assert!(states
            .iter()
            .any(|(n, s)| n == "o/ignored" && s == "ignored"));
        assert!(states
            .iter()
            .any(|(n, s)| n == "o/plain" && s == "participating"));
        // The watched list ETag and the ignored repo's subscription ETag are cached.
        assert_eq!(
            db.get_sync_state("etag:subscriptions")
                .expect("etag")
                .as_deref(),
            Some("\"w1\"")
        );
        assert_eq!(
            db.repo_subscription_etag("o/ignored")
                .expect("etag")
                .as_deref(),
            Some("\"ig1\"")
        );

        // Second pass: /user/subscriptions is unchanged (304) so watched flags
        // are kept, and o/ignored is unchanged (304) so its state is kept.
        sync_once(&client, &db, &config, &status, &config.workspaces[0], true)
            .await
            .expect("second sync");
        assert!(db.is_repo_watched("o/r").expect("watched"));
        assert_eq!(
            db.repo_subscription_etag("o/ignored")
                .expect("etag")
                .as_deref(),
            Some("\"ig1\"")
        );
        assert_eq!(
            ignored_calls.load(Ordering::SeqCst),
            2,
            "one full fetch + one 304"
        );
    }

    #[tokio::test]
    async fn sync_repos_skips_missing_repos() {
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
                auto_dismiss_closed_merged: false,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["o/r".into(), "o/missing".into()],
                }],
            }],
        };

        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        // A missing repo (404) must not abort the sync; o/r is still cached.
        let result = sync_once(&client, &db, &config, &status, &config.workspaces[0], true).await;
        assert!(result.is_ok(), "sync failed: {:?}", result.err());
        assert_eq!(db.count("issues").expect("count"), 2);
    }

    #[tokio::test]
    async fn sync_fetches_all_tracked_repos() {
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let app = Router::new()
            .route(
                "/notifications",
                get(|| async { ([("ETag", "\"n1\"")], r#"[]"#) }),
            )
            .route("/user/subscriptions", get(|| async { r#"[]"# }))
            .route("/repos/a/r1/issues", get(|| async { issue("t1", 1) }))
            .route("/repos/a/r2/issues", get(|| async { issue("t2", 2) }))
            .route("/repos/a/r3/issues", get(|| async { issue("t3", 3) }))
            .route(
                "/repos/a/r1/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route(
                "/repos/a/r2/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route(
                "/repos/a/r3/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        let config = Config {
            github: crate::config::GithubConfig {
                auth_provider: crate::config::AuthProvider::Pat,
                auth_token: "ghp_x".into(),
                poll_interval_seconds: 60,
                repo_refresh_interval_seconds: 60,
                sync_concurrency: 10,
                ..Default::default()
            },
            workspaces: vec![crate::config::Workspace {
                name: "test".into(),
                auto_dismiss_closed_merged: false,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["a/r1".into(), "a/r2".into(), "a/r3".into()],
                }],
            }],
        };
        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        let result = sync_once(&client, &db, &config, &status, &config.workspaces[0], true).await;
        assert!(result.is_ok(), "sync failed: {:?}", result.err());
        // All three repos were fetched and cached (not just the first).
        assert_eq!(db.count("issues").expect("issues"), 3);
        for repo in ["a/r1", "a/r2", "a/r3"] {
            assert!(
                !db.is_repo_watched(repo).expect("watched"),
                "{repo} not watched"
            );
        }
    }

    /// JSON for one open issue fixture.
    fn issue(title: &str, id: u64) -> String {
        format!(
            r#"[{{"id":{id},"number":1,"title":"{title}","state":"open","user":{{"login":"u"}},
                 "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
                 "closed_at":null,"html_url":"https://github.com/a/r{id}/issues/1",
                 "url":"https://api.github.com/repos/a/r{id}/issues/1"}}]"#
        )
    }

    #[tokio::test]
    async fn sync_once_scopes_repos_to_workspace() {
        use axum::routing::get;
        use axum::Router;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let b_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/notifications",
                get(|| async { ([("ETag", "\"n1\"")], r#"[]"#) }),
            )
            .route("/user/subscriptions", get(|| async { r#"[]"# }))
            .route("/repos/a/r1/issues", get(|| async { issue("t1", 1) }))
            .route(
                "/repos/a/r1/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route(
                "/repos/b/r1/issues",
                get({
                    let calls = b_calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            r#"[]"#
                        }
                    }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

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
            workspaces: vec![
                crate::config::Workspace {
                    name: "A".into(),
                    auto_dismiss_closed_merged: false,
                    repo_sets: vec![crate::config::RepoSet {
                        name: "s".into(),
                        repos: vec!["a/r1".into()],
                    }],
                },
                crate::config::Workspace {
                    name: "B".into(),
                    auto_dismiss_closed_merged: false,
                    repo_sets: vec![crate::config::RepoSet {
                        name: "s".into(),
                        repos: vec!["b/r1".into()],
                    }],
                },
            ],
        };
        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));

        // Only workspace A's repos are fetched; B's are left alone.
        sync_once(&client, &db, &config, &status, &config.workspaces[0], true)
            .await
            .expect("sync");
        assert_eq!(db.count("issues").expect("issues"), 1);
        assert_eq!(
            b_calls.load(Ordering::SeqCst),
            0,
            "workspace B repo not fetched"
        );
    }

    #[tokio::test]
    async fn refresh_subject_states_caches_pr_and_check_status() {
        use crate::models::{NotificationThread, ThreadRepository, ThreadSubject};
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let app = Router::new()
            .route(
                "/repos/o/r/pulls/7",
                get(|| async {
                    (
                        [("ETag", "\"p1\"")],
                        r#"{"state":"closed","merged_at":"2026-02-01T00:00:00Z","html_url":"https://github.com/o/r/pull/7"}"#,
                    )
                }),
            )
            .route(
                "/repos/o/r/check-runs/55",
                get(|| async {
                    (
                        [("ETag", "\"c1\"")],
                        r#"{"status":"completed","conclusion":"success","html_url":"https://github.com/o/r/actions/runs/55"}"#,
                    )
                }),
            )
            .route(
                "/repos/o/r/issues/3",
                get(|| async {
                    (
                        [("ETag", "\"i1\"")],
                        r#"{"id":3,"number":3,"title":"an issue","state":"open","user":{"login":"a"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","closed_at":null,"html_url":"https://github.com/o/r/issues/3","url":"https://api.github.com/repos/o/r/issues/3"}"#,
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");

        let thread = |id: &str, kind: &str, url: Option<String>, check_url: Option<String>| {
            NotificationThread {
                id: id.into(),
                unread: true,
                reason: "mention".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                last_read_at: None,
                subject: ThreadSubject {
                    title: "subject".into(),
                    kind: kind.into(),
                    url,
                    latest_comment_url: check_url,
                },
                repository: Some(ThreadRepository {
                    full_name: "o/r".into(),
                    html_url: "https://github.com/o/r".into(),
                }),
                url: format!("https://api.github.com/notifications/threads/{id}"),
            }
        };
        db.upsert_thread(&thread(
            "1:pr",
            "PullRequest",
            Some(format!("{base}/repos/o/r/pulls/7")),
            None,
        ))
        .expect("pr thread");
        db.upsert_thread(&thread(
            "2:ci",
            "CheckSuite",
            None,
            Some(format!("{base}/repos/o/r/check-runs/55")),
        ))
        .expect("check thread");
        db.upsert_thread(&thread(
            "3:issue",
            "Issue",
            Some(format!("{base}/repos/o/r/issues/3")),
            None,
        ))
        .expect("issue thread");

        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let config = Config {
            github: crate::config::GithubConfig {
                auth_provider: crate::config::AuthProvider::Pat,
                auth_token: "ghp_x".into(),
                ..Default::default()
            },
            workspaces: vec![crate::config::Workspace {
                name: "test".into(),
                auto_dismiss_closed_merged: false,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["o/r".into()],
                }],
            }],
        };

        refresh_subject_states(&client, &db, &status, &config.workspaces[0], &config)
            .await
            .expect("refresh");

        let state = |thread_id: &str| {
            db.with_conn(|c| {
                c.query_row(
                    "SELECT subject_state, subject_state_html_url FROM threads WHERE thread_id=?1",
                    rusqlite::params![thread_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .expect("row")
            })
        };
        assert_eq!(state("1:pr").0, "merged");
        assert_eq!(state("1:pr").1, "https://github.com/o/r/pull/7");
        assert_eq!(state("2:ci").0, "success");
        assert_eq!(state("2:ci").1, "https://github.com/o/r/actions/runs/55");
        assert_eq!(state("3:issue").0, "open");
        assert_eq!(state("3:issue").1, "https://github.com/o/r/issues/3");

        // A second pass re-verifies and keeps the states.
        refresh_subject_states(&client, &db, &status, &config.workspaces[0], &config)
            .await
            .expect("refresh");
        assert_eq!(state("1:pr").0, "merged");
        assert_eq!(state("2:ci").0, "success");
    }

    #[tokio::test]
    async fn refresh_subject_states_resolves_check_run_from_title() {
        use crate::models::{NotificationThread, ThreadRepository, ThreadSubject};
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let app = Router::new().route(
            "/repos/o/r/actions/runs",
            get(|| async {
                (
                    [("ETag", "\"r1\"")],
                    r#"{"workflow_runs":[
                        {"name":"Renovate","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/123","created_at":"2026-09-07T11:56:48Z"},
                        {"name":"CI","conclusion":"success","html_url":"https://github.com/o/r/actions/runs/1","created_at":"2026-09-07T01:31:49Z"}
                    ]}"#,
                )
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        let thread = NotificationThread {
            id: "1:check".into(),
            unread: true,
            reason: "ci_activity".into(),
            updated_at: "2026-09-07T11:57:15Z".into(),
            last_read_at: None,
            subject: ThreadSubject {
                title: "Renovate workflow run failed for main branch".into(),
                kind: "CheckSuite".into(),
                url: None,
                latest_comment_url: None,
            },
            repository: Some(ThreadRepository {
                full_name: "o/r".into(),
                html_url: "https://github.com/o/r".into(),
            }),
            url: "https://api.github.com/notifications/threads/check".into(),
        };
        db.upsert_thread(&thread).expect("thread");

        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );
        let status = Arc::new(Mutex::new(SyncStatus::default()));
        let config = Config {
            github: crate::config::GithubConfig {
                auth_provider: crate::config::AuthProvider::Pat,
                auth_token: "ghp_x".into(),
                ..Default::default()
            },
            workspaces: vec![crate::config::Workspace {
                name: "test".into(),
                auto_dismiss_closed_merged: false,
                repo_sets: vec![crate::config::RepoSet {
                    name: "set".into(),
                    repos: vec!["o/r".into()],
                }],
            }],
        };

        refresh_subject_states(&client, &db, &status, &config.workspaces[0], &config)
            .await
            .expect("refresh");

        let (state, url) = db.with_conn(|c| {
            c.query_row(
                "SELECT subject_state, subject_state_html_url FROM threads WHERE thread_id='1:check'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .expect("row")
        });
        assert_eq!(state, "failure");
        assert_eq!(url, "https://github.com/o/r/actions/runs/123");
    }

    #[tokio::test]
    async fn dismiss_closed_merged_only_touches_workspace_repos() {
        use crate::models::{NotificationThread, ThreadRepository, ThreadSubject};
        use axum::routing::{get, patch};
        use axum::Router;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let work_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/repos/o/r/pulls/7",
                get(|| async {
                    r#"{"state":"closed","merged_at":"2026-02-01T00:00:00Z","html_url":"https://github.com/o/r/pull/7"}"#
                }),
            )
            .route(
                "/repos/work/r/pulls/99",
                get({
                    let calls = work_calls.clone();
                    move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            r#"{"state":"closed","merged_at":"2026-02-01T00:00:00Z","html_url":"https://github.com/work/r/pull/99"}"#
                        }
                    }
                }),
            )
            .route(
                "/notifications/threads/1",
                patch(|| async { axum::http::StatusCode::RESET_CONTENT }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        let thread = |id: &str, num: u32, repo: &str, url: &str| NotificationThread {
            id: id.into(),
            unread: true,
            reason: "mention".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_read_at: None,
            subject: ThreadSubject {
                title: "pr".into(),
                kind: "PullRequest".into(),
                url: Some(format!("{base}{url}")),
                latest_comment_url: None,
            },
            repository: Some(ThreadRepository {
                full_name: repo.into(),
                html_url: format!("https://github.com/{repo}"),
            }),
            url: format!("https://api.github.com/notifications/threads/{num}"),
        };
        // A merged PR on the workspace's repo, and one on a repo outside it.
        db.upsert_thread(&thread("1:in", 1, "o/r", "/repos/o/r/pulls/7"))
            .expect("in");
        db.upsert_thread(&thread("2:work", 2, "work/r", "/repos/work/r/pulls/99"))
            .expect("work");

        let ws = crate::config::Workspace {
            name: "test".into(),
            auto_dismiss_closed_merged: false,
            repo_sets: vec![crate::config::RepoSet {
                name: "set".into(),
                repos: vec!["o/r".into()],
            }],
        };
        let client = Client::with_base(
            Arc::new(crate::auth::pat::ClassicPat::new("ghp_x".into())),
            &base,
        );

        let count = dismiss_closed_merged(&client, &db, &ws)
            .await
            .expect("dismiss");
        assert_eq!(count, 1, "the workspace repo's merged PR is dismissed");
        assert_eq!(
            work_calls.load(Ordering::SeqCst),
            0,
            "the out-of-workspace PR must not be fetched"
        );
    }

    #[tokio::test]
    async fn sync_notifications_skips_dismissed_subjects() {
        use axum::routing::get;
        use axum::Router;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));

        let payload = format!(
            r#"[{{"id":"1:pr","unread":true,"reason":"mention",
                 "updated_at":"2026-01-01T00:00:00Z","last_read_at":null,
                 "subject":{{"title":"pr","type":"PullRequest","url":"{base}/repos/o/r/pulls/7","latest_comment_url":null}},
                 "repository":{{"full_name":"o/r","html_url":"https://github.com/o/r"}},
                 "url":"{base}/notifications/threads/7"}}]"#
        );
        let app = Router::new()
            .route(
                "/notifications",
                get(move || async move { ([("ETag", "\"n1\"")], payload.clone()) }),
            )
            .route("/user/subscriptions", get(|| async { r#"[]"# }))
            .route(
                "/repos/o/r/issues",
                get(|| async { ([("ETag", "\"i1\"")], r#"[]"#) }),
            )
            .route(
                "/repos/o/r/subscription",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        // The subject was dismissed; the notification sync must not re-add it.
        db.record_dismissed_subjects(&[format!("{base}/repos/o/r/pulls/7")])
            .expect("record");

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
                auto_dismiss_closed_merged: false,
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

        sync_once(&client, &db, &config, &status, &config.workspaces[0], true)
            .await
            .expect("sync");
        assert_eq!(db.count("threads").expect("threads"), 0);
    }
}
