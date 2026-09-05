use std::sync::LazyLock;

use anyhow::{Context, Result};
use askama::Template;

use crate::config::{Config, Workspace};
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

#[derive(Template)]
#[template(path = "queue.html")]
struct QueueTemplate<'a> {
    ws: &'a str,
    kind: &'a str,
    unread: bool,
    q: &'a str,
    sort: &'a str,
    items: &'a [db::QueueItem],
    synced: bool,
}

/// Render the queue view fragment for `workspace`. `synced` reports whether the
/// cache has been populated by at least one completed sync.
pub fn render_queue(
    db: &Database,
    workspace: &Workspace,
    q: &QueueParams,
    synced: bool,
) -> Result<String> {
    let repos = workspace.tracked_repos();
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
    let template = QueueTemplate {
        ws: &workspace.name,
        kind: &q.kind,
        unread: q.unread,
        q: &q.q,
        sort: &q.sort,
        items: &items,
        synced,
    };
    template.render().context("rendering queue view")
}

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxTemplate<'a> {
    ws: &'a str,
    subject_type: &'a str,
    reason: &'a str,
    unread: bool,
    sort: &'a str,
    items: &'a [db::InboxItem],
    synced: bool,
}

/// Render the inbox view fragment for `workspace`.
pub fn render_inbox(
    db: &Database,
    workspace: &Workspace,
    q: &InboxParams,
    synced: bool,
) -> Result<String> {
    let repos = workspace.tracked_repos();
    let filter = db::InboxFilter {
        repos: &repos,
        subject_type: filter_value(&q.subject_type),
        reason: filter_value(&q.reason),
        unread_only: q.unread,
        sort: q.sort.as_str(),
    };
    let items = db.list_inbox(&filter)?;
    let template = InboxTemplate {
        ws: &workspace.name,
        subject_type: &q.subject_type,
        reason: &q.reason,
        unread: q.unread,
        sort: &q.sort,
        items: &items,
        synced,
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
    synced: bool,
}

/// Render the repos view fragment for `workspace`.
pub fn render_repos(
    db: &Database,
    workspace: &Workspace,
    q: &RepoParams,
    synced: bool,
) -> Result<String> {
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
        synced,
    };
    template.render().context("rendering repos view")
}

/// Query params for the queue view.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct QueueParams {
    pub ws: String,
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
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
            true,
        )
        .expect("render");
        assert!(html.contains("an open issue"));
        assert!(html.contains("o/r"));
    }

    #[test]
    fn queue_renders_not_populated_empty_state() {
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
        // A fresh DB with no last_sync should say "no data yet".
        let html = render_queue(
            &db,
            &ws,
            &QueueParams {
                ws: "w".into(),
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
            false,
        )
        .expect("render");
        assert!(html.contains("No data yet"));

        // Once synced, an empty result is a normal filtered-empty message.
        let html = render_queue(
            &db,
            &ws,
            &QueueParams {
                ws: "w".into(),
                kind: "all".into(),
                unread: false,
                q: String::new(),
                sort: "attention".into(),
            },
            true,
        )
        .expect("render");
        assert!(html.contains("No items match your filters"));
        assert!(!html.contains("No data yet"));
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
                subject_type: "all".into(),
                reason: "all".into(),
                unread: false,
                sort: "updated".into(),
            },
            true,
        )
        .expect("render");
        assert!(html.contains("an open issue"));
        assert!(html.contains("mention"));
    }
}
