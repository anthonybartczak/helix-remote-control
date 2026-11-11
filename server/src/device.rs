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
use tokio::sync::{broadcast, Mutex};

pub use crate::controls::{CategoryInfo, ModelGroup, ModelInfo};
pub use crate::fake::FakeDevice;

/// The `tonepush` commands the bridge may run, and [`Command::SetNative`].
///
/// Writes are limited to what phase 6 needs and TonePush has used on hardware:
/// load, bypass, parameter, rename, save, snapshot, tempo, and adding or
/// removing a block. `move` (reorder) is left out: TonePush marks it untested
/// against hardware (AGENTS.md 6, unverified operations).
#[derive(Debug, Clone, PartialEq)]
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
    /// Set a parameter by its native value (a slider position), checked
    /// against the HX Edit catalog. `model` is the model the client saw in
    /// that slot; a different one there answers [`DeviceError::Stale`].
    /// Edit buffer only. No `tonepush` equivalent.
    SetNative {
        block: u32,
        param: u32,
        native: f32,
        model: u32,
    },
    /// Rename a stored preset. Written to the device's flash at once.
    Rename {
        setlist: u32,
        index: u32,
        name: String,
    },
    /// Commit the edit buffer over the loaded preset, keeping its name.
    Save,
    /// Switch snapshot, zero-based (`tonepush snapshot` counts from 1).
    /// Opcode 88; changes the edit buffer's state, not stored presets.
    Snapshot {
        index: u32,
    },
    /// Set the loaded preset's tempo in BPM (40-240). Edit buffer only, but
    /// TonePush writes the whole preset document for it (no tempo opcode).
    Tempo {
        bpm: f32,
    },
    /// Put a model into an empty block slot: op78 (select) then op40 (set
    /// model), as HX Edit does. Adds, never replaces: an occupied slot is
    /// refused with [`DeviceError::Stale`] (`usb`, `fake`; `cli` cannot check).
    /// Edit buffer only.
    AddBlock {
        block: u32,
        model: u32,
    },
    /// Remove a block: op78 (select) then op28 (clear). `model`, if given, is
    /// the model the client saw there; another one answers
    /// [`DeviceError::Stale`] (`usb`, `fake`; `cli` cannot check). Edit
    /// buffer only.
    RemoveBlock {
        block: u32,
        model: Option<u32>,
    },
    /// Put a block's bypass on footswitch `switch` (one-based, as printed on
    /// the pedal), or take it off every footswitch with `None`. Opcode 56
    /// (assign) and 57 (unassign), zero-based on the wire. A block on another
    /// switch is taken off it first. `model` as in [`Command::RemoveBlock`].
    /// Edit buffer only. `cli` can only assign (`tonepush switch-assign`).
    Footswitch {
        block: u32,
        switch: Option<u8>,
        model: Option<u32>,
    },
    /// Move a block into an empty cell of Path A or Path B
    /// ([`crate::routing::check_cell`]); the first block onto an empty Path B
    /// also attaches the split and merge around the whole line. `model` as in
    /// [`Command::RemoveBlock`]. A preset document write (op21), as TonePush's
    /// GUI moves blocks; unverified on the HX Effects. Edit buffer only.
    /// `usb` and `fake` only.
    MoveBlock {
        block: u32,
        to: u32,
        model: Option<u32>,
    },
    /// Attach the split or the merge before cell `before` (zero-based) of
    /// Path A ([`crate::routing::check_junction`]). A preset document write
    /// (op21); unverified on the HX Effects. Edit buffer only. `usb` and
    /// `fake` only.
    Junction {
        which: crate::routing::Junction,
        before: u32,
    },
    /// Put parameter `param` of the block at `block` under expression pedal
    /// `pedal` (1 or 2), or take it off whatever controls it with `None`.
    /// Opcode 37, as HX Edit's Controller Assign page; the device starts the
    /// travel at the parameter's full range. `model` as in
    /// [`Command::RemoveBlock`]. Unverified on the HX Effects. Edit buffer
    /// only. `usb` and `fake` only.
    Controller {
        block: u32,
        param: u32,
        pedal: Option<u8>,
        model: Option<u32>,
    },
    /// Move the heel (`min`) and/or toe (`max`) end of a parameter's pedal
    /// travel, as native values. Opcodes 65 and 66, which take a fraction of
    /// the parameter's range. The parameter must be under a controller.
    /// Unverified on the HX Effects. Edit buffer only. `usb` and `fake` only.
    ControllerRange {
        block: u32,
        param: u32,
        min: Option<f32>,
        max: Option<f32>,
        model: Option<u32>,
    },
}

impl Command {
    /// Arguments after the program name, or `None` for a command `tonepush`
    /// does not have.
    ///
    /// User text goes after `--`, so a value like `-22.1` is not read as a flag.
    pub fn args(&self) -> Option<Vec<String>> {
        Some(match self {
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
            Command::Snapshot { index } => {
                vec!["snapshot".into(), "--".into(), (index + 1).to_string()]
            }
            Command::Tempo { bpm } => vec!["tempo".into(), "--".into(), format!("{bpm:.1}")],
            Command::AddBlock { block, model } => vec![
                "model".into(),
                "--".into(),
                block.to_string(),
                model.to_string(),
            ],
            Command::RemoveBlock { block, .. } => {
                vec!["clear".into(), "--".into(), block.to_string()]
            }
            Command::Footswitch {
                block,
                switch: Some(switch),
                ..
            } => vec![
                "switch-assign".into(),
                "--".into(),
                block.to_string(),
                switch.to_string(),
            ],
            // Taking it off needs the switch it is on, which `tonepush` does
            // not print.
            Command::SetNative { .. }
            | Command::Footswitch { switch: None, .. }
            | Command::MoveBlock { .. }
            | Command::Junction { .. }
            | Command::Controller { .. }
            | Command::ControllerRange { .. } => return None,
        })
    }

    /// True if the command changes the device.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Command::Select { .. }
                | Command::Enable { .. }
                | Command::Set { .. }
                | Command::SetNative { .. }
                | Command::Rename { .. }
                | Command::Save
                | Command::Snapshot { .. }
                | Command::Tempo { .. }
                | Command::AddBlock { .. }
                | Command::RemoveBlock { .. }
                | Command::Footswitch { .. }
                | Command::MoveBlock { .. }
                | Command::Junction { .. }
                | Command::Controller { .. }
                | Command::ControllerRange { .. }
        )
    }
}

#[derive(Debug, Clone)]
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
    /// The request no longer matches the device (another model in that slot).
    Stale(String),
    /// The device side refused the value before sending it (out of range).
    Invalid(String),
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
            DeviceError::Stale(e) | DeviceError::Invalid(e) => f.write_str(e),
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

    /// Changes the device reports by itself (footswitches, knobs, our own
    /// writes echoed back). `None` for a backend that cannot see them.
    fn subscribe(&self) -> Option<broadcast::Receiver<Live>> {
        None
    }

    /// `chain` output plus what `tonepush chain` does not print: native
    /// values for sliders and the active snapshot. Empty unless the backend
    /// reads the preset document itself.
    fn chain(&self) -> impl Future<Output = Result<(String, ChainExtra)>> + Send {
        async move { Ok((self.run(Command::Chain).await?, ChainExtra::default())) }
    }

    /// Parameter metadata for a model number (`/api/models/{id}`). `None`
    /// without HX Edit's catalog (`cli` backend, or `usb` with no `/vendor`).
    fn model(&self, _id: u32) -> Option<ModelInfo> {
        None
    }

    /// A block's category from HX Edit's catalog, by model number or name.
    /// `None` without the catalog; the UI then guesses from the name.
    fn category(&self, _model_id: Option<u32>, _name: &str) -> Option<CategoryInfo> {
        None
    }

    /// The models a block can be added with, by category (`/api/catalog`).
    /// `None` without HX Edit's catalog.
    fn catalog(&self) -> Option<Vec<ModelGroup>> {
        None
    }
}

/// What a backend knows about the loaded preset beyond `chain` text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChainExtra {
    /// Native values, only with parameter metadata ([`Device::model`]).
    pub natives: Option<Natives>,
    /// Active snapshot, zero-based.
    pub snapshot: Option<u32>,
    /// Every footswitch and the blocks whose bypass it carries (opcode 33).
    /// `None` when the backend cannot read them or the read failed.
    pub footswitches: Option<Vec<crate::parse::Footswitch>>,
    /// Path A, Path B, split and merge. `None` when the backend does not
    /// read the preset document (`cli`).
    pub routing: Option<crate::parse::Routing>,
    /// Controller assignments by block position. `None` when the backend
    /// does not read the preset document (`cli`).
    pub assignments: Option<Vec<(u32, crate::parse::Assignment)>>,
}

/// Native parameter values of the loaded preset, per block.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Natives(pub Vec<BlockNatives>);

#[derive(Debug, Clone, PartialEq)]
pub struct BlockNatives {
    /// Block number as `chain` prints it.
    pub position: u32,
    /// Model number (`/api/models/{id}`).
    pub model: u32,
    /// Values of the primary model, by parameter index.
    pub values: Vec<f32>,
}

/// Something that changed on the device without the client asking.
#[derive(Debug, Clone, PartialEq)]
pub enum Live {
    /// The loaded preset's chain, in `tonepush chain` format, after a change.
    Chain {
        text: String,
        cause: Cause,
        extra: ChainExtra,
    },
    /// The device session came up (`true`) or was lost (`false`).
    Connected(bool),
}

/// Why the chain changed, as far as the device's notifications say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cause {
    /// Anything else: snapshot, an event never captured.
    Other,
    /// A parameter or bypass changed: the edit buffer now differs from storage.
    Edit,
    /// A preset was loaded: edits are gone.
    Preset,
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

    /// Gap between two `tonepush` sessions (default 3 s, measured on Windows).
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
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
        let args = cmd.args().ok_or_else(|| {
            DeviceError::Invalid(match cmd {
                Command::Footswitch { .. } => {
                    "taking a block off its footswitch needs HELIX_BACKEND=usb".into()
                }
                _ => "setting a parameter by native value needs HELIX_BACKEND=usb".into(),
            })
        })?;
        let child = Process::new(&self.program)
            .args(args)
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
        if cmd.args().is_none() {
            return self.run_once(&cmd).await;
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

    /// Held for the whole of every test that starts a process. Tests run in
    /// parallel threads: a fork in one test while another still has its
    /// stand-in `tonepush` open for writing leaves that file open in the
    /// forked child until the child execs, and running the stand-in then
    /// fails on Linux with "Text file busy" (ETXTBSY).
    static PROCESSES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn write_args_keep_user_text_after_the_separator() {
        let set = Command::Set {
            block: 10,
            param: 0,
            value: "-22.1 dB".into(),
        };
        assert_eq!(set.args().unwrap(), ["set", "--", "10", "0", "-22.1 dB"]);
        let rename = Command::Rename {
            setlist: 0,
            index: 7,
            name: "--help".into(),
        };
        assert_eq!(
            rename.args().unwrap(),
            ["rename", "--setlist=0", "--", "7", "--help"]
        );
        assert!(rename.is_write() && Command::Save.is_write());
        let native = Command::SetNative {
            block: 2,
            param: 0,
            native: 0.4,
            model: 269,
        };
        assert!(native.is_write() && native.args().is_none());
        // tonepush counts snapshots from 1; the API from 0.
        let snap = Command::Snapshot { index: 1 };
        assert_eq!(snap.args().unwrap(), ["snapshot", "--", "2"]);
        let tempo = Command::Tempo { bpm: 96.04 };
        assert_eq!(tempo.args().unwrap(), ["tempo", "--", "96.0"]);
        assert!(snap.is_write() && tempo.is_write());
        let add = Command::AddBlock {
            block: 4,
            model: 388,
        };
        assert_eq!(add.args().unwrap(), ["model", "--", "4", "388"]);
        let remove = Command::RemoveBlock {
            block: 4,
            model: Some(388),
        };
        assert_eq!(remove.args().unwrap(), ["clear", "--", "4"]);
        assert!(add.is_write() && remove.is_write());
        let on = Command::Footswitch {
            block: 4,
            switch: Some(3),
            model: None,
        };
        assert_eq!(on.args().unwrap(), ["switch-assign", "--", "4", "3"]);
        let off = Command::Footswitch {
            block: 4,
            switch: None,
            model: None,
        };
        assert!(on.is_write() && off.is_write() && off.args().is_none());
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

        let _processes = PROCESSES.lock().await;
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

        let _processes = PROCESSES.lock().await;
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

    /// `tonepush` has no native-value set; the CLI backend refuses it
    /// without starting a process.
    #[tokio::test]
    async fn cli_refuses_native_values() {
        let dev = CliDevice::new("/nonexistent/tonepush", Duration::from_secs(1));
        let cmd = Command::SetNative {
            block: 2,
            param: 0,
            native: 0.4,
            model: 269,
        };
        let err = dev.run(cmd).await.unwrap_err();
        assert!(matches!(err, DeviceError::Invalid(_)), "{err}");
    }

    #[tokio::test]
    async fn cli_reports_a_missing_program() {
        // Starting a missing program still forks first.
        let _processes = PROCESSES.lock().await;
        let dev = CliDevice::new("/nonexistent/tonepush", Duration::from_secs(1));
        let err = dev.run(Command::Info).await.unwrap_err();
        assert!(matches!(err, DeviceError::Spawn(_)));
    }
}
