use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use clap::Parser;
use github_notifications::{api, auth, config, db, github, sync, util};

#[derive(Parser)]
#[command(
    name = "github-notifications",
    version,
    about = "A local web app for managing GitHub notifications"
)]
struct Cli {
    /// Path to the config file (defaults to ~/.config/github-notifications/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Address to bind the HTTP server (defaults to $HOST:$PORT, or
    /// 127.0.0.1:8080 when those are unset)
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// Do not open the web UI in the default browser after starting
    #[arg(long)]
    no_open: bool,

    /// Enable debug logging (more detail, plus HTTP request tracing).
    /// Overridden by the RUST_LOG environment variable when set.
    #[arg(long)]
    debug: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let default_level = if cli.debug { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .init();

    // --config wins; otherwise honor GHNOTIFY_CONFIG (useful in containers).
    let config_path =
        resolve_config_path(cli.config.clone(), std::env::var("GHNOTIFY_CONFIG").ok());
    let loaded = config::Config::load(config_path.as_deref())?;
    let config = loaded.config;

    if config.workspaces.is_empty() {
        if loaded.created {
            println!(
                "Created a default config at {}.\nAdd a workspace and repo set to it, then run again.",
                loaded.path.display()
            );
        } else {
            eprintln!(
                "No workspaces are configured.\nAdd a workspace and repo set to {}, then run again.",
                loaded.path.display()
            );
            std::process::exit(1);
        }
        return Ok(());
    }

    let data_dir = db::default_data_dir();
    let db =
        Arc::new(db::Database::open(&data_dir.join("data.db")).context("opening local database")?);

    let token_store = auth::TokenStore::new(data_dir.join("auth.toml"));
    let token = auth::from_config(&config, token_store);
    let client = github::Client::new(token.clone());

    let validation = Arc::new(Mutex::new(None::<github::Validation>));
    if config.github.auth_provider != config::AuthProvider::OAuthDevice {
        match client.validate().await {
            Ok(v) => {
                if v.ok {
                    tracing::info!(
                        "authenticated as {}",
                        v.login.as_deref().unwrap_or("unknown")
                    );
                } else {
                    tracing::warn!(
                        "auth incomplete: {}",
                        v.message.as_deref().unwrap_or("unknown reason")
                    );
                }
                *validation.lock().expect("validation lock poisoned") = Some(v);
            }
            Err(e) => tracing::warn!("could not validate credentials: {e}"),
        }
    } else {
        tracing::info!("oauth-device auth configured; authorize from the web UI");
    }

    let addr = resolve_bind(cli.bind);
    tracing::info!("github-notifications listening on http://{addr}");

    let shared_config = Arc::new(RwLock::new(config.clone()));
    let config_path = loaded.path;
    let engine = sync::SyncEngine::spawn(client.clone(), db.clone(), shared_config.clone());

    let app = api::router(api::AppState {
        config: shared_config,
        config_path,
        db,
        token,
        github: client,
        validation,
        sync_status: engine.status.clone(),
        sync_trigger: engine.trigger.clone(),
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP server to {addr}"))?;

    // Open the browser only once the server is actually listening. Skip when
    // the user opted out or the server is bound beyond loopback (e.g. in a
    // container, where there is no local browser to open).
    if should_open_browser(cli.no_open, addr) {
        util::open_browser(&format!("http://{addr}"));
    }

    axum::serve(listener, app).await.context("serving HTTP")?;
    Ok(())
}

/// Whether to auto-open the browser: opt-out flag wins, and we never open one
/// for a non-loopback bind (containers/remote hosts).
fn should_open_browser(no_open: bool, addr: SocketAddr) -> bool {
    if no_open {
        return false;
    }
    addr.ip().is_loopback()
}

/// Determine the listen address: explicit `--bind`, then `$HOST:$PORT`
/// (defaults to `127.0.0.1:8080` so the server stays on loopback).
fn resolve_bind(cli_bind: Option<SocketAddr>) -> SocketAddr {
    resolve_bind_with_env(
        cli_bind,
        std::env::var("HOST").ok(),
        std::env::var("PORT").ok(),
    )
}

fn resolve_bind_with_env(
    cli_bind: Option<SocketAddr>,
    host: Option<String>,
    port: Option<String>,
) -> SocketAddr {
    if let Some(addr) = cli_bind {
        return addr;
    }
    let host = host
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = port.and_then(|p| p.parse::<u16>().ok()).unwrap_or(8080);
    SocketAddr::new(parse_host(&host), port)
}

fn parse_host(host: &str) -> IpAddr {
    match host {
        "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        other => other.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    }
}

/// Resolve the config path: explicit `--config`, else `GHNOTIFY_CONFIG`.
fn resolve_config_path(cli_config: Option<PathBuf>, env_config: Option<String>) -> Option<PathBuf> {
    cli_config.or_else(|| env_config.filter(|p| !p.is_empty()).map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_prefers_explicit() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert_eq!(
            resolve_bind_with_env(Some(addr), Some("0.0.0.0".into()), Some("9090".into())),
            addr
        );
    }

    #[test]
    fn bind_uses_port_on_localhost() {
        assert_eq!(
            resolve_bind_with_env(None, None, Some("9090".into())),
            SocketAddr::from(([127, 0, 0, 1], 9090))
        );
    }

    #[test]
    fn bind_uses_host_override() {
        assert_eq!(
            resolve_bind_with_env(None, Some("0.0.0.0".into()), Some("9090".into())),
            SocketAddr::from(([0, 0, 0, 0], 9090))
        );
    }

    #[test]
    fn bind_maps_localhost() {
        assert_eq!(
            resolve_bind_with_env(None, Some("localhost".into()), None),
            SocketAddr::from(([127, 0, 0, 1], 8080))
        );
    }

    #[test]
    fn bind_defaults_to_localhost() {
        assert_eq!(
            resolve_bind_with_env(None, None, None),
            SocketAddr::from(([127, 0, 0, 1], 8080))
        );
    }

    #[test]
    fn opens_browser_by_default_on_loopback() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(should_open_browser(false, addr));
    }

    #[test]
    fn no_open_disables_browser() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(!should_open_browser(true, addr));
    }

    #[test]
    fn never_opens_browser_for_non_loopback() {
        let addr: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        assert!(!should_open_browser(false, addr));
    }

    #[test]
    fn config_path_prefers_cli_over_env() {
        let cli = Some(PathBuf::from("/cli/config.toml"));
        assert_eq!(
            resolve_config_path(cli.clone(), Some("/env/config.toml".into())),
            cli
        );
    }

    #[test]
    fn config_path_uses_env_when_no_cli() {
        assert_eq!(
            resolve_config_path(None, Some("/env/config.toml".into())),
            Some(PathBuf::from("/env/config.toml"))
        );
    }

    #[test]
    fn config_path_ignores_empty_env() {
        assert_eq!(resolve_config_path(None, Some(String::new())), None);
        assert_eq!(resolve_config_path(None, None), None);
    }
}
