use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

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
