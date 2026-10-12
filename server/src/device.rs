//! Device access. Every call to the HX device goes through [`Device`].
//!
//! A backend returns the raw stdout of one `tonepush` command; parsing lives in
//! [`crate::parse`] so the real and fake backends share it.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command as Process;
use tokio::sync::Mutex;

pub use crate::fake::FakeDevice;

/// The `tonepush` commands the bridge may run.
///
/// Writes are limited to what phase 6 needs and TonePush has used on hardware:
/// load, bypass, parameter, rename, save. `move` (reorder) is left out: TonePush
/// marks it untested against hardware (AGENTS.md 6, unverified operations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    List,
    Info,
    Setlists,
    Presets {
        setlist: u32,
    },
    Preset,
    Chain,
    /// Load a stored preset into the edit buffer. Discards unsaved edits.
    Select {
        setlist: u32,
        index: u32,
    },
    /// Switch a block on or off, by `chain` position. Edit buffer only.
    Enable {
        block: u32,
        on: bool,
    },
    /// Set a parameter by index, in display units (`50`, `2.2 kHz`). Edit buffer only.
    Set {
        block: u32,
        param: u32,
        value: String,
    },
    /// Rename a stored preset. Written to the device's flash at once.
    Rename {
        setlist: u32,
        index: u32,
        name: String,
    },
    /// Commit the edit buffer over the loaded preset, keeping its name.
    Save,
}

impl Command {
    /// Arguments after the program name.
    ///
    /// User text goes after `--`, so a value like `-22.1` is not read as a flag.
    pub fn args(&self) -> Vec<String> {
        match self {
            Command::List => vec!["list".into()],
            Command::Info => vec!["info".into()],
            Command::Setlists => vec!["setlists".into()],
            Command::Presets { setlist } => {
                vec!["presets".into(), format!("--setlist={setlist}")]
            }
            Command::Preset => vec!["preset".into()],
            Command::Chain => vec!["chain".into()],
            Command::Select { setlist, index } => vec![
                "select".into(),
                format!("--setlist={setlist}"),
                "--".into(),
                index.to_string(),
            ],
            Command::Enable { block, on } => vec![
                "enable".into(),
                "--".into(),
                block.to_string(),
                if *on { "on" } else { "off" }.into(),
            ],
            Command::Set {
                block,
                param,
                value,
            } => vec![
                "set".into(),
                "--".into(),
                block.to_string(),
                param.to_string(),
                value.clone(),
            ],
            Command::Rename {
                setlist,
                index,
                name,
            } => vec![
                "rename".into(),
                format!("--setlist={setlist}"),
                "--".into(),
                index.to_string(),
                name.clone(),
            ],
            Command::Save => vec!["save".into()],
        }
    }

    /// True if the command changes the device.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Command::Select { .. }
                | Command::Enable { .. }
                | Command::Set { .. }
                | Command::Rename { .. }
                | Command::Save
        )
    }
}

#[derive(Debug)]
pub enum DeviceError {
    /// `tonepush` could not be started at all.
    Spawn(String),
    /// `tonepush` found no HX device on USB. Holds its stderr.
    NotConnected(String),
    /// `tonepush` ran and failed. Holds its stderr.
    Failed(String),
    /// `tonepush` did not finish in time and was stopped.
    Timeout(Duration),
    /// The fake backend has no recording for this command.
    NoFixture(Command),
    /// Output did not have the expected shape.
    Parse(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::Spawn(e) => write!(f, "could not start tonepush: {e}"),
            DeviceError::NotConnected(e) | DeviceError::Failed(e) => {
                write!(f, "tonepush failed: {e}")
            }
            DeviceError::Timeout(d) => write!(f, "tonepush took longer than {}s", d.as_secs()),
            DeviceError::NoFixture(c) => write!(f, "no fixture recorded for {c:?}"),
            DeviceError::Parse(e) => write!(f, "unexpected tonepush output: {e}"),
        }
    }
}

impl std::error::Error for DeviceError {}

pub type Result<T> = std::result::Result<T, DeviceError>;

/// A source of `tonepush` output.
pub trait Device: Send + Sync + 'static {
    /// Short name for logs and `/api/health`.
    fn kind(&self) -> &'static str;

    /// Run one command and return its stdout.
    fn run(&self, cmd: Command) -> impl Future<Output = Result<String>> + Send;
}

/// The real device, through the `tonepush` CLI.
///
/// One command at a time: the device allows a single USB session, and two
/// overlapping `tonepush` processes would fight over the interface.
pub struct CliDevice {
    program: PathBuf,
    timeout: Duration,
    cooldown: Duration,
    session: Mutex<Session>,
}

/// Held under the lock, so it also serialises the calls.
#[derive(Default)]
struct Session {
    /// When the last `tonepush` process exited.
    last_end: Option<Instant>,
    /// `info` output. Name, id and firmware do not change while the device
    /// stays connected, and every saved call is one USB session less.
    info: Option<String>,
}

/// Gap between two `tonepush` sessions. Windows keeps the interface claimed
/// for a moment after a process exits; a call started sooner spends 11 s or
/// more failing to claim it. Measured on the HX Effects (2026-10-12): 3 s
/// worked every time, 1.5 s sometimes cost 13 s.
const SESSION_COOLDOWN: Duration = Duration::from_secs(3);

/// Waits before each retry of a "could not claim interface" failure. A session
/// that just ended can hold the interface for a few seconds (fixtures README).
const CLAIM_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(8),
];

/// What `tonepush` prints when nothing is plugged in (`main.rs` at v0.9.0).
const NO_DEVICE: &str = "no HX device found";

impl CliDevice {
    pub fn new(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        CliDevice {
            program: program.into(),
            timeout,
            cooldown: SESSION_COOLDOWN,
            session: Mutex::new(Session::default()),
        }
    }

    /// Run one session, after the cooldown since the previous one.
    async fn run_spaced(&self, session: &mut Session, cmd: &Command) -> Result<String> {
        if let Some(end) = session.last_end {
            let wait = self.cooldown.saturating_sub(end.elapsed());
            if !wait.is_zero() {
                tracing::debug!(?cmd, ?wait, "waiting for the previous session to clear");
                tokio::time::sleep(wait).await;
            }
        }
        let result = self.run_once(cmd).await;
        session.last_end = Some(Instant::now());
        result
    }

    async fn run_once(&self, cmd: &Command) -> Result<String> {
        let child = Process::new(&self.program)
            .args(cmd.args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| DeviceError::Spawn(format!("{}: {e}", self.program.display())))?;

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| DeviceError::Timeout(self.timeout))?
            .map_err(|e| DeviceError::Spawn(e.to_string()))?;

        if output.status.success() {
            String::from_utf8(output.stdout).map_err(|e| DeviceError::Parse(e.to_string()))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(if stderr.contains(NO_DEVICE) {
                DeviceError::NotConnected(stderr)
            } else if stderr.is_empty() {
                DeviceError::Failed(output.status.to_string())
            } else {
                DeviceError::Failed(stderr)
            })
        }
    }
}

fn is_claim_failure(e: &DeviceError) -> bool {
    matches!(e, DeviceError::Failed(msg) if msg.contains("could not claim interface"))
}

impl Device for CliDevice {
    fn kind(&self) -> &'static str {
        "cli"
    }

    async fn run(&self, cmd: Command) -> Result<String> {
        let mut session = self.session.lock().await;
        if cmd == Command::Info {
            if let Some(info) = &session.info {
                return Ok(info.clone());
            }
        }
        if cmd.is_write() {
            tracing::info!(args = ?cmd.args(), "device write");
        }
        // Safe to retry a write too: a claim failure means tonepush never got
        // a session, so nothing reached the device.
        let mut result = self.run_spaced(&mut session, &cmd).await;
        for delay in CLAIM_RETRY_DELAYS {
            match &result {
                Err(e) if is_claim_failure(e) => {
                    tracing::warn!(?cmd, ?delay, "interface busy, retrying");
                    tokio::time::sleep(delay).await;
                    result = self.run_spaced(&mut session, &cmd).await;
                }
                _ => break,
            }
        }
        match &result {
            Ok(out) if cmd == Command::Info => session.info = Some(out.clone()),
            // Unplugged: the next device may be a different one.
            Err(DeviceError::NotConnected(_)) => session.info = None,
            _ => {}
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_args_keep_user_text_after_the_separator() {
        let set = Command::Set {
            block: 10,
            param: 0,
            value: "-22.1 dB".into(),
        };
        assert_eq!(set.args(), ["set", "--", "10", "0", "-22.1 dB"]);
        let rename = Command::Rename {
            setlist: 0,
            index: 7,
            name: "--help".into(),
        };
        assert_eq!(
            rename.args(),
            ["rename", "--setlist=0", "--", "7", "--help"]
        );
        assert!(rename.is_write() && Command::Save.is_write());
        assert!(!Command::Chain.is_write());
    }

    #[test]
    fn claim_failures_are_recognised() {
        let e = DeviceError::Failed("could not claim interface 0 (error 5).".into());
        assert!(is_claim_failure(&e));
        assert!(!is_claim_failure(&DeviceError::Failed(
            "no HX device found".into()
        )));
    }

    /// A stand-in `tonepush` that fails to claim the interface once, then
    /// prints `info`.
    #[cfg(unix)]
    #[tokio::test]
    async fn cli_retries_a_busy_interface() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("helix-bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("tonepush");
        let marker = dir.join("claimed");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 [ \"$1\" = info ] || exit 9\n\
                 if [ ! -e {m} ]; then touch {m}; echo 'could not claim interface 0 (busy)' >&2; exit 1; fi\n\
                 echo 'HX Effects (128 presets)'\n",
                m = marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let dev = CliDevice::new(&script, Duration::from_secs(5));
        let out = dev.run(Command::Info).await.unwrap();
        assert_eq!(out, "HX Effects (128 presets)\n");

        let err = dev.run(Command::Chain).await.unwrap_err();
        assert!(matches!(err, DeviceError::Failed(_)), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_reports_a_missing_device() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("helix-bridge-nodev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("tonepush");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'Error: no HX device found - check the USB cable' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = CliDevice::new(&script, Duration::from_secs(5))
            .run(Command::Info)
            .await
            .unwrap_err();
        assert!(matches!(err, DeviceError::NotConnected(_)), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn cli_reports_a_missing_program() {
        let dev = CliDevice::new("/nonexistent/tonepush", Duration::from_secs(1));
        let err = dev.run(Command::Info).await.unwrap_err();
        assert!(matches!(err, DeviceError::Spawn(_)));
    }
}
