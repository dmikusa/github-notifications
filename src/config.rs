use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The default configuration file name used within the config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// The auth provider used to obtain GitHub API credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthProvider {
    /// A classic personal access token configured in `[github] auth_token` or
    /// the `GITHUB_TOKEN` environment variable.
    #[default]
    Pat,
    /// Reuse the token managed by the GitHub CLI (`gh auth token`).
    GhToken,
    /// OAuth device flow using a registered OAuth app (`[github] oauth_client_id`).
    OAuthDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default)]
    pub auth_provider: AuthProvider,
    #[serde(default)]
    pub auth_token: String,
    #[serde(default)]
    pub oauth_client_id: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
    #[serde(default = "default_repo_refresh_interval")]
    pub repo_refresh_interval_seconds: u64,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            auth_provider: AuthProvider::default(),
            auth_token: String::new(),
            oauth_client_id: String::new(),
            poll_interval_seconds: default_poll_interval(),
            repo_refresh_interval_seconds: default_repo_refresh_interval(),
        }
    }
}

fn default_poll_interval() -> u64 {
    300
}

fn default_repo_refresh_interval() -> u64 {
    600
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub name: String,
    #[serde(default)]
    pub auto_dismiss_closed_merged: bool,
    #[serde(default)]
    pub repo_sets: Vec<RepoSet>,
}

impl Workspace {
    /// All repos across this workspace's repo sets, deduplicated and sorted.
    pub fn tracked_repos(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for repo_set in &self.repo_sets {
            for repo in &repo_set.repos {
                if !repo.is_empty() {
                    set.insert(repo.clone());
                }
            }
        }
        set.into_iter().collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RepoSet {
    pub name: String,
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub github: GithubConfig,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}

impl Config {
    /// Load configuration from `path`, or create and return a default config
    /// file when it does not yet exist.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => default_config_path(),
        };

        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("reading config file {}", path.display()))?;
            let config: Config = toml::from_str(&raw)
                .with_context(|| format!("parsing config file {}", path.display()))?;
            return Ok(config);
        }

        let config = Config::default();
        write_template(&path)?;
        tracing::info!(
            "created default config file at {} \u{2014} edit it to add credentials and workspaces",
            path.display()
        );
        Ok(config)
    }

    /// Persist the config to `path`, creating parent directories and locking
    /// down file permissions when a token is present.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("serializing config")?;
        let mut file = fs::File::create(path)
            .with_context(|| format!("creating config file {}", path.display()))?;
        file.write_all(raw.as_bytes())
            .with_context(|| format!("writing config file {}", path.display()))?;
        if !self.github.auth_token.is_empty() {
            set_private_permissions(path)?;
        }
        Ok(())
    }

    /// The effective GitHub API token from config or the `GITHUB_TOKEN`
    /// environment variable. Empty when no token is configured.
    pub fn effective_token(&self) -> String {
        if !self.github.auth_token.is_empty() {
            self.github.auth_token.clone()
        } else {
            std::env::var("GITHUB_TOKEN").unwrap_or_default()
        }
    }
}

/// A full, commented example configuration written to disk on first run so
/// new users can see every option and an example workspace.
const CONFIG_TEMPLATE: &str = r#"# github-notifications configuration.
#
# Created on first run. Edit this file and restart the app to apply changes.
# See docs/setup.md for authentication setup and a plain example.

[github]
# Which credential source to use:
#   pat          - a classic personal access token (auth_token or GITHUB_TOKEN)
#   gh-token     - reuse the GitHub CLI's token (requires a logged-in `gh`)
#   oauth-device - browser OAuth device flow (uses oauth_client_id)
auth_provider = "pat"

# Classic PAT. Only used when auth_provider = "pat".
# Can also be provided via the GITHUB_TOKEN environment variable.
auth_token = ""

# OAuth app client ID. Only used when auth_provider = "oauth-device".
oauth_client_id = ""

# How often to poll GitHub for new notifications, in seconds.
poll_interval_seconds = 300

# How often to refresh open issues and pull requests for each tracked repo,
# in seconds.
repo_refresh_interval_seconds = 600

# Workspaces group repos and saved filters into separate views (e.g. personal
# vs work). Each workspace has one or more repo sets: explicit lists of repos
# to track.
#
# Orgs are not wildcards; every tracked repo must be listed.
#
# Uncomment and edit the example below to add your first workspace.
#
# [[workspaces]]
# name = "Personal"
# # When enabled, threads for closed + merged pull requests are auto-marked read.
# auto_dismiss_closed_merged = false
#
# [[workspaces.repo_sets]]
# name = "Org 1 Repos"
# repos = [
#   "org1/repo-a",
#   "org1/repo-b",
# ]
#
# [[workspaces.repo_sets]]
# name = "My Projects"
# repos = ["me/repo-a"]
"#;

/// Write the commented example configuration to `path`, creating parent
/// directories and locking down permissions.
fn write_template(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating config directory {}", parent.display()))?;
    }
    let mut file = fs::File::create(path)
        .with_context(|| format!("creating config file {}", path.display()))?;
    file.write_all(CONFIG_TEMPLATE.as_bytes())
        .with_context(|| format!("writing config file {}", path.display()))?;
    set_private_permissions(path)?;
    Ok(())
}

/// Resolve the default configuration path following the XDG base directory
/// specification on Unix and the conventional macOS location.
fn default_config_path() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("github-notifications")
            .join(CONFIG_FILE_NAME);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("github-notifications")
            .join(CONFIG_FILE_NAME);
    }
    PathBuf::from(CONFIG_FILE_NAME)
}

/// Restrict file permissions so only the owner can read the file. Best effort;
/// failures are ignored on platforms without POSIX permissions.
#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("setting config permissions")
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_round_trip() {
        let config = Config {
            github: GithubConfig {
                auth_provider: AuthProvider::GhToken,
                auth_token: String::new(),
                oauth_client_id: "client-123".into(),
                poll_interval_seconds: 60,
                repo_refresh_interval_seconds: 120,
            },
            workspaces: vec![Workspace {
                name: "personal".into(),
                auto_dismiss_closed_merged: true,
                repo_sets: vec![RepoSet {
                    name: "paketo".into(),
                    repos: vec!["paketo-buildpacks/abc".into()],
                }],
            }],
        };

        let raw = toml::to_string_pretty(&config).expect("serialize");
        let parsed: Config = toml::from_str(&raw).expect("parse");
        assert_eq!(parsed.github.auth_provider, AuthProvider::GhToken);
        assert_eq!(parsed.github.oauth_client_id, "client-123");
        assert_eq!(parsed.github.poll_interval_seconds, 60);
        assert_eq!(parsed.workspaces[0].name, "personal");
        assert!(parsed.workspaces[0].auto_dismiss_closed_merged);
        assert_eq!(
            parsed.workspaces[0].repo_sets[0].repos[0],
            "paketo-buildpacks/abc"
        );
    }

    #[test]
    fn load_creates_default_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        assert!(!path.exists());

        let config = Config::load(Some(&path)).expect("load");
        assert!(path.exists());
        assert!(config.workspaces.is_empty());
        assert_eq!(config.github.auth_provider, AuthProvider::Pat);

        // The written file is the commented example, not an empty serialization.
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("# github-notifications configuration"));
        assert!(raw.contains("auto_dismiss_closed_merged"));
    }

    #[test]
    fn template_parses_as_valid_config() {
        let config: Config = toml::from_str(CONFIG_TEMPLATE).expect("template parses");
        assert_eq!(config.github.auth_provider, AuthProvider::Pat);
        assert_eq!(config.github.poll_interval_seconds, 300);
        assert!(config.workspaces.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn template_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_template(&path).expect("write template");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn load_parses_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[github]
auth_provider = "gh-token"

[[workspaces]]
name = "work"
"#,
        )
        .expect("write");

        let config = Config::load(Some(&path)).expect("load");
        assert_eq!(config.github.auth_provider, AuthProvider::GhToken);
        assert_eq!(config.workspaces[0].name, "work");
    }

    #[test]
    fn unknown_fields_rejected() {
        let raw = r#"
[github]
bogus = true
"#;
        assert!(toml::from_str::<Config>(raw).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn saves_token_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let config = Config {
            github: GithubConfig {
                auth_token: "ghp_secret".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        config.save(&path).expect("save");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn effective_token_prefers_config() {
        let config = Config {
            github: GithubConfig {
                auth_token: "config-token".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(config.effective_token(), "config-token");
    }
}
