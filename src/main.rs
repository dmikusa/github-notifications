use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use github_notifications::{api, config, db};

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

    /// Address to bind the HTTP server (defaults to 127.0.0.1:8080, or
    /// 0.0.0.0:$PORT when the PORT environment variable is set)
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// Open the web UI in the default browser after starting
    #[arg(long)]
    open: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let config = config::Config::load(cli.config.as_deref())?;
    let db = db::Database::open(&db::default_data_dir().join("data.db"))
        .context("opening local database")?;

    let addr = resolve_bind(cli.bind);
    tracing::info!("github-notifications listening on http://{addr}");

    if cli.open {
        open_browser(&format!("http://{addr}"));
    }

    let app = api::router(api::AppState {
        config: Arc::new(config),
        db: Arc::new(db),
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP server to {addr}"))?;
    axum::serve(listener, app).await.context("serving HTTP")?;
    Ok(())
}

/// Determine the listen address: explicit `--bind`, then `$PORT` (container),
/// then the localhost default.
fn resolve_bind(cli_bind: Option<SocketAddr>) -> SocketAddr {
    if let Some(addr) = cli_bind {
        return addr;
    }
    if let Ok(port) = std::env::var("PORT")
        .and_then(|p| p.parse::<u16>().map_err(|_| std::env::VarError::NotPresent))
    {
        return SocketAddr::from(([0, 0, 0, 0], port));
    }
    SocketAddr::from(([127, 0, 0, 1], 8080))
}

/// Best-effort open of a URL in the platform default browser.
#[cfg(not(target_os = "windows"))]
fn open_browser(url: &str) {
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(command).arg(url).spawn() {
        tracing::warn!("could not open browser: {e}");
    }
}

#[cfg(target_os = "windows")]
fn open_browser(url: &str) {
    if let Err(e) = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
    {
        tracing::warn!("could not open browser: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_prefers_explicit() {
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert_eq!(resolve_bind(Some(addr)), addr);
    }

    #[test]
    fn bind_uses_port_env() {
        std::env::set_var("PORT", "9090");
        assert_eq!(resolve_bind(None), SocketAddr::from(([0, 0, 0, 0], 9090)));
        std::env::remove_var("PORT");
    }

    #[test]
    fn bind_defaults_to_localhost() {
        std::env::remove_var("PORT");
        assert_eq!(resolve_bind(None), SocketAddr::from(([127, 0, 0, 1], 8080)));
    }
}
