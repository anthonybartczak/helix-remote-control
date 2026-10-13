//! [`UsbDevice`]: the device through TonePush's `hx-usb` library, one session
//! held open for as long as the pedal stays connected.
//!
//! A worker thread owns the session (its API is blocking). It runs commands,
//! and between them reads the pedal's notifications every 700 ms and sends
//! keep-alives, the same loop TonePush's own GUI and `tonepush watch` use.
//! When a notification says something changed, it re-reads the chain inside
//! the session and publishes it as [`Live::Chain`].
//!
//! Commands answer in the text formats `tonepush` v0.9.0 prints, ported from
//! `crates/tonepush-cli/src/main.rs`, so [`crate::parse`] reads both backends.
//!
//! Device hazards (PROTOCOL.md): never reset USB; one handshake per session
//! (`Found::open`); keep reading so notifications never back up; a session
//! that hit a transport error is dropped, never reused.

use std::fmt::Write as _;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hx_proto::msgpack::Value;
use hx_usb::Session;
use tokio::sync::{broadcast, oneshot};

use crate::device::{Cause, Command, Device, DeviceError, Live, Result};

/// How often an idle session reads notifications and sends keep-alives.
/// TonePush uses 700 ms; faster polling wedged a device during its development.
const POLL: Duration = Duration::from_millis(700);
/// Wait this long after a notification for more before re-reading the chain:
/// one footswitch press arrives as several notifications.
const SETTLE: Duration = Duration::from_millis(250);
/// Re-read at the latest this long after the first notification of a burst.
const SETTLE_MAX: Duration = Duration::from_secs(1);
/// With no session, look for the pedal this often (only USB enumeration).
const RECONNECT: Duration = Duration::from_secs(5);
/// After a failed open of a pedal that is present, wait longer each time, up
/// to this. A wedged pedal refuses every session until its power is pulled;
/// there is no point knocking every 5 s.
const RECONNECT_MAX: Duration = Duration::from_secs(60);

const NO_DEVICE: &str = "no HX device found - check the USB cable";

enum Job {
    Run(Command, oneshot::Sender<Result<String>>),
    /// Close the session cleanly and end the worker.
    Stop,
}

pub struct UsbDevice {
    jobs: mpsc::Sender<Job>,
    live: broadcast::Sender<Live>,
    worker: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

/// Closes the session on shutdown. Kept apart from [`UsbDevice`], which the
/// server owns until it stops.
pub struct UsbCloser {
    jobs: mpsc::Sender<Job>,
    worker: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl UsbCloser {
    /// End the session the way HX Edit does (`Session`'s `Drop`). A session
    /// that just vanishes can leave the pedal refusing connections until its
    /// power is pulled (hx-usb), so call this before the process exits.
    pub fn close(self) {
        let _ = self.jobs.send(Job::Stop);
        let worker = self.worker.lock().ok().and_then(|mut w| w.take());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

impl UsbDevice {
    /// Start the worker. It connects at once and reconnects by itself.
    pub fn start() -> Self {
        let (jobs, rx) = mpsc::channel();
        let (live, _) = broadcast::channel(16);
        let publish = live.clone();
        let worker = thread::Builder::new()
            .name("hx-usb".into())
            .spawn(move || Worker::new(publish).run(rx))
            .expect("spawn the USB worker thread");
        UsbDevice {
            jobs,
            live,
            worker: Arc::new(Mutex::new(Some(worker))),
        }
    }

    pub fn closer(&self) -> UsbCloser {
        UsbCloser {
            jobs: self.jobs.clone(),
            worker: self.worker.clone(),
        }
    }
}

impl Device for UsbDevice {
    fn kind(&self) -> &'static str {
        "usb"
    }

    async fn run(&self, cmd: Command) -> Result<String> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .send(Job::Run(cmd, reply))
            .map_err(|_| DeviceError::Spawn("USB worker stopped".into()))?;
        answer
            .await
            .map_err(|_| DeviceError::Spawn("USB worker stopped".into()))?
    }

    fn subscribe(&self) -> Option<broadcast::Receiver<Live>> {
        Some(self.live.subscribe())
    }
}

struct Worker {
    session: Option<Session>,
    catalog: Option<hx_catalog::Catalog>,
    live: broadcast::Sender<Live>,
    last_poll: Instant,
    last_connect: Option<Instant>,
    /// Failed opens in a row of a pedal that is present.
    open_failures: u32,
    /// A chain re-read is due: (first notification, latest one, strongest cause).
    reload: Option<(Instant, Instant, Cause)>,
    /// Last chain published, to send only real changes.
    shown: Option<String>,
    /// The loaded preset as last read, patched by bypass and parameter
    /// notifications. Those carry the new value; a re-read right after a
    /// change can still return the old document (TonePush GUI, `present`).
    doc: Option<Doc>,
    /// Re-reads refused in a row (a pedal mid-load refuses for a moment).
    reload_retries: u32,
}

struct Doc {
    preset: hx_proto::Preset,
    /// `preset_info`: setlist, index, name. `None` if the pedal refused.
    info: Option<(i64, i64, String)>,
}

/// What one notification does to the cached document.
#[derive(Debug, PartialEq)]
enum Patch {
    /// Slot `block` switched on or off.
    Enabled { block: usize, on: bool },
    /// Parameter `index` of slot `block`'s primary model took `value`.
    Param {
        block: usize,
        index: usize,
        value: f32,
    },
}

impl Worker {
    fn new(live: broadcast::Sender<Live>) -> Self {
        let catalog = match hx_catalog::Catalog::load() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("no HX Edit catalog, models stay numbered: {e}");
                None
            }
        };
        Worker {
            session: None,
            catalog,
            live,
            last_poll: Instant::now(),
            last_connect: None,
            open_failures: 0,
            reload: None,
            shown: None,
            doc: None,
            reload_retries: 0,
        }
    }

    fn run(mut self, jobs: mpsc::Receiver<Job>) {
        loop {
            let wait = self.next_wake().saturating_duration_since(Instant::now());
            match jobs.recv_timeout(wait) {
                Ok(Job::Run(cmd, reply)) => {
                    let result = self.command(&cmd);
                    let _ = reply.send(result);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Job::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if self.session.take().is_some() {
                        tracing::info!("USB session closed");
                    }
                    return;
                }
            }
            self.tick();
        }
    }

    fn next_wake(&self) -> Instant {
        let poll = match self.session {
            Some(_) => self.last_poll + POLL,
            None => self
                .last_connect
                .map_or(Instant::now(), |t| t + self.reconnect_delay()),
        };
        match self.reload {
            Some((first, latest, _)) => poll.min((latest + SETTLE).min(first + SETTLE_MAX)),
            None => poll,
        }
    }

    /// Housekeeping between commands: reconnect, poll, re-read.
    fn tick(&mut self) {
        let now = Instant::now();
        if self.session.is_none() {
            if self
                .last_connect
                .is_none_or(|t| now >= t + self.reconnect_delay())
            {
                // Quietly: a missing pedal is the normal state until plugged in.
                let _ = self.connect();
            }
            return;
        }
        if now >= self.last_poll + POLL {
            self.poll();
        }
        if let Some((first, latest, cause)) = self.reload {
            if now >= latest + SETTLE || now >= first + SETTLE_MAX {
                self.reload = None;
                self.publish_chain(cause);
            }
        }
    }

    fn reconnect_delay(&self) -> Duration {
        RECONNECT
            .saturating_mul(1 << self.open_failures.min(4))
            .min(RECONNECT_MAX)
    }

    fn connect(&mut self) -> Result<&mut Session> {
        if self.session.is_none() {
            self.last_connect = Some(Instant::now());
            let found = hx_usb::list().map_err(|e| DeviceError::Failed(e.to_string()))?;
            let device = found
                .first()
                .ok_or_else(|| DeviceError::NotConnected(NO_DEVICE.into()))?;
            let session = device.open().map_err(|e| {
                self.open_failures += 1;
                if self.open_failures == 1 {
                    tracing::warn!("cannot open the pedal: {e}");
                }
                DeviceError::Failed(e.to_string())
            })?;
            self.open_failures = 0;
            tracing::info!(device = session.profile.name, "USB session open");
            self.session = Some(session);
            self.last_poll = Instant::now();
            self.shown = None;
            let _ = self.live.send(Live::Connected(true));
        }
        Ok(self.session.as_mut().expect("just opened"))
    }

    /// Drop a session after an error that leaves its state unknown.
    fn check<T>(&mut self, result: hx_usb::Result<T>) -> Result<T> {
        result.map_err(|e| {
            if e.loses_session() && self.session.take().is_some() {
                tracing::warn!("USB session lost: {e}");
                self.reload = None;
                self.doc = None;
                // Try again at once; failed opens back off by themselves.
                self.last_connect = None;
                let _ = self.live.send(Live::Connected(false));
            }
            DeviceError::Failed(e.to_string())
        })
    }

    fn poll(&mut self) {
        self.last_poll = Instant::now();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let polled = session.poll_notifications();
        let events = match self.check(polled) {
            Ok(events) => events,
            Err(_) => return,
        };
        let mut patched = false;
        for (event, args) in &events {
            let Some(cause) = notice(*event, args) else {
                continue;
            };
            let patch = patch(*event, args);
            tracing::debug!(event, ?args, ?cause, ?patch, "device notification");
            match patch {
                Some(p) if self.apply(&p) => patched = true,
                _ => self.want_reload(cause),
            }
        }
        if patched {
            self.publish_doc(Cause::Edit);
        }
        if let Some(session) = self.session.as_mut() {
            let kept = session.keepalive();
            let _ = self.check(kept);
        }
    }

    fn want_reload(&mut self, cause: Cause) {
        let now = Instant::now();
        self.reload = Some(match self.reload {
            Some((first, _, was)) => (first, now, was.max(cause)),
            None => (now, now, cause),
        });
    }

    /// Change the cached document. False if there is none or it does not
    /// have that block or parameter: then the caller re-reads instead.
    fn apply(&mut self, patch: &Patch) -> bool {
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        match *patch {
            Patch::Enabled { block, on } => match doc.preset.slots.get_mut(block) {
                Some(slot) if slot.model.is_some() => {
                    slot.enabled = on;
                    true
                }
                _ => false,
            },
            Patch::Param {
                block,
                index,
                value,
            } => match doc
                .preset
                .slots
                .get_mut(block)
                .and_then(|slot| slot.values.get_mut(index))
            {
                Some(v) => {
                    *v = value;
                    true
                }
                None => false,
            },
        }
    }

    /// Publish the cached document, if it changed.
    fn publish_doc(&mut self, cause: Cause) {
        let Some(doc) = &self.doc else {
            return;
        };
        let text = self.render(doc);
        self.publish(text, cause);
    }

    fn publish(&mut self, text: String, cause: Cause) {
        // A loaded preset is news even if it looks the same (two `New Preset`s).
        if cause != Cause::Preset && self.shown.as_deref() == Some(text.as_str()) {
            return;
        }
        tracing::debug!(
            ?cause,
            listeners = self.live.receiver_count(),
            "live update"
        );
        self.shown = Some(text.clone());
        let _ = self.live.send(Live::Chain { text, cause });
    }

    fn publish_chain(&mut self, cause: Cause) {
        let text = match self.chain() {
            Ok(text) => {
                self.reload_retries = 0;
                text
            }
            Err(e) => {
                tracing::debug!("chain re-read failed: {e}");
                if self.session.is_some() && self.reload_retries < 3 {
                    self.reload_retries += 1;
                    self.want_reload(cause);
                }
                return;
            }
        };
        self.publish(text, cause);
    }

    fn command(&mut self, cmd: &Command) -> Result<String> {
        if *cmd == Command::List {
            return list();
        }
        self.connect()?;
        if cmd.is_write() {
            tracing::info!(?cmd, "device write");
        }
        let out = match cmd {
            Command::List => unreachable!(),
            Command::Info => self.info(),
            Command::Setlists => self.setlists(),
            Command::Presets { setlist } => self.presets(*setlist),
            Command::Preset => self.preset(),
            Command::Chain => self.chain(),
            Command::Select { setlist, index } => self.select(*setlist, *index),
            Command::Enable { block, on } => self.enable(*block, *on),
            Command::Set {
                block,
                param,
                value,
            } => self.set(*block, *param, value),
            Command::Rename {
                setlist,
                index,
                name,
            } => self.rename(*setlist, *index, name),
            Command::Save => self.save(),
        };
        if *cmd == Command::Chain {
            if let Ok(text) = &out {
                self.shown = Some(text.clone());
            }
        }
        // Some writes swallow their own notifications (a select reads them
        // while it waits), so every write publishes its result itself.
        // Bypass and parameter writes patched `doc` already.
        if out.is_ok() {
            match cmd {
                Command::Select { .. } => self.want_reload(Cause::Preset),
                Command::Enable { .. } | Command::Set { .. } => self.publish_doc(Cause::Edit),
                Command::Rename { .. } | Command::Save => self.want_reload(Cause::Other),
                _ => {}
            }
        }
        out
    }

    /// Run `f` on the open session, dropping the session on transport loss.
    fn with<T>(&mut self, f: impl FnOnce(&mut Session) -> hx_usb::Result<T>) -> Result<T> {
        let session = self.connect()?;
        let result = f(session);
        self.check(result)
    }

    fn label(&self, index: i64) -> String {
        self.session
            .as_ref()
            .map_or_else(|| index.to_string(), |s| s.profile.slot_label(index))
    }

    /// `show_info`.
    fn info(&mut self) -> Result<String> {
        let preset = self.with(|s| s.read_preset())?;
        let profile = self.session.as_ref().expect("connected").profile;
        Ok(format!(
            "{} ({} presets)\ndevice id: {:#010x}\nfirmware:  {}\nbuild:     {}\n",
            profile.name,
            profile.presets,
            profile.device_id,
            preset.firmware().unwrap_or_else(|| "?".into()),
            preset.build().unwrap_or("?"),
        ))
    }

    fn setlists(&mut self) -> Result<String> {
        let names = self.with(|s| s.setlists())?;
        let mut out = String::new();
        for (i, name) in names.iter().enumerate() {
            let _ = writeln!(out, "{i}  {name}");
        }
        Ok(out)
    }

    /// `list_presets`.
    fn presets(&mut self, setlist: u32) -> Result<String> {
        let setlist = i64::from(setlist);
        let (loaded_setlist, loaded, _) = self.with(|s| s.preset_info())?;
        let names = self.with(|s| s.presets(setlist))?;
        let mut out = String::new();
        for (index, name) in names.iter().enumerate() {
            let index = index as i64;
            let marker = if (loaded_setlist, loaded) == (setlist, index) {
                "<- loaded"
            } else {
                ""
            };
            let _ = writeln!(out, "{} {:<24} {marker}", self.label(index), name);
        }
        Ok(out)
    }

    /// `show_preset`, summary form.
    fn preset(&mut self) -> Result<String> {
        let preset = self.with(|s| s.read_preset())?;
        let (setlist, index, name) = self.with(|s| s.preset_info())?;
        Ok(format!(
            "preset:   {} {name}  (setlist {setlist}, index {index})\nfirmware: {}\nbuild:    {}\nsections: {} bytes\n",
            self.label(index),
            preset.firmware().unwrap_or_else(|| "?".into()),
            preset.build().unwrap_or("?"),
            preset.sections.len(),
        ))
    }

    /// Read the loaded preset into `doc` and render it.
    fn chain(&mut self) -> Result<String> {
        let preset = self.with(|s| s.read_preset())?;
        // A refusal here (a pedal mid-load) only loses the header.
        let info = match self.with(|s| s.preset_info()) {
            Ok(info) => Some(info),
            Err(_) if self.session.is_some() => None,
            Err(e) => return Err(e),
        };
        let doc = Doc { preset, info };
        let text = self.render(&doc);
        self.doc = Some(doc);
        Ok(text)
    }

    /// `show_chain`.
    fn render(&self, doc: &Doc) -> String {
        let (preset, catalog) = (&doc.preset, self.catalog.as_ref());
        let mut out = String::new();
        if let Some((_, index, name)) = &doc.info {
            let index = *index;
            let _ = write!(out, "{} {name}", self.label(index));
        }
        if let Some(tempo) = preset.tempo() {
            let _ = write!(out, "   {tempo:.1} BPM");
        }
        out.push('\n');
        let snapshots = preset.snapshots();
        if !snapshots.is_empty() {
            let _ = writeln!(out, "snapshots: {}", snapshots.join(", "));
        }
        out.push('\n');
        let model_name = |model: u32| {
            catalog
                .and_then(|c| c.model_number(model))
                .map_or_else(|| format!("model {model}"), |m| m.name.clone())
        };
        for (position, block) in preset.blocks() {
            let model = block.model.unwrap_or_default();
            let _ = writeln!(
                out,
                "{:>2}. {:<24} {}",
                position + 1,
                model_name(model),
                if block.enabled { "" } else { "(bypassed)" },
            );
            params(&mut out, catalog, model, &block.values);
            if let Some(cab) = block.paired {
                let _ = writeln!(out, "    + {}", model_name(cab));
                params(&mut out, catalog, cab, &block.paired_values);
            }
        }
        out
    }

    fn select(&mut self, setlist: u32, index: u32) -> Result<String> {
        let (setlist, index) = (i64::from(setlist), i64::from(index));
        self.with(|s| s.select_preset(setlist, index))?;
        Ok(format!(
            "selected {} (setlist {setlist})\n",
            self.label(index)
        ))
    }

    fn enable(&mut self, block: u32, on: bool) -> Result<String> {
        self.with(|s| s.set_enabled(i64::from(block) - 1, on))?;
        let slot = block as usize - 1;
        if !self.apply(&Patch::Enabled { block: slot, on }) {
            self.doc = None;
        }
        Ok(format!(
            "block {block} {}\n",
            if on { "engaged" } else { "bypassed" }
        ))
    }

    /// `set_param`, with the parameter given by index.
    fn set(&mut self, block: u32, param: u32, value: &str) -> Result<String> {
        let preset = self.with(|s| s.read_preset())?;
        let model = preset
            .slots
            .get((block as usize).wrapping_sub(1))
            .and_then(|s| s.model)
            .ok_or_else(|| {
                DeviceError::Failed(format!(
                    "no block at position {block}; try `tonepush chain`"
                ))
            })?;
        let slot = i64::from(block) - 1;
        let index = i64::from(param);
        let described = self
            .catalog
            .as_ref()
            .and_then(|c| c.param(model, param as usize).map(|p| (c, p)));
        let Some((catalog, described)) = described else {
            // Without the catalog the number goes through untouched.
            let native: f32 = value
                .trim()
                .parse()
                .ok()
                .filter(|v: &f32| v.is_finite())
                .ok_or_else(|| DeviceError::Failed(format!("{value:?} is not a number")))?;
            self.with(|s| s.set_param(slot, index, Value::F32(native)))?;
            self.patch_param(slot, param, native);
            return Ok(format!("block {block}: parameter {param} = {native}\n"));
        };
        let native = catalog.parse(described, value).ok_or_else(|| {
            DeviceError::Failed(format!("{value:?} is not a valid {}", described.name))
        })?;
        let (name, shown, wire) = (
            described.name.clone(),
            catalog.format(described, native),
            described.kind.wire(native),
        );
        self.with(|s| s.set_param(slot, index, wire))?;
        self.patch_param(slot, param, native);
        Ok(format!("block {block}: {name} = {shown}\n"))
    }

    /// Mirror our own parameter write into `doc`; forget it if it cannot be.
    fn patch_param(&mut self, slot: i64, param: u32, value: f32) {
        let patch = Patch::Param {
            block: slot as usize,
            index: param as usize,
            value,
        };
        if !self.apply(&patch) {
            self.doc = None;
        }
    }

    fn rename(&mut self, setlist: u32, index: u32, name: &str) -> Result<String> {
        let (setlist, index) = (i64::from(setlist), i64::from(index));
        self.with(|s| s.rename_preset(setlist, index, name))?;
        Ok(format!("{} renamed to {name}\n", self.label(index)))
    }

    /// `save_preset` with no target: the loaded slot, its current name.
    fn save(&mut self) -> Result<String> {
        let (setlist, index, name) = self.with(|s| s.preset_info())?;
        self.with(|s| s.save_preset(setlist, index, &name))?;
        Ok(format!(
            "saved to {} in setlist {setlist} as {name:?}\n",
            self.label(index)
        ))
    }
}

/// `list_devices`, HX part.
fn list() -> Result<String> {
    let devices = hx_usb::list().map_err(|e| DeviceError::Failed(e.to_string()))?;
    if devices.is_empty() {
        return Ok("no supported devices found\n".into());
    }
    let mut out = String::new();
    for d in &devices {
        let _ = writeln!(
            out,
            "{} (pid {:#06x}) serial {} - {} presets",
            d.profile.name,
            d.profile.product_id,
            d.serial.as_deref().unwrap_or("?"),
            d.profile.presets
        );
    }
    Ok(out)
}

/// `show_params`.
fn params(out: &mut String, catalog: Option<&hx_catalog::Catalog>, model: u32, values: &[f32]) {
    for (i, value) in values.iter().enumerate() {
        let _ = match catalog.and_then(|c| c.param(model, i).map(|p| (c, p))) {
            Some((c, p)) => writeln!(out, "      {:<18} {}", p.name, c.format(p, *value)),
            None => writeln!(out, "      param {i:<12} {value}"),
        };
    }
}

/// The change a bypass or parameter notification carries, if it is one this
/// code can apply without re-reading.
///
/// - 49/(5,17) `{98: slot, 59: enabled}`: bypass changed (also from a
///   footswitch, docs/hx-effects-notifications.md).
/// - 30/(6,20) `{98: slot, 29, 26: path, 28: index, 119: value}`: parameter
///   changed, reporting the new value (TonePush opcodes.md 4.3). `26` is not
///   pinned down (primary/cab model or path); only 0 is applied, anything
///   else re-reads.
fn patch(event: i64, args: &Value) -> Option<Patch> {
    use hx_proto::rpc::key;
    let inner = args.get(key::EVENT_ARGS)?;
    let int = |k| inner.get(k).and_then(Value::as_i64);
    let block = usize::try_from(int(key::BLOCK)?).ok()?;
    match (event, args.get(121).and_then(Value::as_i64)) {
        (49, Some(17)) => Some(Patch::Enabled {
            block,
            on: inner.get(key::ENABLED)?.as_bool()?,
        }),
        (30, _) if int(key::PATH)? == 0 => Some(Patch::Param {
            block,
            index: usize::try_from(int(28)?).ok()?,
            value: inner.get(119)?.as_f32().filter(|v| v.is_finite())?,
        }),
        _ => None,
    }
}

/// What a notification means for the chain on screen, or `None` for noise.
///
/// Ids from TonePush `docs/_reference/opcodes.md` section 3 and
/// `tonepush-gui/src/session.rs` (`notice`). Event 49/(5,17) also fires for
/// a front-panel footswitch on the HX Effects (`docs/hx-effects-notifications.md`).
/// Anything not known to be noise re-reads the chain: a re-read inside the
/// session is cheap, a missed change is not.
fn notice(event: i64, args: &Value) -> Option<Cause> {
    use hx_proto::rpc::key;
    const TOPIC: i64 = 68;
    const SUBTYPE: i64 = 121;
    const LOADED_SLOT: i64 = 28;
    let field = |k| args.get(k).and_then(Value::as_i64);
    let inner = args.get(key::EVENT_ARGS).unwrap_or(args);
    match event {
        // Preset load starts / ends; the loaded slot changed.
        4 | 8 => Some(Cause::Preset),
        22 if inner.get(key::OBJECT_ID).and_then(Value::as_i64) == Some(LOADED_SLOT) => {
            Some(Cause::Preset)
        }
        // The steady tick, every ~75 ms to 1 s.
        22 if field(TOPIC) == Some(10) && field(SUBTYPE) == Some(27) => None,
        // Parameter changed; bypass changed or slot rebuilt.
        30 | 49 => Some(Cause::Edit),
        // Completions and the selected block: nothing on screen changes.
        1 | 20 | 21 | 39 => None,
        _ => Some(Cause::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hx_proto::msgmap;

    /// `{82: 0, 68: 10, 121: 27, 106: nil}` and friends, as `tonepush watch`
    /// printed them on the HX Effects (docs/hx-effects-notifications.md).
    fn event(topic: i64, subtype: i64, inner: Value) -> Value {
        msgmap! { 82 => Value::Int(0), 68 => Value::Int(topic), 121 => Value::Int(subtype), 106 => inner }
    }

    #[test]
    fn recorded_footswitch_events() {
        assert_eq!(notice(22, &event(10, 27, Value::Nil)), None);
        let bypass = event(
            5,
            17,
            msgmap! { 98 => Value::Int(2), 59 => Value::Bool(true) },
        );
        assert_eq!(notice(49, &bypass), Some(Cause::Edit));
        let cursor = event(3, 19, msgmap! { 98 => Value::Int(2), 26 => Value::Int(0) });
        assert_eq!(notice(39, &cursor), None);
    }

    #[test]
    fn patches_from_events() {
        let bypass = event(
            5,
            17,
            msgmap! { 98 => Value::Int(2), 59 => Value::Bool(false) },
        );
        assert_eq!(
            patch(49, &bypass),
            Some(Patch::Enabled {
                block: 2,
                on: false
            })
        );
        // Opcodes.md 4.3, verbatim apart from the elided keys.
        let knob = event(
            6,
            20,
            msgmap! { 98 => Value::Int(6), 29 => Value::Bool(true), 26 => Value::Int(0), 28 => Value::Int(0), 119 => Value::F32(0.05) },
        );
        assert_eq!(
            patch(30, &knob),
            Some(Patch::Param {
                block: 6,
                index: 0,
                value: 0.05
            })
        );
        let cab = event(
            6,
            20,
            msgmap! { 98 => Value::Int(6), 26 => Value::Int(1), 28 => Value::Int(0), 119 => Value::F32(0.5) },
        );
        assert_eq!(patch(30, &cab), None);
        // Slot rebuilt (49/10) is not a bypass change.
        assert_eq!(
            patch(49, &event(5, 10, msgmap! { 98 => Value::Int(1) })),
            None
        );
    }

    #[test]
    fn documented_events() {
        let load = msgmap! { 107 => Value::Int(0), 108 => Value::Int(7) };
        assert_eq!(notice(8, &load), Some(Cause::Preset));
        let slot = event(
            9,
            25,
            msgmap! { 118 => Value::Int(28), 119 => Value::Int(7) },
        );
        assert_eq!(notice(22, &slot), Some(Cause::Preset));
        assert_eq!(notice(30, &event(6, 20, Value::Nil)), Some(Cause::Edit));
        assert_eq!(notice(46, &Value::Nil), Some(Cause::Other));
        assert_eq!(notice(77, &Value::Nil), Some(Cause::Other));
    }
}
