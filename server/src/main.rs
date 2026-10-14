use std::net::SocketAddr;
use std::time::Duration;

use helix_bridge::api::{self, AppState};
use helix_bridge::device::{CliDevice, Device, FakeDevice};
use helix_bridge::usb::UsbDevice;
use tracing_subscriber::EnvFilter;

/// Configuration from the environment. See README for the variables.
struct Config {
    backend: String,
    bind: SocketAddr,
    tonepush: String,
    timeout: Duration,
    fixtures: Option<String>,
    fake_delay: Duration,
    cooldown: Duration,
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
        let cooldown = env_or("HELIX_SESSION_COOLDOWN_MS", "3000");
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
            cooldown: Duration::from_millis(
                cooldown
                    .parse()
                    .map_err(|e| format!("HELIX_SESSION_COOLDOWN_MS={cooldown:?}: {e}"))?,
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

    // Diagnostic: `helix-bridge watch [SECONDS]` (see usb::watch).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("watch") {
        let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(180);
        if let Err(e) = helix_bridge::usb::watch(seconds) {
            eprintln!("watch failed: {e}");
            std::process::exit(1);
        }
        return;
    }

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
                CliDevice::new(&config.tonepush, config.timeout).with_cooldown(config.cooldown),
                config.bind,
                config.token,
            )
            .await
        }
        "usb" => {
            let device = UsbDevice::start();
            let closer = device.closer();
            let result = serve(device, config.bind, config.token).await;
            closer.close();
            result
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
            tracing::error!("HELIX_BACKEND must be \"usb\", \"cli\" or \"fake\", got {other:?}");
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
    let state = AppState::new(device, token).with_based_on(helix_bridge::based_on::load());
    axum::serve(listener, api::router(state))
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
