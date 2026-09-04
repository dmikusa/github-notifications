//! Online smoke test against the live GitHub API.
//!
//! Skipped by default. Run with `cargo online-checks` (alias for
//! `cargo test -- --ignored`). Uses `GITHUB_TOKEN` if set, otherwise the `gh`
//! CLI's token when `gh` is logged in. Skips cleanly when neither is available.

use std::sync::Arc;

use github_notifications::{
    auth::{self, TokenProvider},
    config::{AuthProvider, Config, GithubConfig, RepoSet, Workspace},
    db,
    github::Client,
    sync,
};

#[tokio::test]
#[ignore = "requires live GitHub credentials; run via `cargo online-checks`"]
async fn live_sync_polls_github() {
    let Some(provider) = live_provider() else {
        eprintln!("skipping live smoke test: no GITHUB_TOKEN and gh is not logged in");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let db = db::Database::open(&dir.path().join("data.db")).expect("open db");

    let repo = std::env::var("GH_NOTIFY_SMOKE_REPO")
        .unwrap_or_else(|_| "dmikusa/github-notifications".to_string());

    let config = Config {
        github: GithubConfig {
            auth_provider: AuthProvider::Pat,
            ..Default::default()
        },
        workspaces: vec![Workspace {
            name: "smoke".into(),
            auto_dismiss_closed_merged: false,
            repo_sets: vec![RepoSet {
                name: "smoke".into(),
                repos: vec![repo],
            }],
        }],
    };

    let client = Client::new(provider);
    let stamp = sync::sync_all(&client, &db, &config)
        .await
        .expect("live sync should succeed");
    assert!(!stamp.is_empty(), "sync should return a timestamp");

    assert!(
        db.count("threads").expect("thread count") > 0,
        "expected the notification inbox to be cached"
    );
    assert!(
        db.count("repos").expect("repo count") > 0,
        "expected repositories to be cached"
    );
}

/// A token provider for online checks: `GITHUB_TOKEN` if set, otherwise the
/// `gh` CLI token when `gh` is logged in.
fn live_provider() -> Option<Arc<dyn TokenProvider>> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return Some(Arc::new(auth::pat::ClassicPat::new(token)));
        }
    }
    let gh_logged_in = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if gh_logged_in {
        return Some(Arc::new(auth::gh::GhToken::new()));
    }
    None
}
