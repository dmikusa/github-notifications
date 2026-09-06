use std::sync::LazyLock;

use anyhow::{Context, Result};
use askama::Template;

use crate::config::{Config, RepoSet, Workspace};
use crate::db::{self, Database};

/// The workspace shown when the config has no workspaces yet.
static EMPTY_WORKSPACE: LazyLock<Workspace> = LazyLock::new(Workspace::default);

/// Resolve the workspace named `name`, falling back to the first configured
/// workspace, or a static empty workspace when none exist.
pub fn resolve_workspace<'a>(config: &'a Config, name: &str) -> &'a Workspace {
    if let Some(ws) = config.workspaces.iter().find(|w| w.name == name) {
        return ws;
    }
    if let Some(ws) = config.workspaces.first() {
        return ws;
    }
    &EMPTY_WORKSPACE
}

/// Treat empty or "all" as "no filter".
fn filter_value(value: &str) -> Option<&str> {
    if value.is_empty() || value == "all" {
        None
    } else {
        Some(value)
    }
}

/// Resolve which repos to query for `workspace` given a repo-set filter
/// ("all"/empty means every tracked repo).
fn repos_for_filter(workspace: &Workspace, repo_set: &str) -> Vec<String> {
    if repo_set.is_empty() || repo_set == "all" {
        workspace.tracked_repos()
    } else {
        workspace.repo_set_repos(repo_set).unwrap_or_default()
    }
}

/// The repo set names for a workspace's filter selector.
fn repo_set_names(workspace: &Workspace) -> Vec<String> {
    workspace.repo_sets.iter().map(|s| s.name.clone()).collect()
}

#[derive(Template)]
#[template(path = "queue.html")]
struct QueueTemplate<'a> {
    ws: &'a str,
    repo_set: &'a str,
    repo_sets: &'a [String],
    kind: &'a str,
    unread: bool,
    q: &'a str,
    sort: &'a str,
    items: &'a [db::QueueItem],
}

/// Render the queue view fragment for `workspace`. Rows only; the frontend
/// decides how to present the initial-loading state via `/api/sync/status`.
pub fn render_queue(db: &Database, workspace: &Workspace, q: &QueueParams) -> Result<String> {
    let repos = repos_for_filter(workspace, &q.repo_set);
    let filter = db::QueueFilter {
        repos: &repos,
        kind: if q.kind == "all" || q.kind.is_empty() {
            None
        } else {
            Some(q.kind.as_str())
        },
        unread_only: q.unread,
        search: Some(q.q.as_str()),
        sort: q.sort.as_str(),
    };
    let items = db.list_queue(&filter)?;
    let repo_sets = repo_set_names(workspace);
    let template = QueueTemplate {
        ws: &workspace.name,
        repo_set: if q.repo_set.is_empty() {
            "all"
        } else {
            &q.repo_set
        },
        repo_sets: &repo_sets,
        kind: &q.kind,
        unread: q.unread,
        q: &q.q,
        sort: &q.sort,
        items: &items,
    };
    template.render().context("rendering queue view")
}

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxTemplate<'a> {
    ws: &'a str,
    repo_set: &'a str,
    repo_sets: &'a [String],
    subject_type: &'a str,
    reason: &'a str,
    unread: bool,
    sort: &'a str,
    items: &'a [db::InboxItem],
}

/// Render the inbox view fragment for `workspace`.
pub fn render_inbox(db: &Database, workspace: &Workspace, q: &InboxParams) -> Result<String> {
    let repos = repos_for_filter(workspace, &q.repo_set);
    let filter = db::InboxFilter {
        repos: &repos,
        subject_type: filter_value(&q.subject_type),
        reason: filter_value(&q.reason),
        unread_only: q.unread,
        sort: q.sort.as_str(),
    };
    let items = db.list_inbox(&filter)?;
    let repo_sets = repo_set_names(workspace);
    let template = InboxTemplate {
        ws: &workspace.name,
        repo_set: if q.repo_set.is_empty() {
            "all"
        } else {
            &q.repo_set
        },
        repo_sets: &repo_sets,
        subject_type: &q.subject_type,
        reason: &q.reason,
        unread: q.unread,
        sort: &q.sort,
        items: &items,
    };
    template.render().context("rendering inbox view")
}

#[derive(Template)]
#[template(path = "repos.html")]
struct ReposTemplate<'a> {
    ws: &'a str,
    show: &'a str,
    q: &'a str,
    items: &'a [db::RepoItem],
}

/// Render the repos view fragment for `workspace`.
pub fn render_repos(db: &Database, workspace: &Workspace, q: &RepoParams) -> Result<String> {
    let filter = db::RepoFilter {
        workspace_repos: &workspace.tracked_repos(),
        show: &q.show,
        search: Some(q.q.as_str()),
    };
    let items = db.list_repos(&filter)?;
    let template = ReposTemplate {
        ws: &workspace.name,
        show: &q.show,
        q: &q.q,
        items: &items,
    };
    template.render().context("rendering repos view")
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate<'a> {
    ws: &'a str,
    repo_sets: &'a [RepoSet],
}

/// Render the settings view fragment (repo set management) for `workspace`.
pub fn render_settings(workspace: &Workspace) -> Result<String> {
    let template = SettingsTemplate {
        ws: &workspace.name,
        repo_sets: &workspace.repo_sets,
    };
    template.render().context("rendering settings view")
}

/// Query params for the queue view.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct QueueParams {
    pub ws: String,
    pub repo_set: String,
    pub kind: String,
    pub unread: bool,
    pub q: String,
    pub sort: String,
}

/// Query params for the inbox view.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct InboxParams {
    pub ws: String,
    pub repo_set: String,
    #[serde(rename = "type")]
    pub subject_type: String,
    pub reason: String,
    pub unread: bool,
    pub sort: String,
}

/// Query params for the repos view.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct RepoParams {
    pub ws: String,
    pub show: String,
    pub q: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::pat::ClassicPat;
    use crate::config::RepoSet;
    use crate::db::Database;
    use crate::models::{GithubIssue, NotificationThread, ThreadRepository, ThreadSubject};

    fn seed(db: &Database) {
        let repo_id = db
            .upsert_repo("o/r", Some("https://github.com/o/r"))
            .expect("repo");
        let issue: GithubIssue = serde_json::from_value(serde_json::json!({
            "id": 1, "number": 3, "title": "an open issue", "state": "open",
            "user": {"login": "a"},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
            "closed_at": null,
            "html_url": "https://github.com/o/r/issues/3",
            "url": "https://api.github.com/repos/o/r/issues/3"
        }))
        .expect("issue");
        db.upsert_issue(repo_id, &issue, "issue", None)
            .expect("issue row");

        let thread = NotificationThread {
            id: "1:111".into(),
            unread: true,
            reason: "mention".into(),
            updated_at: "2026-01-03T00:00:00Z".into(),
            last_read_at: None,
            subject: ThreadSubject {
                title: "an open issue".into(),
                kind: "Issue".into(),
                url: Some("https://api.github.com/repos/o/r/issues/3".into()),
                latest_comment_url: None,
            },
            repository: Some(ThreadRepository {
                full_name: "o/r".into(),
                html_url: "https://github.com/o/r".into(),
            }),
            url: "https://api.github.com/notifications/threads/111".into(),
        };
        db.upsert_thread(&thread).expect("thread row");
        let _ = ClassicPat::new("x".into());
    }

    #[test]
    fn queue_renders_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        seed(&db);
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["o/r".into()],
            }],
            ..Default::default()
        };
        let html = render_queue(
            &db,
            &ws,
            &QueueParams {
                ws: "w".into(),
                repo_set: "all".into(),
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
        )
        .expect("render");
        assert!(html.contains("an open issue"));
        assert!(html.contains("o/r"));
    }

    #[test]
    fn queue_filters_by_repo_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        // Seed two repos in two repo sets.
        for (full, n) in [("a/r", 1u64), ("b/r", 2)] {
            let repo_id = db
                .upsert_repo(full, Some(&format!("https://github.com/{full}")))
                .expect("repo");
            let issue: GithubIssue = serde_json::from_value(serde_json::json!({
                "id": n, "number": n, "title": format!("issue {n}"), "state": "open",
                "user": {"login": "a"},
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z",
                "closed_at": null,
                "html_url": format!("https://github.com/{full}/issues/{n}"),
                "url": format!("https://api.github.com/repos/{full}/issues/{n}")
            }))
            .expect("issue");
            db.upsert_issue(repo_id, &issue, "issue", None)
                .expect("row");
        }
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![
                RepoSet {
                    name: "set-a".into(),
                    repos: vec!["a/r".into()],
                },
                RepoSet {
                    name: "set-b".into(),
                    repos: vec!["b/r".into()],
                },
            ],
            ..Default::default()
        };
        let params = |repo_set: &str| QueueParams {
            ws: "w".into(),
            repo_set: repo_set.into(),
            kind: "all".into(),
            unread: false,
            q: String::new(),
            sort: "attention".into(),
        };
        // Only set-a's repo.
        let html = render_queue(&db, &ws, &params("set-a")).expect("render set-a");
        assert!(html.contains("issue 1"));
        assert!(!html.contains("issue 2"));
        // Only set-b's repo.
        let html = render_queue(&db, &ws, &params("set-b")).expect("render set-b");
        assert!(html.contains("issue 2"));
        assert!(!html.contains("issue 1"));
        // "all" shows both.
        let html = render_queue(&db, &ws, &params("all")).expect("render all");
        assert!(html.contains("issue 1") && html.contains("issue 2"));
    }

    #[test]
    fn queue_renders_empty_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["o/r".into()],
            }],
            ..Default::default()
        };
        let html = render_queue(
            &db,
            &ws,
            &QueueParams {
                ws: "w".into(),
                repo_set: "all".into(),
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
        )
        .expect("render");
        assert!(html.contains("No items match your filters"));
    }

    #[test]
    fn inbox_renders_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        seed(&db);
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["o/r".into()],
            }],
            ..Default::default()
        };
        let html = render_inbox(
            &db,
            &ws,
            &InboxParams {
                ws: "w".into(),
                repo_set: "all".into(),
                subject_type: "all".into(),
                reason: "all".into(),
                unread: false,
                sort: "updated".into(),
            },
        )
        .expect("render");
        assert!(html.contains("an open issue"));
        assert!(html.contains("mention"));
    }

    #[test]
    fn queue_filter_controls_are_htmx_wired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        seed(&db);
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["o/r".into()],
            }],
            ..Default::default()
        };
        let html = render_queue(
            &db,
            &ws,
            &QueueParams {
                ws: "w".into(),
                repo_set: "all".into(),
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
        )
        .expect("render");
        // Repo set, kind, unread checkbox, and sort all reload on `change`.
        assert_eq!(html.matches(r#"hx-trigger="change""#).count(), 4);
        assert!(html.contains(r#"hx-get="/api/views/queue""#));
        assert!(html.contains(r#"hx-include="closest form""#));
        // The unread checkbox must submit a value serde parses as `bool`; the
        // server's `QueueParams.unread: bool` rejects "1" with a 400.
        assert!(html.contains(r#"name="unread" value="true""#));
    }

    #[test]
    fn inbox_filter_controls_are_htmx_wired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        seed(&db);
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["o/r".into()],
            }],
            ..Default::default()
        };
        let html = render_inbox(
            &db,
            &ws,
            &InboxParams {
                ws: "w".into(),
                repo_set: "all".into(),
                subject_type: "all".into(),
                reason: "all".into(),
                unread: false,
                sort: "updated".into(),
            },
        )
        .expect("render");
        // Repo set, type, reason, unread checkbox, and sort all reload on `change`.
        assert_eq!(html.matches(r#"hx-trigger="change""#).count(), 5);
        assert!(html.contains(r#"hx-get="/api/views/inbox""#));
        assert!(html.contains(r#"hx-include="closest form""#));
        assert!(html.contains(r#"name="unread" value="true""#));
    }

    #[test]
    fn settings_empty_state_renders_em_dash() {
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![],
            ..Default::default()
        };
        let html = render_settings(&ws).expect("render");
        // Regression: a literal `\u2014` in the template was rendered as text.
        assert!(!html.contains(r"\u2014"));
        assert!(html.contains("&mdash;"));
    }

    #[test]
    fn repos_renders_subscription_badges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("db");
        for (full, watched, state) in [
            ("a/w", true, Some("watched")),
            ("a/i", false, Some("ignored")),
            ("a/p", false, Some("participating")),
        ] {
            db.upsert_repo(full, Some(&format!("https://github.com/{full}")))
                .expect("repo");
            db.set_repo_watched(full, watched).expect("watched");
            if let Some(s) = state {
                db.set_repo_subscription_state(full, s).expect("state");
            }
        }
        let ws = Workspace {
            name: "w".into(),
            repo_sets: vec![RepoSet {
                name: "s".into(),
                repos: vec!["a/w".into(), "a/i".into(), "a/p".into()],
            }],
            ..Default::default()
        };
        let html = render_repos(
            &db,
            &ws,
            &RepoParams {
                ws: "w".into(),
                show: "all".into(),
                q: String::new(),
            },
        )
        .expect("render");
        assert!(html.contains("class=\"badge ok\">watched"));
        assert!(html.contains("class=\"badge ignored\">ignored"));
        assert!(html.contains("class=\"badge\">participating"));
        // The ignored filter option is present.
        assert!(html.contains("value=\"ignored\""));
    }
}
