use std::net::SocketAddr;
use std::time::Duration;

use helix_bridge::api::{self, AppState};
use helix_bridge::device::{CliDevice, Device, FakeDevice};
use tracing_subscriber::EnvFilter;

/// Configuration from the environment. See README for the variables.
struct Config {
    backend: String,
    bind: SocketAddr,
    tonepush: String,
    timeout: Duration,
    fixtures: Option<String>,
    fake_delay: Duration,
    /// Shared token for writes; unset or empty = writes off.
    token: Option<String>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let bind = env_or("HELIX_BIND", "0.0.0.0:8080");
        let timeout = env_or("HELIX_TONEPUSH_TIMEOUT_SECS", "30");
        let fake_delay = env_or("HELIX_FAKE_DELAY_MS", "0");
        Ok(Config {
            backend: env_or("HELIX_BACKEND", "fake"),
            bind: bind
                .parse()
                .map_err(|e| format!("HELIX_BIND={bind:?}: {e}"))?,
            tonepush: env_or("HELIX_TONEPUSH", "tonepush"),
            timeout: Duration::from_secs(
                timeout
                    .parse()
                    .map_err(|e| format!("HELIX_TONEPUSH_TIMEOUT_SECS={timeout:?}: {e}"))?,
            ),
            fixtures: std::env::var("HELIX_FIXTURES").ok(),
            fake_delay: Duration::from_millis(
                fake_delay
                    .parse()
                    .map_err(|e| format!("HELIX_FAKE_DELAY_MS={fake_delay:?}: {e}"))?,
            ),
            token: std::env::var("HELIX_TOKEN")
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty()),
        })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Plain text under systemd, so `journalctl` shows no colour codes.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("bad configuration: {e}");
            std::process::exit(2);
        }
    };

    let result = match config.backend.as_str() {
        "cli" => {
            serve(
                CliDevice::new(&config.tonepush, config.timeout),
                config.bind,
                config.token,
            )
            .await
        }
        "fake" => {
            let fake = match &config.fixtures {
                Some(dir) => FakeDevice::new(dir),
                None => FakeDevice::bundled(),
            };
            serve(
                fake.with_delay(config.fake_delay),
                config.bind,
                config.token,
            )
            .await
        }
        other => {
            tracing::error!("HELIX_BACKEND must be \"cli\" or \"fake\", got {other:?}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        tracing::error!("server stopped: {e}");
        std::process::exit(1);
    }
}

async fn serve<D: Device>(
    device: D,
    bind: SocketAddr,
    token: Option<String>,
) -> std::io::Result<()> {
    let writes = if token.is_some() { "on" } else { "off" };
    tracing::info!(backend = device.kind(), %bind, writes, "helix-bridge listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, api::router(AppState::new(device, token)))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// Ctrl-C, or SIGTERM on Unix (what `systemctl stop` sends).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!("cannot listen for SIGTERM: {e}");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
}
