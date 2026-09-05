use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// Resolve the directory holding local data files, honoring
/// `GHNOTIFY_DATA_DIR`, then XDG, then the conventional macOS location.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("GHNOTIFY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("github-notifications");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/github-notifications");
    }
    PathBuf::from("data")
}

/// SQLite database wrapping the local cache of GitHub data.
///
/// The connection is guarded by a mutex so the shared handle can be used from
/// the API handlers and the background sync loop alike. This is fine for a
/// single-user local app; the SQLite cache layer keeps statements short-lived.
pub struct Database {
    conn: Mutex<Connection>,
}

/// One row of the queue (an open issue or PR joined to its latest thread).
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: i64,
    pub repo: String,
    pub kind: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub updated_at: String,
    pub created_at: String,
    pub html_url: String,
    pub thread_unread: bool,
    pub thread_reason: Option<String>,
    pub thread_updated: Option<String>,
}

/// Filters for the queue query.
#[derive(Debug, Default)]
pub struct QueueFilter<'a> {
    pub repos: &'a [String],
    pub kind: Option<&'a str>,
    pub unread_only: bool,
    pub search: Option<&'a str>,
    /// "attention" | "updated" | "created" | "repo"
    pub sort: &'a str,
}

/// One row of the inbox (a notification thread).
#[derive(Debug, Clone)]
pub struct InboxItem {
    pub thread_id: String,
    pub repo: String,
    pub subject_type: String,
    pub subject_title: String,
    pub reason: String,
    pub unread: bool,
    pub updated_at: String,
    pub subject_api_url: Option<String>,
    pub subject_html_url: Option<String>,
}

/// Filters for the inbox query.
#[derive(Debug, Default)]
pub struct InboxFilter<'a> {
    pub repos: &'a [String],
    pub subject_type: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub unread_only: bool,
    pub sort: &'a str,
}

/// One row of the repos view.
#[derive(Debug, Clone)]
pub struct RepoItem {
    pub full_name: String,
    pub html_url: String,
    pub is_watched: bool,
    pub in_workspace: bool,
    pub last_refreshed_at: Option<String>,
}

/// Filters for the repos query.
#[derive(Debug, Default)]
pub struct RepoFilter<'a> {
    /// Repos tracked by the current workspace (determines `in_workspace`).
    pub workspace_repos: &'a [String],
    /// "all" | "watched" | "untracked" (show mode)
    pub show: &'a str,
    pub search: Option<&'a str>,
}

impl Database {
    /// Open (creating if needed) the database at `path` and initialize the schema.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enabling WAL mode")?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .context("enabling foreign keys")?;
        let db = Database {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS repos (
    id               INTEGER PRIMARY KEY,
    full_name        TEXT NOT NULL UNIQUE,
    owner            TEXT NOT NULL,
    name             TEXT NOT NULL,
    html_url         TEXT,
    is_watched       INTEGER NOT NULL DEFAULT 0,
    last_refreshed_at TEXT
);

CREATE TABLE IF NOT EXISTS issues (
    id         INTEGER PRIMARY KEY,
    repo_id    INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    github_id  INTEGER NOT NULL,
    number     INTEGER NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('issue', 'pr')),
    title      TEXT NOT NULL,
    state      TEXT NOT NULL,
    author     TEXT,
    created_at TEXT,
    updated_at TEXT,
    closed_at  TEXT,
    merged_at  TEXT,
    html_url   TEXT,
    api_url    TEXT NOT NULL UNIQUE,
    UNIQUE (repo_id, kind, number)
);

CREATE TABLE IF NOT EXISTS threads (
    id            INTEGER PRIMARY KEY,
    thread_id     TEXT NOT NULL UNIQUE,
    repo_id       INTEGER REFERENCES repos(id) ON DELETE CASCADE,
    subject_type  TEXT,
    subject_title TEXT,
    subject_url   TEXT,
    subject_api_url TEXT,
    reason        TEXT,
    unread        INTEGER NOT NULL DEFAULT 1,
    updated_at    TEXT,
    last_read_at  TEXT,
    api_url       TEXT
);

CREATE TABLE IF NOT EXISTS sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT
);
"#,
        )
        .context("initializing database schema")?;
        Ok(())
    }

    /// Return the stored sync state value for `key`, if any.
    pub fn get_sync_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut stmt = conn
            .prepare("SELECT value FROM sync_state WHERE key = ?1")
            .context("preparing sync_state select")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row.get(0)?));
        }
        Ok(None)
    }

    /// Set the stored sync state value for `key`.
    pub fn set_sync_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .context("upserting sync_state")?;
        Ok(())
    }

    /// Upsert a repository by full name, returning its row id.
    pub fn upsert_repo(&self, full_name: &str, html_url: Option<&str>) -> Result<i64> {
        let (owner, name) = split_repo(full_name);
        let conn = self.conn.lock().expect("db lock poisoned");
        let id = conn
            .query_row(
                "INSERT INTO repos (full_name, owner, name, html_url)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (full_name) DO UPDATE SET
                   html_url = COALESCE(repos.html_url, excluded.html_url)
                 RETURNING id",
                params![full_name, owner, name, html_url.unwrap_or_default()],
                |row| row.get(0),
            )
            .context("upserting repo")?;
        Ok(id)
    }

    /// Upsert an issue or pull request belonging to `repo_id`.
    pub fn upsert_issue(
        &self,
        repo_id: i64,
        issue: &crate::models::GithubIssue,
        kind: &str,
        merged_at: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "INSERT INTO issues
               (repo_id, github_id, number, kind, title, state, author,
                created_at, updated_at, closed_at, merged_at, html_url, api_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (repo_id, kind, number) DO UPDATE SET
               github_id = excluded.github_id,
               title = excluded.title,
               state = excluded.state,
               author = excluded.author,
               updated_at = excluded.updated_at,
               closed_at = excluded.closed_at,
               merged_at = excluded.merged_at,
               html_url = excluded.html_url,
               api_url = excluded.api_url,
               created_at = COALESCE(issues.created_at, excluded.created_at)",
            params![
                repo_id,
                issue.id,
                issue.number,
                kind,
                issue.title,
                issue.state,
                issue.user.as_ref().map(|u| u.login.as_str()),
                issue.created_at,
                issue.updated_at,
                issue.closed_at,
                merged_at,
                issue.html_url,
                issue.url,
            ],
        )
        .context("upserting issue")?;
        Ok(())
    }

    /// Upsert a notification thread, resolving (and creating if needed) its
    /// repository row. Threads without a repository are skipped.
    pub fn upsert_thread(&self, thread: &crate::models::NotificationThread) -> Result<()> {
        let Some(repo) = &thread.repository else {
            return Ok(());
        };
        let repo_id = self.upsert_repo(&repo.full_name, Some(&repo.html_url))?;
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "INSERT INTO threads
               (thread_id, repo_id, subject_type, subject_title, subject_url,
                subject_api_url, reason, unread, updated_at, last_read_at, api_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (thread_id) DO UPDATE SET
               repo_id = excluded.repo_id,
               subject_type = excluded.subject_type,
               subject_title = excluded.subject_title,
               subject_url = excluded.subject_url,
               subject_api_url = excluded.subject_api_url,
               reason = excluded.reason,
               unread = excluded.unread,
               updated_at = excluded.updated_at,
               last_read_at = excluded.last_read_at",
            params![
                thread.id,
                repo_id,
                thread.subject.kind,
                thread.subject.title,
                thread.subject.url,
                thread.subject.url,
                thread.reason,
                thread.unread,
                thread.updated_at,
                thread.last_read_at,
                thread.url,
            ],
        )
        .context("upserting thread")?;
        Ok(())
    }

    /// Set a thread's unread flag locally (after marking read on GitHub).
    pub fn set_thread_unread(&self, thread_id: &str, unread: bool) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "UPDATE threads SET unread = ?2 WHERE thread_id = ?1",
            params![thread_id, unread],
        )
        .context("updating thread unread")?;
        Ok(())
    }

    /// Notification id, thread API url, and subject API url for unread
    /// pull-request threads, used by the auto-dismiss pass.
    pub fn get_unread_pr_threads(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT thread_id, api_url, subject_api_url FROM threads
                 WHERE unread = 1 AND subject_type = 'PullRequest'
                   AND api_url IS NOT NULL AND subject_api_url IS NOT NULL",
            )
            .context("preparing unread PR threads")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("querying unread PR threads")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("reading unread PR thread row")?);
        }
        Ok(out)
    }

    /// Mark every repo as not watched (start of a watch sync pass).
    pub fn clear_watched(&self) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute("UPDATE repos SET is_watched = 0", [])
            .context("clearing watched flags")?;
        Ok(())
    }

    /// Upsert a watched repository and flag it as watched.
    pub fn upsert_watched_repo(&self, full_name: &str, html_url: &str) -> Result<()> {
        let id = self.upsert_repo(full_name, Some(html_url))?;
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute("UPDATE repos SET is_watched = 1 WHERE id = ?1", params![id])
            .context("marking repo watched")?;
        Ok(())
    }

    /// Record the last refresh time for a repository.
    pub fn set_repo_refreshed(&self, full_name: &str, at: &str) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "UPDATE repos SET last_refreshed_at = ?2 WHERE full_name = ?1",
            params![full_name, at],
        )
        .context("recording repo refresh")?;
        Ok(())
    }

    /// Count rows in a table (used by tests and status reporting).
    pub fn count(&self, table: &str) -> Result<i64> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .with_context(|| format!("counting {table}"))
    }

    /// Number of unread threads currently cached.
    pub fn unread_thread_count(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.query_row("SELECT COUNT(*) FROM threads WHERE unread = 1", [], |row| {
            row.get(0)
        })
        .context("counting unread threads")
    }

    /// List open issues/PRs for the given repos, joined to their latest thread.
    pub fn list_queue(&self, f: &QueueFilter) -> Result<Vec<QueueItem>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut sql = String::from(
            "SELECT i.id, r.full_name, i.kind, i.number, i.title, i.state,
                    i.updated_at, i.created_at, i.html_url,
                    COALESCE((SELECT t.unread FROM threads t
                              WHERE t.repo_id = i.repo_id AND t.subject_api_url = i.api_url
                              ORDER BY t.updated_at DESC LIMIT 1), 0) AS thread_unread,
                    (SELECT t.reason FROM threads t
                     WHERE t.repo_id = i.repo_id AND t.subject_api_url = i.api_url
                     ORDER BY t.updated_at DESC LIMIT 1) AS thread_reason,
                    (SELECT t.updated_at FROM threads t
                     WHERE t.repo_id = i.repo_id AND t.subject_api_url = i.api_url
                     ORDER BY t.updated_at DESC LIMIT 1) AS thread_updated
             FROM issues i JOIN repos r ON r.id = i.repo_id
             WHERE i.state = 'open'",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let repo_clause = in_clause(f.repos.len());
        sql.push_str(&format!(" AND r.full_name IN {repo_clause}"));
        for repo in f.repos {
            params.push(Box::new(repo.clone()));
        }
        if let Some(kind) = f.kind {
            sql.push_str(" AND i.kind = ?");
            params.push(Box::new(kind.to_string()));
        }
        if f.unread_only {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM threads t
                             WHERE t.repo_id = i.repo_id AND t.subject_api_url = i.api_url
                               AND t.unread = 1)",
            );
        }
        if let Some(search) = f.search.filter(|s| !s.is_empty()) {
            sql.push_str(" AND i.title LIKE ?");
            params.push(Box::new(format!("%{search}%")));
        }
        let order = match f.sort {
            "updated" => "ORDER BY i.updated_at DESC",
            "created" => "ORDER BY i.created_at DESC",
            "repo" => "ORDER BY r.full_name, i.number DESC",
            _ => "ORDER BY COALESCE(thread_updated, i.updated_at) DESC",
        };
        sql.push_str(order);

        let mut stmt = conn.prepare(&sql).context("preparing queue query")?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok(QueueItem {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    kind: row.get(2)?,
                    number: row.get(3)?,
                    title: row.get(4)?,
                    state: row.get(5)?,
                    updated_at: row.get(6)?,
                    created_at: row.get(7)?,
                    html_url: row.get(8)?,
                    thread_unread: row.get::<_, i64>(9)? != 0,
                    thread_reason: row.get(10)?,
                    thread_updated: row.get(11)?,
                })
            })
            .context("querying queue")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading queue")
    }

    /// List notification threads for the given repos.
    pub fn list_inbox(&self, f: &InboxFilter) -> Result<Vec<InboxItem>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut sql = String::from(
            "SELECT t.thread_id, r.full_name, t.subject_type, t.subject_title,
                    t.reason, t.unread, t.updated_at, t.subject_api_url
             FROM threads t JOIN repos r ON r.id = t.repo_id
             WHERE 1 = 1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let repo_clause = in_clause(f.repos.len());
        sql.push_str(&format!(" AND r.full_name IN {repo_clause}"));
        for repo in f.repos {
            params.push(Box::new(repo.clone()));
        }
        if f.unread_only {
            sql.push_str(" AND t.unread = 1");
        }
        if let Some(kind) = f.subject_type.filter(|s| *s != "all") {
            sql.push_str(" AND t.subject_type = ?");
            params.push(Box::new(kind.to_string()));
        }
        if let Some(reason) = f.reason.filter(|s| *s != "all") {
            sql.push_str(" AND t.reason = ?");
            params.push(Box::new(reason.to_string()));
        }
        let order = match f.sort {
            "repo" => "ORDER BY r.full_name, t.updated_at DESC",
            _ => "ORDER BY t.updated_at DESC",
        };
        sql.push_str(order);

        let mut stmt = conn.prepare(&sql).context("preparing inbox query")?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                let subject_api_url: Option<String> = row.get(7)?;
                Ok(InboxItem {
                    thread_id: row.get(0)?,
                    repo: row.get(1)?,
                    subject_type: row.get(2)?,
                    subject_title: row.get(3)?,
                    reason: row.get(4)?,
                    unread: row.get::<_, i64>(5)? != 0,
                    updated_at: row.get(6)?,
                    subject_html_url: subject_html_url(subject_api_url.as_deref()),
                    subject_api_url,
                })
            })
            .context("querying inbox")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading inbox")
    }

    /// List repos that are watched or tracked by the workspace.
    pub fn list_repos(&self, f: &RepoFilter) -> Result<Vec<RepoItem>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut sql = String::from(
            "SELECT r.full_name, r.html_url, r.is_watched, r.last_refreshed_at
             FROM repos r WHERE (r.is_watched = 1",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if !f.workspace_repos.is_empty() {
            let repo_clause = in_clause(f.workspace_repos.len());
            sql.push_str(&format!(" OR r.full_name IN {repo_clause}"));
            for repo in f.workspace_repos {
                params.push(Box::new(repo.clone()));
            }
        }
        sql.push(')');
        if f.show == "watched" {
            sql.push_str(" AND r.is_watched = 1");
        } else if f.show == "untracked" {
            sql.push_str(" AND r.is_watched = 0");
        }
        if let Some(search) = f.search.filter(|s| !s.is_empty()) {
            sql.push_str(" AND r.full_name LIKE ?");
            params.push(Box::new(format!("%{search}%")));
        }
        sql.push_str(" ORDER BY r.full_name");

        let workspace_set: std::collections::HashSet<&str> =
            f.workspace_repos.iter().map(String::as_str).collect();
        let mut stmt = conn.prepare(&sql).context("preparing repos query")?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                let full_name: String = row.get(0)?;
                Ok(RepoItem {
                    in_workspace: workspace_set.contains(full_name.as_str()),
                    full_name,
                    html_url: row.get(1)?,
                    is_watched: row.get::<_, i64>(2)? != 0,
                    last_refreshed_at: row.get(3)?,
                })
            })
            .context("querying repos")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading repos")
    }

    /// The API url of an issue/PR row, if present.
    pub fn issue_api_url(&self, id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.query_row(
            "SELECT api_url FROM issues WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .context("looking up issue api_url")
    }

    /// The API url of a thread by its notification id.
    pub fn thread_api_url(&self, thread_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.query_row(
            "SELECT api_url FROM threads WHERE thread_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )
        .optional()
        .context("looking up thread api_url")
    }

    /// Thread ids + API urls for threads whose subject matches an issue API
    /// url (used to mark a queue item's threads read).
    pub fn threads_for_subject(&self, subject_api_url: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().expect("db lock poisoned");
        let mut stmt = conn
            .prepare("SELECT thread_id, api_url FROM threads WHERE subject_api_url = ?1")
            .context("preparing threads_for_subject")?;
        let rows = stmt
            .query_map(params![subject_api_url], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("querying threads_for_subject")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("reading threads_for_subject row")?);
        }
        Ok(out)
    }

    /// Set the unread flag for a set of thread ids (dynamic IN clause).
    pub fn set_threads_unread(&self, thread_ids: &[String], unread: bool) -> Result<()> {
        if thread_ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().expect("db lock poisoned");
        let in_clause = in_clause(thread_ids.len());
        let sql = format!(
            "UPDATE threads SET unread = ?{} WHERE thread_id IN {in_clause}",
            thread_ids.len() + 1
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for id in thread_ids {
            params.push(Box::new(id.clone()));
        }
        params.push(Box::new(unread));
        conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
            .context("updating thread unread flags")?;
        Ok(())
    }

    /// Set the unread flag for all threads in the given repos.
    pub fn set_unread_for_repos(&self, repos: &[String], unread: bool) -> Result<()> {
        if repos.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().expect("db lock poisoned");
        let in_clause = in_clause(repos.len());
        let sql = format!(
            "UPDATE threads SET unread = ?{}
             WHERE repo_id IN (SELECT id FROM repos WHERE full_name IN {in_clause})",
            repos.len() + 1
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for repo in repos {
            params.push(Box::new(repo.clone()));
        }
        params.push(Box::new(unread));
        conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
            .context("updating unread flags for repos")?;
        Ok(())
    }

    /// Set a repo's watched flag locally.
    pub fn set_repo_watched(&self, full_name: &str, watched: bool) -> Result<()> {
        let conn = self.conn.lock().expect("db lock poisoned");
        conn.execute(
            "UPDATE repos SET is_watched = ?2 WHERE full_name = ?1",
            params![full_name, watched],
        )
        .context("updating repo watched flag")?;
        Ok(())
    }

    /// Run a closure against the guarded connection (used by cross-module
    /// tests to run ad-hoc queries).
    #[cfg(test)]
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let conn = self.conn.lock().expect("db lock poisoned");
        f(&conn)
    }
}

/// Build an `(?, ?, ...)` placeholder clause for `count` values.
fn in_clause(count: usize) -> String {
    let placeholders: Vec<String> = (0..count).map(|i| format!("?{}", i + 1)).collect();
    format!("({})", placeholders.join(","))
}

/// Split an `owner/repo` full name into its parts. Falls back to treating the
/// whole string as the repo name when there is no slash.
fn split_repo(full_name: &str) -> (&str, &str) {
    match full_name.split_once('/') {
        Some((owner, name)) => (owner, name),
        None => (full_name, ""),
    }
}

/// Convert a subject API url (e.g. `https://api.github.com/repos/o/r/pulls/7`)
/// into the corresponding github.com HTML url.
fn subject_html_url(api: Option<&str>) -> Option<String> {
    let api = api?;
    let rest = api.strip_prefix("https://api.github.com/repos/")?;
    let parts: Vec<&str> = rest.splitn(4, '/').collect();
    if parts.len() != 4 {
        return None;
    }
    let (owner, repo, kind, id) = (parts[0], parts[1], parts[2], parts[3]);
    let kind = match kind {
        "pulls" => "pull",
        "commits" => "commit",
        other => other,
    };
    Some(format!("https://github.com/{owner}/{repo}/{kind}/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("open");
        drop(db);
    }

    #[test]
    fn sync_state_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("open");
        assert_eq!(db.get_sync_state("last_sync").expect("get"), None);
        db.set_sync_state("last_sync", "2026-01-01T00:00:00Z")
            .expect("set");
        assert_eq!(
            db.get_sync_state("last_sync").expect("get").as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn data_dir_respects_env_override() {
        std::env::set_var("GHNOTIFY_DATA_DIR", "/tmp/custom");
        assert_eq!(default_data_dir(), PathBuf::from("/tmp/custom"));
        std::env::remove_var("GHNOTIFY_DATA_DIR");
    }

    #[test]
    fn schema_tables_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::open(&dir.path().join("data.db")).expect("open");
        let conn = db.conn.lock().expect("lock");
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .expect("prepare");
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("collect");
        for expected in ["repos", "issues", "threads", "sync_state"] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}"
            );
        }
    }
}
