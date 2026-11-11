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
use hx_proto::preset::Kind;
use hx_usb::Session;
use tokio::sync::{broadcast, oneshot};

use crate::controls::{self, CategoryInfo, ModelGroup, ModelInfo};
use crate::device::{
    BlockNatives, Cause, ChainExtra, Command, Device, DeviceError, Live, Natives, Result,
};
use crate::parse;
use crate::routing;

/// How often an idle session reads notifications and sends keep-alives.
/// TonePush uses 700 ms; faster polling wedged a device during its development.
const POLL: Duration = Duration::from_millis(700);
/// Each poll reads until the pedal has nothing left, for at most this long.
/// `poll_notifications` takes one USB transfer (512 bytes, about 14
/// notifications), and the pedal can queue more than that between polls. A
/// backlog delays every change and, once the pedal's buffer is full, stops it
/// (PROTOCOL.md: keep reading so notifications never back up).
const DRAIN_BUDGET: Duration = Duration::from_millis(200);
/// How often the debug log reports notification counts.
const STATS_EVERY: Duration = Duration::from_secs(10);
/// Wait this long after a notification for more before re-reading the chain:
/// one footswitch press arrives as several notifications.
const SETTLE: Duration = Duration::from_millis(250);
/// Re-read at the latest this long after the first notification of a burst.
const SETTLE_MAX: Duration = Duration::from_secs(1);
/// Least time between a session ending (here or in another process) or the
/// pedal appearing, and the next open. On an HX Effects 3.80 every open 0-4 s
/// after one of those left the events channel deaf, often after a failed
/// first attempt; every open 15 s or more after worked (2026-10-14, PATCHES.md 2b).
const SETTLE_BEFORE_OPEN: Duration = Duration::from_secs(20);
/// After an open whose events channel did not answer, close it this much
/// later and open again after [`SETTLE_BEFORE_OPEN`]. Until then the session
/// still serves requests.
const REOPEN_DEAF: Duration = Duration::from_secs(20);
/// Give up reopening for live updates after this many deaf opens in a row.
const DEAF_RETRIES: u32 = 3;
/// With no session, look for the pedal this often (only USB enumeration).
const RECONNECT: Duration = Duration::from_secs(5);
/// After a failed open of a pedal that is present, wait longer each time, up
/// to this. A wedged pedal refuses every session until its power is pulled;
/// there is no point knocking every 5 s.
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// Least time between two slider writes (op30). Clients send at most one
/// every 50-100 ms; this holds when several iPads drag at once. HX Edit sends
/// one op30 per move step (opcodes.md 4.3), but this device wedges when
/// hammered (PROTOCOL.md), so stay well below that.
const PARAM_GAP: Duration = Duration::from_millis(40);

/// How often an idle session reads the chain again to check the cached copy
/// against the pedal: a notification lost on the way leaves the cache wrong
/// until then. Six footswitch reads and two document reads, once a minute.
const AUDIT_EVERY: Duration = Duration::from_secs(60);
/// Idle means no command for this long, so a check never delays a drag.
const AUDIT_IDLE: Duration = Duration::from_secs(10);

/// Reads of the document a busy pedal may refuse before one counts as failed,
/// and the wait between them (TonePush GUI, `read_settled`).
const SETTLED_READS: u32 = 3;
const SETTLED_READ_GAP: Duration = Duration::from_millis(300);

/// Reads of the document after adding or removing a block, until it shows.
const SLOT_READS: u32 = 5;
const SLOT_READ_GAP: Duration = Duration::from_millis(200);

const NO_DEVICE: &str = "no HX device found - check the USB cable";

type Reply = oneshot::Sender<Result<String>>;

enum Job {
    Run(Command, Reply),
    /// `chain` plus the native values behind it.
    Chain(oneshot::Sender<Result<(String, ChainExtra)>>),
    /// Close the session cleanly and end the worker.
    Stop,
}

pub struct UsbDevice {
    jobs: mpsc::Sender<Job>,
    live: broadcast::Sender<Live>,
    /// HX Edit's catalog, shared with the worker; metadata is read here so
    /// `/api/models` never waits behind a USB command.
    catalog: Option<Arc<hx_catalog::Catalog>>,
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
        let catalog = match hx_catalog::Catalog::load() {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                tracing::warn!("no HX Edit catalog, models stay numbered, no sliders: {e}");
                None
            }
        };
        let shared = catalog.clone();
        let worker = thread::Builder::new()
            .name("hx-usb".into())
            .spawn(move || Worker::new(publish, shared).run(rx))
            .expect("spawn the USB worker thread");
        UsbDevice {
            jobs,
            live,
            catalog,
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

    async fn chain(&self) -> Result<(String, ChainExtra)> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .send(Job::Chain(reply))
            .map_err(|_| DeviceError::Spawn("USB worker stopped".into()))?;
        answer
            .await
            .map_err(|_| DeviceError::Spawn("USB worker stopped".into()))?
    }

    fn model(&self, id: u32) -> Option<ModelInfo> {
        controls::from_catalog(self.catalog.as_deref()?, id)
    }

    fn category(&self, model_id: Option<u32>, name: &str) -> Option<CategoryInfo> {
        controls::category(self.catalog.as_deref()?, model_id, name)
    }

    fn catalog(&self) -> Option<Vec<ModelGroup>> {
        Some(controls::browser(self.catalog.as_deref()?))
    }
}

struct Worker {
    session: Option<Session>,
    catalog: Option<Arc<hx_catalog::Catalog>>,
    live: broadcast::Sender<Live>,
    last_poll: Instant,
    last_connect: Option<Instant>,
    /// Failed opens in a row of a pedal that is present.
    open_failures: u32,
    /// A chain re-read is due: (first notification, latest one, strongest cause).
    reload: Option<(Instant, Instant, Cause)>,
    /// Last chain published or read by a client, to send only real changes.
    shown: Option<(String, ChainExtra)>,
    /// The loaded preset as last read, patched by bypass and parameter
    /// notifications. Those carry the new value; a re-read right after a
    /// change can still return the old document (TonePush GUI, `present`).
    doc: Option<Doc>,
    /// Notification counts for the debug log: (since, polls, transfers, events).
    stats: (Instant, u32, u32, u32),
    /// Re-reads refused in a row (a pedal mid-load refuses for a moment).
    reload_retries: u32,
    /// When the last slider write went out, for [`PARAM_GAP`].
    last_param_write: Option<Instant>,
    /// The session's events channel did not answer: open it again then.
    reopen: Option<Instant>,
    /// Opens in a row whose events channel did not answer.
    deaf_opens: u32,
    /// `Session::events_skipped` when last looked at.
    skipped: u64,
    /// No open before this: see [`SETTLE_BEFORE_OPEN`].
    not_before: Instant,
    /// The last look for the pedal found none.
    absent: bool,
    /// When the last command ran, for [`AUDIT_IDLE`].
    last_job: Instant,
    /// When the cached document was last checked against the pedal.
    last_audit: Instant,
    /// A session just opened: compare the edit buffer with the stored
    /// preset ([`Worker::check_stored`]).
    check_stored: bool,
}

struct Doc {
    preset: hx_proto::Preset,
    /// `preset_info`: setlist, index, name. `None` if the pedal refused.
    info: Option<(i64, i64, String)>,
    /// What each footswitch carries (opcode 33). Not in the document: a
    /// bypass on a footswitch lives in the switch's own configuration
    /// (PROTOCOL.md). `None` if the pedal refused.
    switches: Option<Vec<parse::Footswitch>>,
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
    fn new(live: broadcast::Sender<Live>, catalog: Option<Arc<hx_catalog::Catalog>>) -> Self {
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
            stats: (Instant::now(), 0, 0, 0),
            reload_retries: 0,
            last_param_write: None,
            reopen: None,
            deaf_opens: 0,
            skipped: 0,
            // A restart: the previous process's session may have just ended.
            not_before: Instant::now() + SETTLE_BEFORE_OPEN,
            absent: false,
            last_job: Instant::now(),
            last_audit: Instant::now(),
            check_stored: false,
        }
    }

    fn run(mut self, jobs: mpsc::Receiver<Job>) {
        // A job read while coalescing slider writes, to run next.
        let mut queued: Option<Job> = None;
        loop {
            let wait = self.next_wake().saturating_duration_since(Instant::now());
            let next = match queued.take() {
                Some(job) => Ok(job),
                None => jobs.recv_timeout(wait),
            };
            match next {
                Ok(Job::Run(cmd, reply)) => {
                    let (cmd, replies, after) = coalesce(cmd, reply, &jobs);
                    queued = after;
                    let result = self.command(&cmd);
                    self.last_job = Instant::now();
                    for reply in replies {
                        let _ = reply.send(result.clone());
                    }
                }
                Ok(Job::Chain(reply)) => {
                    let read = match self.cached_chain() {
                        Some(text) => Ok(text),
                        None => self.command(&Command::Chain),
                    };
                    let result = read.map(|text| {
                        let extra = self.extra();
                        // The client shows this now: publish only a change from it.
                        self.shown = Some((text.clone(), extra.clone()));
                        (text, extra)
                    });
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
            Some(_) => self
                .reopen
                .map_or(self.last_poll + POLL, |t| t.min(self.last_poll + POLL)),
            None => self
                .last_connect
                .map_or(Instant::now(), |t| t + self.reconnect_delay())
                .max(self.not_before),
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
        if self.reopen.is_some_and(|t| now >= t) {
            self.reopen = None;
            tracing::info!(
                "closing the USB session to open it again for live updates in {} s",
                SETTLE_BEFORE_OPEN.as_secs()
            );
            self.close();
            let _ = self.live.send(Live::Connected(false));
            return;
        }
        if self.check_stored {
            self.check_stored = false;
            self.check_stored();
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
        if self.reload.is_none() && audit_due(now, self.last_audit, self.last_job) {
            self.audit();
        }
    }

    /// Read the chain again and push it if it differs from what clients were
    /// last sent or read: catches a change whose notification got lost.
    fn audit(&mut self) {
        self.last_audit = Instant::now();
        if self.session.is_none() || self.doc.is_none() {
            return;
        }
        match self.reread(Cause::Other) {
            Ok(text) => {
                if self.publish(text, Cause::Other) {
                    tracing::info!(
                        "periodic check: the pedal changed without a notification; sent"
                    );
                }
            }
            Err(e) => tracing::debug!("periodic check failed: {e}"),
        }
    }

    /// After a session opens (a service restart, a reconnect): if the edit
    /// buffer differs from the stored copy of the loaded preset, it holds
    /// edits nothing here saw, so publish it as an [`Cause::Edit`] and the
    /// server marks it unsaved. Without this a restart forgot them, and a
    /// load dropped them unasked (HX Effects, 2026-10-22). If it matches,
    /// publish it as a [`Cause::Preset`] instead, so a flag left from before
    /// clears: a pedal restarted under a running service drops its edits but
    /// kept `unsaved` on (HX Effects, 2026-10-25). The stored copy comes by
    /// opcode 4 (hx-usb `read_preset_at`), which loads nothing.
    fn check_stored(&mut self) {
        let check = (|| -> Result<Option<Vec<String>>> {
            self.read_doc()?;
            let Some((setlist, index, _)) = self.doc.as_ref().and_then(|d| d.info.clone()) else {
                return Ok(None);
            };
            let stored = self.with(|s| s.read_preset_at(setlist, index))?;
            let controls = self.controls();
            let edit = &self.doc.as_ref().expect("just read").preset;
            Ok(Some(match stored {
                Some(stored) => unsaved_edits(&stored, edit, controls),
                None => vec!["(the slot is empty)".to_owned()],
            }))
        })();
        let paths = match check {
            Ok(Some(paths)) => paths,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("could not compare with the stored preset: {e}");
                return;
            }
        };
        let cause = stored_cause(&paths);
        if cause == Cause::Edit {
            tracing::info!(
                differs = paths
                    .iter()
                    .take(20)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                "the loaded preset holds edits not saved on the pedal; marked unsaved"
            );
        }
        if self.read_switches().is_ok() {
            let doc = self.doc.as_ref().expect("just read");
            let text = self.render(doc);
            let extra = self.extra();
            self.shown = Some((text.clone(), extra.clone()));
            let _ = self.live.send(Live::Chain { text, cause, extra });
        }
    }

    /// The cached chain, when it needs no read: the pedal's notifications
    /// reach this session (they keep the cache current), no re-read is due,
    /// and the header and footswitches are known. `None`: read the pedal.
    fn cached_chain(&self) -> Option<String> {
        let notified = self.session.as_ref().is_some_and(Session::events_answered);
        let doc = self
            .doc
            .as_ref()
            .filter(|d| d.info.is_some() && d.switches.is_some())?;
        (notified && self.reload.is_none()).then(|| self.render(doc))
    }

    fn reconnect_delay(&self) -> Duration {
        RECONNECT
            .saturating_mul(1 << self.open_failures.min(4))
            .min(RECONNECT_MAX)
    }

    fn connect(&mut self) -> Result<&mut Session> {
        if self.session.is_none() {
            let now = Instant::now();
            if now < self.not_before {
                return Err(DeviceError::NotConnected(format!(
                    "waiting {} s for the pedal to settle before connecting",
                    (self.not_before - now).as_secs() + 1
                )));
            }
            self.last_connect = Some(now);
            let found = hx_usb::list().map_err(|e| DeviceError::Failed(e.to_string()))?;
            let Some(device) = found.first() else {
                self.absent = true;
                return Err(DeviceError::NotConnected(NO_DEVICE.into()));
            };
            if self.absent {
                // Just plugged in or powered up: let it finish booting.
                self.absent = false;
                self.not_before = now + SETTLE_BEFORE_OPEN;
                tracing::info!(
                    "pedal found; connecting in {} s",
                    SETTLE_BEFORE_OPEN.as_secs()
                );
                return Err(DeviceError::NotConnected(format!(
                    "pedal found; connecting in {} s",
                    SETTLE_BEFORE_OPEN.as_secs()
                )));
            }
            let session = device.open().map_err(|e| {
                self.open_failures += 1;
                if self.open_failures == 1 {
                    tracing::warn!("cannot open the pedal: {e}");
                }
                DeviceError::Failed(e.to_string())
            })?;
            self.open_failures = 0;
            tracing::info!(device = session.profile.name, "USB session open");
            self.check_events(&session);
            self.skipped = session.events_skipped();
            self.session = Some(session);
            self.last_poll = Instant::now();
            self.last_audit = Instant::now();
            self.shown = None;
            self.check_stored = true;
            let _ = self.live.send(Live::Connected(true));
        }
        Ok(self.session.as_mut().expect("just opened"))
    }

    /// A session whose events channel did not answer works, but no live
    /// update reaches it. The HX Effects does that when a session opens too
    /// soon after another one ended or after it powered up (PATCHES.md 2b).
    /// Keep this session for requests for a while, then close it and open
    /// again after [`SETTLE_BEFORE_OPEN`].
    fn check_events(&mut self, session: &Session) {
        self.reopen = None;
        if session.events_answered() {
            self.deaf_opens = 0;
            return;
        }
        self.deaf_opens += 1;
        if self.deaf_opens > DEAF_RETRIES {
            tracing::warn!(
                "the pedal's events channel still does not answer after {DEAF_RETRIES} reopens; \
                 live updates are off until the service restarts"
            );
            return;
        }
        tracing::warn!(
            "the pedal's events channel did not answer, so live updates will not arrive; \
             closing in {} s and opening again {} s later",
            REOPEN_DEAF.as_secs(),
            SETTLE_BEFORE_OPEN.as_secs()
        );
        self.reopen = Some(Instant::now() + REOPEN_DEAF);
    }

    /// End the session on purpose, keeping nothing that belonged to it.
    fn close(&mut self) {
        self.session = None;
        self.reload = None;
        self.doc = None;
        self.not_before = Instant::now() + SETTLE_BEFORE_OPEN;
    }

    /// Drop a session after an error that leaves its state unknown.
    fn check<T>(&mut self, result: hx_usb::Result<T>) -> Result<T> {
        result.map_err(|e| {
            if e.loses_session() && self.session.take().is_some() {
                tracing::warn!("USB session lost: {e}");
                self.reload = None;
                self.doc = None;
                // Try again once the pedal has settled; failed opens back
                // off by themselves.
                self.last_connect = None;
                self.not_before = Instant::now() + SETTLE_BEFORE_OPEN;
                let _ = self.live.send(Live::Connected(false));
            }
            DeviceError::Failed(e.to_string())
        })
    }

    fn poll(&mut self) {
        self.last_poll = Instant::now();
        let deadline = self.last_poll + DRAIN_BUDGET;
        let mut events = Vec::new();
        let mut transfers = 0;
        loop {
            let Some(session) = self.session.as_mut() else {
                return;
            };
            let polled = session.poll_notifications();
            let Ok(batch) = self.check(polled) else {
                return;
            };
            transfers += 1;
            let drained = batch.is_empty();
            events.extend(batch);
            if drained || Instant::now() >= deadline {
                break;
            }
        }
        self.count(transfers, events.len());
        if let Some(session) = self.session.as_ref() {
            let skipped = session.events_skipped();
            if skipped > self.skipped {
                tracing::warn!(
                    bytes = skipped - self.skipped,
                    "the pedal sent an events frame out of order; one notification lost"
                );
                self.skipped = skipped;
            }
        }
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

    fn count(&mut self, transfers: u32, events: usize) {
        let (since, polls, total_transfers, total_events) = &mut self.stats;
        *polls += 1;
        *total_transfers += transfers;
        *total_events += events as u32;
        if since.elapsed() >= STATS_EVERY {
            tracing::debug!(
                polls = *polls,
                transfers = *total_transfers,
                notifications = *total_events,
                "pedal notifications in the last {}s",
                STATS_EVERY.as_secs()
            );
            self.stats = (Instant::now(), 0, 0, 0);
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

    /// How the connected pedal numbers its controller sources (TonePush
    /// `ControlLayout`, per device since v0.10.0). An HX Effects uses the HX
    /// Stomp's numbering, which is also the answer with no session open.
    fn controls(&self) -> hx_proto::rpc::ControlLayout {
        self.session
            .as_ref()
            .map_or(hx_proto::rpc::ControlLayout::Stomp, |s| s.profile.controls)
    }

    /// Native values of the cached document, when the catalog can describe
    /// them (no catalog, no sliders: the UI types values instead).
    fn natives(&self) -> Option<Natives> {
        self.catalog.as_ref()?;
        let doc = self.doc.as_ref()?;
        Some(Natives(
            doc.preset
                .blocks()
                .map(|(slot, block)| BlockNatives {
                    position: slot as u32 + 1,
                    model: block.model.unwrap_or_default(),
                    values: block.values.clone(),
                })
                .collect(),
        ))
    }

    /// What the cached document says beyond the `chain` text.
    fn extra(&self) -> ChainExtra {
        ChainExtra {
            natives: self.natives(),
            snapshot: self.doc.as_ref().and_then(|d| active_snapshot(&d.preset)),
            footswitches: self.doc.as_ref().and_then(|d| d.switches.clone()),
            routing: self.doc.as_ref().and_then(|d| routing_of(&d.preset)),
            assignments: self
                .doc
                .as_ref()
                .map(|d| assignments_of(&d.preset, self.controls(), self.catalog.as_deref())),
        }
    }

    /// Push the chain to live clients unless they have it already. True if sent.
    fn publish(&mut self, text: String, cause: Cause) -> bool {
        let extra = self.extra();
        // Natives compared too: a slider step can leave the text unchanged.
        // A loaded preset is news even if it looks the same (two `New Preset`s).
        let same = self
            .shown
            .as_ref()
            .is_some_and(|(t, e)| *t == text && *e == extra);
        if cause != Cause::Preset && same {
            return false;
        }
        tracing::debug!(
            ?cause,
            listeners = self.live.receiver_count(),
            "live update"
        );
        self.shown = Some((text.clone(), extra.clone()));
        let _ = self.live.send(Live::Chain { text, cause, extra });
        true
    }

    fn publish_chain(&mut self, cause: Cause) {
        let text = match self.reread(cause) {
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
            Command::SetNative {
                block,
                param,
                native,
                model,
            } => self.set_native(*block, *param, *native, *model),
            Command::Rename {
                setlist,
                index,
                name,
            } => self.rename(*setlist, *index, name),
            Command::Save => self.save(),
            Command::Snapshot { index } => self.snapshot(*index),
            Command::Tempo { bpm } => self.tempo(*bpm),
            Command::AddBlock { block, model } => self.add_block(*block, *model),
            Command::RemoveBlock { block, model } => self.remove_block(*block, *model),
            Command::Footswitch {
                block,
                switch,
                model,
            } => self.footswitch(*block, *switch, *model),
            Command::MoveBlock { block, to, model } => self.move_block(*block, *to, *model),
            Command::Junction { which, before } => self.junction(*which, *before),
            Command::Controller {
                block,
                param,
                pedal,
                model,
            } => self.controller(*block, *param, *pedal, *model),
            Command::ControllerRange {
                block,
                param,
                min,
                max,
                model,
            } => self.controller_range(*block, *param, *min, *max, *model),
        };
        // Some writes swallow their own notifications (a select reads them
        // while it waits), so every write publishes its result itself.
        // Bypass and parameter writes patched `doc` already; adding and
        // removing a block read it back.
        if out.is_ok() {
            match cmd {
                Command::Select { .. } => self.want_reload(Cause::Preset),
                Command::Enable { .. }
                | Command::Set { .. }
                | Command::SetNative { .. }
                | Command::AddBlock { .. }
                | Command::RemoveBlock { .. }
                | Command::Footswitch { .. }
                | Command::MoveBlock { .. }
                | Command::Junction { .. }
                | Command::Controller { .. }
                | Command::ControllerRange { .. } => self.publish_doc(Cause::Edit),
                // A rename changes a name and a save nothing on screen; a
                // read-back right after either races the flash commit, and a
                // burst of them once wedged a pedal into a factory reset
                // (TonePush GUI, `Cmd::Rename`). So no re-read for those.
                Command::Rename {
                    setlist,
                    index,
                    name,
                } => self.renamed(*setlist, *index, name),
                Command::Save => {}
                // A snapshot changes which blocks are on and their values:
                // read the document again rather than guess.
                Command::Snapshot { .. } => self.want_reload(Cause::Other),
                Command::Tempo { .. } => self.want_reload(Cause::Edit),
                _ => {}
            }
        }
        out
    }

    /// Run `f` on the open session, dropping the session on transport loss.
    ///
    /// hx-usb reports a request it will not send (an index out of range, a
    /// document that does not encode) as a protocol error, the kind that
    /// ends a session. One that left nothing on the wire changed nothing, so
    /// the session stays, as TonePush's GUI keeps it (`loses_session`):
    /// dropping it costs every client 20 s of reconnecting over one bad value.
    /// Transport failures and timeouts still end it.
    fn with<T>(&mut self, f: impl FnOnce(&mut Session) -> hx_usb::Result<T>) -> Result<T> {
        let session = self.connect()?;
        let before = session.channel_stats();
        let result = f(session);
        let untouched = session.channel_stats() == before;
        match result {
            Err(hx_usb::Error::Protocol(why)) if untouched => {
                tracing::debug!("request refused before it was sent: {why}");
                Err(DeviceError::Invalid(why))
            }
            result => self.check(result),
        }
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

    /// Read the loaded preset and its footswitches into `doc` and render it.
    fn chain(&mut self) -> Result<String> {
        self.read_doc()?;
        self.read_switches()?;
        Ok(self.render(self.doc.as_ref().expect("just read")))
    }

    /// [`Worker::chain`] after a notification. An edit (a knob, a bypass)
    /// leaves the footswitches as they were, so they are read again only when
    /// the slots changed (a block added or removed on the pedal), for any
    /// other cause, or when they are unknown. Keeps opcode 33 out of a
    /// front-panel knob turn.
    fn reread(&mut self, cause: Cause) -> Result<String> {
        let before = self.doc.as_ref().map(|d| slot_models(&d.preset));
        self.read_doc()?;
        let doc = self.doc.as_ref().expect("just read");
        let same_slots = before.as_ref() == Some(&slot_models(&doc.preset));
        if cause != Cause::Edit || !same_slots || doc.switches.is_none() {
            self.read_switches()?;
        }
        Ok(self.render(self.doc.as_ref().expect("just read")))
    }

    /// Read the loaded preset into `doc`, keeping the footswitches last read
    /// (for re-reads within one preset; [`Worker::chain`] reads them anew).
    fn read_doc(&mut self) -> Result<()> {
        let preset = self.read_settled()?;
        // A refusal here (a pedal mid-load) only loses the header.
        let info = match self.with(|s| s.preset_info()) {
            Ok(info) => Some(info),
            Err(_) if self.session.is_some() => None,
            Err(e) => return Err(e),
        };
        let switches = self.doc.take().and_then(|d| d.switches);
        self.doc = Some(Doc {
            preset,
            info,
            switches,
        });
        Ok(())
    }

    /// Read the loaded preset, asking again when the pedal refuses: a read
    /// that lands while the last write is still committing is refused by a
    /// busy pedal, not a dead one (TonePush GUI, `read_settled`). Before, the
    /// next write after a quick first one failed on its own read.
    fn read_settled(&mut self) -> Result<hx_proto::Preset> {
        let mut attempt = 0;
        loop {
            match self.with(|s| s.read_preset()) {
                Err(DeviceError::Failed(_))
                    if attempt + 1 < SETTLED_READS && self.session.is_some() =>
                {
                    attempt += 1;
                    thread::sleep(SETTLED_READ_GAP);
                }
                read => return read,
            }
        }
    }

    /// Read every footswitch (opcode 33, six round trips on an HX Effects)
    /// into `doc`. A refusal leaves them unknown rather than failing the
    /// chain; a lost session fails.
    ///
    /// The raw reply, decoded here: hx-usb's `read_switch` turns any entry
    /// it does not expect into a protocol error, and every error but a
    /// refusal ends the session ([`Worker::check`]). A precaution: not seen
    /// on hardware (the 2026-10-15 drops were hx-usb PATCHES.md 3).
    fn read_switches(&mut self) -> Result<()> {
        use hx_proto::rpc::{key, op};
        let count = self.session.as_ref().map_or(0, |s| s.profile.switches);
        let read = self.with(|s| {
            (1..=count)
                .map(|n| {
                    let ask = hx_proto::msgmap! { key::SWITCH => Value::Int(i64::from(n)) };
                    s.request(hx_proto::ChannelId::DATA, op::FOOTSWITCH_CONFIG, ask)
                        .map(|reply| switch_read(n, &reply))
                })
                .collect::<hx_usb::Result<Vec<_>>>()
        });
        let switches = match read {
            Ok(switches) => Some(switches),
            Err(e) if self.session.is_some() => {
                tracing::debug!("footswitches unreadable: {e}");
                None
            }
            Err(e) => return Err(e),
        };
        if let Some(doc) = self.doc.as_mut() {
            doc.switches = switches.map(|s| footswitches(&s, &doc.preset.tone));
        }
        Ok(())
    }

    /// `show_chain`.
    fn render(&self, doc: &Doc) -> String {
        let (preset, catalog) = (&doc.preset, self.catalog.as_deref());
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
        let model_name = |model| model_name(catalog, model);
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
        let preset = self.read_settled()?;
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

    /// Set a parameter by native value: a slider step. Uses the cached
    /// document for the model (no preset read per step), re-reading only when
    /// it is missing or names another model than the client saw.
    fn set_native(&mut self, block: u32, param: u32, native: f32, model: u32) -> Result<String> {
        let catalog = self.catalog.clone().ok_or_else(|| {
            DeviceError::Invalid(
                "setting by native value needs HX Edit data on the server (/vendor)".into(),
            )
        })?;
        let slot = (block as usize).wrapping_sub(1);
        let model_at = |doc: &Option<Doc>| {
            doc.as_ref()
                .and_then(|d| d.preset.slots.get(slot))
                .and_then(|s| s.model)
        };
        if model_at(&self.doc) != Some(model) {
            self.chain()?;
        }
        match model_at(&self.doc) {
            Some(m) if m == model => {}
            Some(m) => {
                return Err(DeviceError::Stale(format!(
                    "block {block} now holds model {m}, not {model}; reload the chain"
                )))
            }
            None => {
                return Err(DeviceError::Stale(format!(
                    "no block at position {block}; reload the chain"
                )))
            }
        }
        let described = catalog.param(model, param as usize).ok_or_else(|| {
            DeviceError::Invalid(format!("model {model} has no parameter {param}"))
        })?;
        if !described.accepts(native) {
            return Err(DeviceError::Invalid(format!(
                "{native} is outside {}..{} for {}",
                described.min, described.max, described.name
            )));
        }
        let held = controls::settle(described.kind, native);
        let wire = described.kind.wire(native);
        if let Some(last) = self.last_param_write {
            thread::sleep(pace(last, Instant::now(), PARAM_GAP));
        }
        let index = i64::from(param);
        let sent = self.with(|s| s.set_param(slot as i64, index, wire));
        self.last_param_write = Some(Instant::now());
        sent?;
        self.patch_param(slot as i64, param, held);
        Ok(format!(
            "block {block}: {} = {}\n",
            described.name,
            catalog.format(described, held)
        ))
    }

    /// `snapshot`: opcode 88, zero-based. Confirmed on an HX Stomp by
    /// TonePush and on the HX Effects 3.80 (2026-10-14). A snapshot the
    /// preset lacks is refused (-46).
    fn snapshot(&mut self, index: u32) -> Result<String> {
        self.with(|s| s.select_snapshot(i64::from(index)))?;
        Ok(format!("snapshot {}\n", index + 1))
    }

    /// `tempo`: TonePush has no tempo opcode, so it rewrites the preset
    /// document (op21) and reads it back until the change shows.
    fn tempo(&mut self, bpm: f32) -> Result<String> {
        self.with(|s| s.set_tempo(bpm))?;
        self.doc = None;
        Ok(format!("tempo {bpm:.1} BPM\n"))
    }

    /// Put `model` into the empty slot at chain position `block`, on Path A
    /// or Path B ([`routing::check_cell`]). The first block on an empty
    /// Path B then gets the split and merge attached around the whole line,
    /// after the model lands (the device zeroes them when it does), as
    /// TonePush's GUI does ([`Worker::claim_whole_line`]). Op78 + op40
    /// confirmed on an HX Stomp by TonePush and on the HX Effects for Path A
    /// (2026-10-15) and Path B (2026-10-25).
    fn add_block(&mut self, block: u32, model: u32) -> Result<String> {
        let catalog = self.catalog.clone().ok_or_else(|| {
            DeviceError::Invalid("adding a block needs HX Edit data on the server (/vendor)".into())
        })?;
        if !controls::addable(&catalog, model) {
            return Err(DeviceError::Invalid(format!(
                "model {model} is not an effect this device offers"
            )));
        }
        self.read_doc()?;
        let slot = (block as usize).wrapping_sub(1);
        let preset = &self.doc.as_ref().expect("just read").preset;
        let place = routing_of(preset)
            .ok_or_else(|| DeviceError::Failed("the preset has no signal path".into()))
            .and_then(|r| routing::check_cell(&r, block))?;
        let target = preset
            .slots
            .get(slot)
            .filter(|s| matches!(s.kind, Kind::Block | Kind::Empty));
        match target {
            None => {
                return Err(DeviceError::Invalid(format!(
                    "position {block} is not a block slot"
                )))
            }
            Some(s) if s.model.is_some() => {
                return Err(DeviceError::Stale(format!(
                    "position {block} already holds {}; reload the chain",
                    model_name(Some(&*catalog), s.model.unwrap_or_default())
                )))
            }
            Some(_) => {}
        }
        self.with(|s| s.set_model(slot as i64, model))?;
        self.settle_slot(slot, Some(model));
        if place == (routing::Place::Branch { claim: true }) {
            self.claim_whole_line()?;
        }
        Ok(format!(
            "block {block} is now {}\n",
            model_name(Some(&*catalog), model)
        ))
    }

    /// Move the block at `block` into the empty cell `to` (Path A or Path B):
    /// the two slots trade places in the preset document, which is written
    /// back whole (op21), as TonePush's GUI moves blocks. A move that
    /// empties Path B leaves its attach points at zero (hx-usb settles them
    /// on every write). Verified on the HX Effects 3.80 (2026-10-22).
    ///
    /// The first block onto an empty Path B claims the whole line, in a
    /// second write: in the same write as the swap, the HX Effects puts the
    /// split where it likes (before cell 3 on 01A), as it zeroes the attach
    /// points when a model lands in [`Worker::add_block`]; a split moved on
    /// its own stays where it was put.
    ///
    /// A footswitch is not in the document, and the HX Effects drops it
    /// from a moved block (on neither cell afterwards): the block is put
    /// back on the switches it was on, as the fake and HX Edit keep them.
    fn move_block(&mut self, block: u32, to: u32, model: Option<u32>) -> Result<String> {
        self.read_doc()?;
        let held = self.block_at(block, model)?;
        self.read_switches()?;
        let before = self.doc.as_ref().and_then(|d| d.switches.clone());
        let preset = &self.doc.as_ref().expect("just read").preset;
        let (from_slot, to_slot) = (block as usize - 1, (to as usize).wrapping_sub(1));
        let place = routing_of(preset)
            .ok_or_else(|| DeviceError::Failed("the preset has no signal path".into()))
            .and_then(|r| routing::check_cell(&r, to))?;
        if let Some(m) = preset.slots.get(to_slot).and_then(|s| s.model) {
            return Err(DeviceError::Stale(format!(
                "position {to} already holds {}; reload the chain",
                model_name(self.catalog.as_deref(), m)
            )));
        }
        self.rewrite(|p| {
            p.swap_slots(from_slot, to_slot)
                .then_some(())
                .ok_or("that block cannot move there")
        })?;
        if place == (routing::Place::Branch { claim: true }) {
            self.claim_whole_line()?;
        }
        // The document is the one written; only the switches need reading.
        self.read_switches()?;
        let after = self.doc.as_ref().and_then(|d| d.switches.clone());
        if let (Some(before), Some(after)) = (before, after) {
            let (off, on) = switch_follow(&before, &after, block, to);
            for &n in &off {
                self.with(|s| s.unassign_bypass_footswitch(from_slot as i64, n))?;
            }
            for &n in &on {
                self.with(|s| s.assign_bypass_footswitch(to_slot as i64, n))?;
            }
            if !(off.is_empty() && on.is_empty()) {
                self.read_switches()?;
            }
        }
        Ok(format!(
            "moved {} from block {block} to block {to}\n",
            model_name(self.catalog.as_deref(), held)
        ))
    }

    /// Attach the split or the merge before cell `before` of Path A: a
    /// preset document write (op21), as TonePush's GUI drags a junction.
    /// Verified on the HX Effects 3.80 (2026-10-22).
    fn junction(&mut self, which: routing::Junction, before: u32) -> Result<String> {
        self.read_doc()?;
        let preset = &self.doc.as_ref().expect("just read").preset;
        let r = routing_of(preset)
            .ok_or_else(|| DeviceError::Failed("the preset has no signal path".into()))?;
        let occupied = |p: u32| {
            preset
                .slots
                .get(p as usize - 1)
                .is_some_and(|s| s.model.is_some())
        };
        routing::check_junction(&r, occupied, which, before)?;
        let junction = match which {
            routing::Junction::Split => r.split.as_ref(),
            routing::Junction::Join => r.join.as_ref(),
        }
        .map(|j| j.position as usize - 1)
        .ok_or_else(|| DeviceError::Failed("the preset has no split".into()))?;
        // Path A's cell `before` is the slot after the input plus `before`.
        let at = r.main[0] as usize - 1 + before as usize;
        self.rewrite(|p| {
            p.set_attach(junction, at)
                .then_some(())
                .ok_or("only a split or merge can be moved")
        })?;
        let name = match which {
            routing::Junction::Split => "split",
            routing::Junction::Join => "merge",
        };
        Ok(format!("{name} now before cell {}\n", before + 1))
    }

    /// Put parameter `param` of the block at `block` under expression pedal
    /// `pedal`, or take it off any controller: opcode 37, hx-usb's
    /// `assign_parameter`, as HX Edit's Controller Assign page. Then the
    /// document is read again: the controller table is in it. Confirmed on
    /// an HX Stomp by TonePush and on the HX Effects 3.80 (2026-10-22).
    fn controller(
        &mut self,
        block: u32,
        param: u32,
        pedal: Option<u8>,
        model: Option<u32>,
    ) -> Result<String> {
        if let Some(n) = pedal.filter(|n| !(1..=2).contains(n)) {
            return Err(DeviceError::Invalid(format!(
                "expression pedals are 1 and 2; {n} is not one"
            )));
        }
        let name = self.param_name(block, param, model)?;
        let slot = i64::from(block) - 1;
        let source = pedal.map(hx_proto::rpc::Source::Expression);
        self.with(|s| s.assign_parameter(slot, i64::from(param), source))?;
        self.read_doc()?;
        Ok(match pedal {
            Some(n) => format!("{name} on expression pedal {n}\n"),
            None => format!("{name} on no controller\n"),
        })
    }

    /// Move the heel and/or toe end of a parameter's controller travel:
    /// opcodes 65 and 66 (hx-usb `set_assign_range`), which take a fraction
    /// of the parameter's range; `min` and `max` are native values. The
    /// parameter must be under a controller. Verified on the HX Effects
    /// 3.80 (2026-10-22).
    fn controller_range(
        &mut self,
        block: u32,
        param: u32,
        min: Option<f32>,
        max: Option<f32>,
        model: Option<u32>,
    ) -> Result<String> {
        use hx_proto::preset::Target;
        let name = self.param_name(block, param, model)?;
        let slot = i64::from(block) - 1;
        let controls = self.controls();
        let doc = self.doc.as_ref().expect("read by param_name");
        let held = self.block_at(block, model)?;
        let assigned = doc
            .preset
            .assignments(controls)
            .iter()
            .any(|a| a.block == slot && a.target == Target::Param(i64::from(param)));
        if !assigned {
            return Err(DeviceError::Invalid(format!(
                "{name} is not under a controller; assign one first"
            )));
        }
        let catalog = self.catalog.clone().expect("checked by param_name");
        let p = catalog
            .param(held, param as usize)
            .expect("checked by param_name");
        let (low, high) = (p.min, p.max);
        let fraction = |v: f32| {
            if v.is_finite() && (low..=high).contains(&v) && high > low {
                Ok((v - low) / (high - low))
            } else {
                Err(DeviceError::Invalid(format!(
                    "{name} runs from {low} to {high}; {v} is outside it"
                )))
            }
        };
        let ends = [(min, false), (max, true)];
        let ends: Vec<(f32, bool)> = ends
            .into_iter()
            .filter_map(|(v, high_end)| v.map(|v| fraction(v).map(|f| (f, high_end))))
            .collect::<Result<_>>()?;
        if ends.is_empty() {
            return Err(DeviceError::Invalid("give min, max or both".into()));
        }
        for (f, high_end) in ends {
            self.with(|s| s.set_assign_range(slot, i64::from(param), f, high_end))?;
        }
        self.read_doc()?;
        Ok(format!("{name} travel set\n"))
    }

    /// Read the document and name parameter `param` of the block at
    /// `block`, which must hold `model` if given; needs the catalog.
    fn param_name(&mut self, block: u32, param: u32, model: Option<u32>) -> Result<String> {
        let catalog = self.catalog.clone().ok_or_else(|| {
            DeviceError::Invalid("controllers need HX Edit data on the server (/vendor)".into())
        })?;
        self.read_doc()?;
        let held = self.block_at(block, model)?;
        catalog
            .param(held, param as usize)
            .map(|p| p.name.clone())
            .ok_or_else(|| {
                DeviceError::Invalid(format!(
                    "{} has no parameter {param}",
                    model_name(Some(&*catalog), held)
                ))
            })
    }

    /// Attach the split and merge around the whole line ([`claim_line`]),
    /// for the first block on an empty Path B. The pedal placed the split
    /// itself when the block landed, and can place it again after the write:
    /// on the HX Effects (01A, 2026-10-25) the split written before cell 0
    /// read back there, then settled before cell 2. So read it back for
    /// about a second, claiming again whenever it moved.
    fn claim_whole_line(&mut self) -> Result<()> {
        self.read_doc()?;
        self.rewrite(claim_line)?;
        let want = self.doc.as_ref().and_then(|d| attach_points(&d.preset));
        for attempt in 0..SLOT_READS {
            thread::sleep(SLOT_READ_GAP);
            self.read_doc()?;
            let now = self.doc.as_ref().and_then(|d| attach_points(&d.preset));
            tracing::debug!(attempt, ?want, ?now, "Path B claim: split and merge");
            if now != want {
                tracing::info!("the pedal moved Path B's ends after the claim; claiming again");
                self.rewrite(claim_line)?;
            }
        }
        Ok(())
    }

    /// Change a copy of the cached document and write it back whole (op21).
    /// hx-usb's write settles empty branches and waits for the commit.
    fn rewrite(
        &mut self,
        change: impl FnOnce(&mut hx_proto::Preset) -> std::result::Result<(), &'static str>,
    ) -> Result<()> {
        let doc = self.doc.as_ref().ok_or_else(|| {
            DeviceError::Failed("the preset was not read before changing it".into())
        })?;
        let mut preset = hx_proto::Preset::parse(&doc.preset.encode())
            .ok_or_else(|| DeviceError::Failed("the preset does not re-parse".into()))?;
        change(&mut preset).map_err(|e| DeviceError::Invalid(e.into()))?;
        self.with(|s| s.write_preset(&preset))?;
        // hx-usb read the document back until it showed this write, so what
        // went out is what the pedal holds: keep it rather than read a third
        // time while the pedal settles (TonePush GUI, `edit_document`). It
        // went out with empty branches settled; so is the copy kept.
        preset.settle_branches();
        if let Some(doc) = self.doc.as_mut() {
            doc.preset = preset;
        }
        Ok(())
    }

    /// Empty the slot at chain position `block`. `model`, if given, must be
    /// what the slot holds. Confirmed on an HX Stomp by TonePush and on the
    /// HX Effects 3.80 (2026-10-15).
    fn remove_block(&mut self, block: u32, model: Option<u32>) -> Result<String> {
        self.read_doc()?;
        let held = self.block_at(block, model)?;
        let slot = block as usize - 1;
        self.with(|s| s.clear_block(slot as i64))?;
        self.settle_slot(slot, None);
        Ok(format!(
            "removed {} from block {block}\n",
            model_name(self.catalog.as_deref(), held)
        ))
    }

    /// The model of the block at chain position `block` in `doc`, which must
    /// be `model` if the client gave one: else [`DeviceError::Stale`].
    fn block_at(&self, block: u32, model: Option<u32>) -> Result<u32> {
        let held = self
            .doc
            .as_ref()
            .and_then(|d| d.preset.slots.get((block as usize).wrapping_sub(1)))
            .filter(|s| s.kind == Kind::Block)
            .and_then(|s| s.model);
        match (held, model) {
            (None, _) => Err(DeviceError::Stale(format!(
                "no block at position {block}; reload the chain"
            ))),
            (Some(h), Some(m)) if h != m => Err(DeviceError::Stale(format!(
                "block {block} now holds model {h}, not {m}; reload the chain"
            ))),
            (Some(h), _) => Ok(h),
        }
    }

    /// Put the block at chain position `block` on footswitch `switch`
    /// (one-based), or take it off with `None`: opcode 57 off any other
    /// switch it is on, then opcode 56, the calls TonePush's GUI makes
    /// (hx-usb converts to the wire's zero-based index). Then the switches
    /// are read back, so the chain shows what the pedal did, not what was
    /// asked. Confirmed on an HX Stomp by TonePush and on the HX Effects
    /// 3.80 (2026-10-15).
    fn footswitch(&mut self, block: u32, switch: Option<u8>, model: Option<u32>) -> Result<String> {
        let count = self.session.as_ref().expect("connected").profile.switches;
        if let Some(n) = switch.filter(|n| !(1..=count).contains(n)) {
            return Err(DeviceError::Invalid(format!(
                "this device has footswitches 1 to {count}; {n} is not one"
            )));
        }
        self.read_doc()?;
        let held = self.block_at(block, model)?;
        self.read_switches()?;
        let on: Vec<u8> = self
            .doc
            .as_ref()
            .and_then(|d| d.switches.as_ref())
            .ok_or_else(|| {
                DeviceError::Failed("the pedal did not say what its footswitches carry".into())
            })?
            .iter()
            .filter(|s| s.blocks.contains(&block))
            .map(|s| s.switch)
            .collect();
        let slot = i64::from(block) - 1;
        for &n in on.iter().filter(|&&n| Some(n) != switch) {
            self.with(|s| s.unassign_bypass_footswitch(slot, n))?;
        }
        if let Some(n) = switch.filter(|n| !on.contains(n)) {
            self.with(|s| s.assign_bypass_footswitch(slot, n))?;
        }
        self.read_switches()?;
        let name = model_name(self.catalog.as_deref(), held);
        Ok(match switch {
            Some(n) => format!("block {block} ({name}) on footswitch {n}\n"),
            None => format!("block {block} ({name}) on no footswitch\n"),
        })
    }

    /// Read the document until slot `slot` holds `want`, for at most about a
    /// second: a read right after a change can still return the old one.
    /// The write stands either way; the device's notification re-reads later.
    /// Then read the footswitches: a removed block leaves its switch.
    fn settle_slot(&mut self, slot: usize, want: Option<u32>) {
        for attempt in 0..SLOT_READS {
            if attempt > 0 {
                thread::sleep(SLOT_READ_GAP);
            }
            let read = self.read_doc().map(|_| {
                self.doc
                    .as_ref()
                    .and_then(|d| d.preset.slots.get(slot))
                    .map(|s| s.model)
            });
            if read.as_ref().is_ok_and(|m| *m == Some(want)) {
                let _ = self.read_switches();
                return;
            }
            if self.session.is_none() {
                return;
            }
        }
        let _ = self.read_switches();
        tracing::debug!(slot, ?want, "the document does not show the change yet");
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

    /// Mirror a rename into the cached header when it named the loaded
    /// preset, and show it. Another slot's name is not in the chain.
    fn renamed(&mut self, setlist: u32, index: u32, name: &str) {
        let loaded = (i64::from(setlist), i64::from(index));
        let Some(info) = self.doc.as_mut().and_then(|d| d.info.as_mut()) else {
            return;
        };
        if (info.0, info.1) == loaded {
            info.2 = name.to_owned();
            self.publish_doc(Cause::Other);
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

/// `helix-bridge watch [SECONDS]`: print every notification with a time
/// stamp, and a count every 10 s, the way `tonepush watch` does but through
/// the patched hx-usb (third_party/hx-usb/PATCHES.md). For checking on the
/// pedal that notifications keep flowing. Stop the service first.
pub fn watch(seconds: u64) -> std::result::Result<(), String> {
    let found = hx_usb::list().map_err(|e| e.to_string())?;
    let device = found.first().ok_or(NO_DEVICE)?;
    let mut session = device.open().map_err(|e| e.to_string())?;
    println!(
        "watching {} for {seconds} s; ctrl-c to stop",
        session.profile.name
    );
    if !session.events_answered() {
        println!(
            "warning: the events channel did not answer its handshake, so no notification \
             will arrive; wait 40 s for the pedal to stop sending to the previous session, \
             then run this again"
        );
    }
    let start = Instant::now();
    let (mut window, mut count, mut total) = (Instant::now(), 0u32, 0u32);
    while start.elapsed() < Duration::from_secs(seconds) {
        for (event, args) in session.poll_notifications().map_err(|e| e.to_string())? {
            count += 1;
            total += 1;
            if event != 22 || args.get(121).and_then(Value::as_i64) != Some(27) {
                println!(
                    "{:>6.1}s event {event}: {args:?}",
                    start.elapsed().as_secs_f32()
                );
            }
        }
        if window.elapsed() >= STATS_EVERY {
            println!(
                "{:>6.1}s {count} notifications in the last 10 s",
                start.elapsed().as_secs_f32()
            );
            (window, count) = (Instant::now(), 0);
        }
        thread::sleep(POLL);
        session.keepalive().map_err(|e| e.to_string())?;
    }
    println!("total: {total} notifications in {seconds} s");
    let skipped = session.events_skipped();
    if skipped > 0 {
        println!("skipped {skipped} bytes of events sent out of order");
    }
    Ok(())
}

/// `helix-bridge probe-slot POSITION`: read-only. Print the loaded preset's
/// slot row and layout, and the raw body of the slot at chain position
/// `position`. For comparing a block HX Edit added with one the bridge added.
/// Stop the service first.
pub fn probe_slot(position: usize) -> std::result::Result<(), String> {
    let mut session = probe_open()?;
    let preset = session.read_preset().map_err(|e| e.to_string())?;
    print_preset(&preset, position);
    Ok(())
}

/// `helix-bridge probe-add POSITION MODEL`: put model number `model` into the
/// empty slot at chain position `position` the way the bridge does (op78
/// then op40), printing every reply, every notification and the slot before
/// and after. For finding out why the HX Effects stores the block in its
/// document but does not run it. Writes to the edit buffer only. Stop the
/// service first.
pub fn probe_add(position: usize, model: u32) -> std::result::Result<(), String> {
    use hx_proto::rpc::{key, op};
    use hx_proto::ChannelId;

    let mut session = probe_open()?;
    let slot = position
        .checked_sub(1)
        .ok_or("positions count from 1, as /api/chain")?;
    let before = session.read_preset().map_err(|e| e.to_string())?;
    println!("== before");
    print_preset(&before, position);
    if before.slots.get(slot).is_none_or(|s| s.model.is_some()) {
        return Err(format!(
            "position {position} is not an empty slot; nothing sent"
        ));
    }
    print_events(&mut session, "backlog", Duration::from_millis(800))?;

    let block = Value::Int(slot as i64);
    let select = hx_proto::msgmap! { key::BLOCK => block.clone(), key::PATH => Value::Int(0) };
    println!("== op78 select {select:?}");
    let reply = session.request_full(ChannelId::DATA, op::SELECT_BLOCK, select);
    println!("reply (status, body): {reply:?}");
    print_events(&mut session, "after op78", Duration::from_millis(800))?;

    let set = hx_proto::msgmap! {
        key::BLOCK => block,
        key::MODEL_REF => hx_proto::msgmap! {
            key::PAIRED => Value::Bool(false),
            key::MODEL => Value::Int(i64::from(model)),
            key::PAIRED_MODEL => Value::Int(-1),
        },
    };
    println!("== op40 set model {set:?}");
    let reply = session.request_full(ChannelId::DATA, op::SET_MODEL, set);
    println!("reply (status, body): {reply:?}");
    print_events(&mut session, "after op40", Duration::from_secs(3))?;

    let after = session.read_preset().map_err(|e| e.to_string())?;
    println!("== after");
    print_preset(&after, position);
    println!(
        "== done. The block stays in the edit buffer: load another preset to drop it, \
         or keep it to compare with `probe-slot {position}`."
    );
    Ok(())
}

fn probe_open() -> std::result::Result<Session, String> {
    let found = hx_usb::list().map_err(|e| e.to_string())?;
    let device = found.first().ok_or(NO_DEVICE)?;
    let session = device.open().map_err(|e| e.to_string())?;
    println!(
        "{} (device id {:#010x})",
        session.profile.name, session.profile.device_id
    );
    if !session.events_answered() {
        println!(
            "warning: the events channel did not answer, so no notification will show; \
             wait 40 s after stopping the service, then run this again"
        );
    }
    Ok(session)
}

/// Path A, Path B and where the split and merge sit, from TonePush's reading
/// of the slot array (`Preset::layout`). The first path only: an HX Effects
/// has one. Positions count from 1, as `chain` prints them.
///
/// Confirmed on the HX Effects 3.80 with `probe-slot` (2026-10-22): split
/// 12, Path B 13-21, join 22, as in TonePush's HX Stomp capture, which keeps
/// an empty split and join after the output, with a branch cell per
/// main-row cell.
fn routing_of(preset: &hx_proto::Preset) -> Option<parse::Routing> {
    let layout = preset.layout();
    let path = layout.paths.first()?;
    let (input, output) = (path.input?, path.output?);
    let position = |slot: usize| slot as u32 + 1;
    let branch = match (path.split, path.join) {
        (Some(split), Some(join)) if split < join => (split + 1..join).map(position).collect(),
        _ => Vec::new(),
    };
    // TonePush draws a split with nothing below it as one line, and so do we.
    let divides = !path.lanes.is_empty();
    let junction = |slot: Option<usize>| {
        slot.map(|slot| parse::Junction {
            position: position(slot),
            before: preset
                .attach_of(slot)
                .filter(|_| divides)
                .and_then(|at| at.checked_sub(input + 1))
                .map(|cell| cell as u32),
        })
    };
    Some(parse::Routing {
        main: (input + 1..output).map(position).collect(),
        branch,
        split: junction(path.split),
        join: junction(path.join),
    })
}

/// What controllers move, by block position, from the preset's controller
/// table (TonePush `Preset::assignments`, document key 4). The ends of a
/// parameter's travel are native values; with the catalog they are also
/// formatted as the device shows them.
fn assignments_of(
    preset: &hx_proto::Preset,
    controls: hx_proto::rpc::ControlLayout,
    catalog: Option<&hx_catalog::Catalog>,
) -> Vec<(u32, parse::Assignment)> {
    use hx_proto::preset::Target;
    preset
        .assignments(controls)
        .into_iter()
        .filter_map(|a| {
            let position = u32::try_from(a.block).ok()? + 1;
            let model = preset.slots.get(a.block as usize).and_then(|s| s.model);
            let param = match a.target {
                Target::Bypass => None,
                Target::Param(i) => Some(u32::try_from(i).ok()?),
                // A cab's Mic is a model setting, not a parameter the API
                // can name by index (TonePush listed it as parameter 0, the
                // cab's Distance, before v0.10.0). Left out.
                Target::Mic => return None,
            };
            let shown = |v: f32| {
                let c = catalog?;
                c.param(model?, param? as usize).map(|p| c.format(p, v))
            };
            let ends = param.map(|_| (a.min, a.max));
            Some((
                position,
                parse::Assignment {
                    param,
                    source: parse::source_name(controls.ordinal(a.source)?),
                    min: ends.map(|e| e.0),
                    max: ends.map(|e| e.1),
                    min_value: ends.and_then(|e| shown(e.0)),
                    max_value: ends.and_then(|e| shown(e.1)),
                },
            ))
        })
        .collect()
}

/// Attach the first path's split after its input and its merge before its
/// output: the first block on an empty Path B parallels the whole line, as
/// TonePush's GUI does.
fn claim_line(p: &mut hx_proto::Preset) -> std::result::Result<(), &'static str> {
    let layout = p.layout();
    let path = layout
        .paths
        .first()
        .ok_or("the preset has no signal path")?;
    match (path.split, path.join, path.input, path.output) {
        (Some(split), Some(join), Some(input), Some(output))
            if p.set_attach(split, input + 1) && p.set_attach(join, output) =>
        {
            Ok(())
        }
        _ => Err("the preset has no split to attach"),
    }
}

/// The first path's split and merge, each as (its slot, the slot it
/// attaches before): `None` unless both are attached.
fn attach_points(p: &hx_proto::Preset) -> Option<[(usize, usize); 2]> {
    let path = p.layout().paths.into_iter().next()?;
    let (split, join) = (path.split?, path.join?);
    Some([(split, p.attach_of(split)?), (join, p.attach_of(join)?)])
}

/// What [`Worker::check_stored`] publishes for the [`unsaved_edits`] it
/// found: an edit if any, else the stored preset as loaded, which clears a
/// stale `unsaved` (the server sets the flag from the cause either way).
fn stored_cause(edits: &[String]) -> Cause {
    if edits.is_empty() {
        Cause::Preset
    } else {
        Cause::Edit
    }
}

/// Where the edit buffer `edit` holds edits its stored copy `stored` lacks,
/// as [`tone_diff`] paths; empty if none. A snapshot switch is not an edit
/// (the server's own rule, `/api/snapshot`), but it rewrites the buffer: on
/// the HX Effects (2026-10-22) snapshot 2 of 01A changed the active index
/// (10/6), a block's on/off (0/22/1/20/10), and a footswitch's light (two
/// leaves under 3). So when the snapshots differ, the stored copy is
/// compared as that snapshot would recall it.
fn unsaved_edits(
    stored: &hx_proto::Preset,
    edit: &hx_proto::Preset,
    controls: hx_proto::rpc::ControlLayout,
) -> Vec<String> {
    let snapshot_params: Vec<(i64, i64)> = stored
        .assignments(controls)
        .iter()
        .filter(|a| a.source == hx_proto::rpc::Source::Snapshots)
        .filter_map(|a| match a.target {
            hx_proto::preset::Target::Param(p) => Some((a.block, p)),
            hx_proto::preset::Target::Bypass | hx_proto::preset::Target::Mic => None,
        })
        .collect();
    recalled_diff(
        &stored.tone,
        &edit.tone,
        &stored.snapshot_details(),
        &snapshot_params,
    )
}

/// [`unsaved_edits`] on the tones: `snapshots` and `snapshot_params`
/// (block, parameter) are the stored copy's. Key 10 of a footswitch table
/// entry (3/8/switch/entry/10) is left out: opcode 56 sets it to 7 on every
/// assign while presets made on the pedal hold 0, and no opcode sets it back
/// (58 and 62 tried, HX Effects 2026-10-25), so moving a block to another
/// switch and back read as an edit after a restart.
fn recalled_diff(
    stored: &Value,
    edit: &Value,
    snapshots: &[hx_proto::preset::Snapshot],
    snapshot_params: &[(i64, i64)],
) -> Vec<String> {
    let assign_mark = |p: &String| {
        matches!(
            p.split('/').collect::<Vec<_>>().as_slice(),
            ["3", "8", _, _, "10"]
        )
    };
    recalled_tone_diff(stored, edit, snapshots, snapshot_params)
        .into_iter()
        .filter(|p| !assign_mark(p))
        .collect()
}

/// [`recalled_diff`] before the assign marks are left out.
fn recalled_tone_diff(
    stored: &Value,
    edit: &Value,
    snapshots: &[hx_proto::preset::Snapshot],
    snapshot_params: &[(i64, i64)],
) -> Vec<String> {
    const ACTIVE: [i64; 2] = [10, 6];
    let active = |tone: &Value| tone.get(ACTIVE[0])?.get(ACTIVE[1])?.as_i64();
    let (Some(was), Some(now)) = (active(stored), active(edit)) else {
        return tone_diff(stored, edit);
    };
    if was == now {
        return tone_diff(stored, edit);
    }
    // The stored copy, recalled to snapshot `now`: its index and on/offs.
    let mut recalled = stored.clone();
    set_at(&mut recalled, &ACTIVE, Value::Int(now));
    let on = usize::try_from(now)
        .ok()
        .and_then(|i| snapshots.get(i))
        .map_or(&[][..], |s| &s.enabled[..]);
    for (slot, on) in on.iter().enumerate() {
        if let Some(on) = on {
            set_at(
                &mut recalled,
                &[0, 22, slot as i64, 20, 10],
                Value::Bool(*on),
            );
        }
    }
    // What a recall sets that the document does not say how: values under
    // the snapshot controller, the snapshot's tempo, footswitch lights.
    let recalled_value = |path: &str| {
        let keys: Vec<&str> = path.split('/').collect();
        let param = match keys.as_slice() {
            ["0", "22", block, "20", "11", "4", param] => {
                block.parse().ok().zip(param.parse().ok())
            }
            _ => None,
        };
        param.is_some_and(|p| snapshot_params.contains(&p))
            || path == "5/16"
            || matches!(keys.as_slice(), ["3", .., "11", "6" | "7"])
    };
    tone_diff(&recalled, edit)
        .into_iter()
        .filter(|p| !recalled_value(p))
        .collect()
}

/// Put `to` at `path` (map keys, array indices) in `v`. False if absent.
fn set_at(v: &mut Value, path: &[i64], to: Value) -> bool {
    let Some((&k, rest)) = path.split_first() else {
        *v = to;
        return true;
    };
    let next = match v {
        Value::Array(items) => usize::try_from(k).ok().and_then(|i| items.get_mut(i)),
        v => v.get_mut(k),
    };
    next.is_some_and(|n| set_at(n, rest, to))
}

/// Where two preset tones differ, as `/`-joined key paths. Numbers compare
/// by value, not by the width they came in: a stored document and a loaded
/// one encode some integers differently.
fn tone_diff(stored: &Value, edit: &Value) -> Vec<String> {
    fn walk(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
        let at = |k: &dyn std::fmt::Display| {
            if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}/{k}")
            }
        };
        match (a, b) {
            (Value::Map(x), Value::Map(y)) => {
                for (k, va) in x {
                    let name = k.as_i64().map_or_else(|| "?".to_owned(), |i| i.to_string());
                    match y.iter().find(|(kb, _)| kb == k) {
                        Some((_, vb)) => walk(va, vb, &at(&name), out),
                        None => out.push(at(&name)),
                    }
                }
                for (k, _) in y.iter().filter(|(k, _)| !x.iter().any(|(ka, _)| ka == k)) {
                    let name = k.as_i64().map_or_else(|| "?".to_owned(), |i| i.to_string());
                    out.push(at(&name));
                }
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
                for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                    walk(va, vb, &at(&i), out);
                }
            }
            _ => {
                let same = match (a.as_i64(), b.as_i64()) {
                    (Some(i), Some(j)) => i == j,
                    _ => match (a.as_f32(), b.as_f32()) {
                        (Some(f), Some(g)) => f == g,
                        _ => match (a.as_raw(), b.as_raw()) {
                            (Some(r), Some(s)) => r == s,
                            _ => a == b,
                        },
                    },
                };
                if !same {
                    out.push(if path.is_empty() {
                        "(tone)".to_owned()
                    } else {
                        path.to_owned()
                    });
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(stored, edit, "", &mut out);
    out
}

/// What a move of the block at `from` to `to` leaves to do on the
/// footswitches, from the switches read `before` and `after` it: the
/// switches still listing the emptied `from` (take it off them), and the
/// ones the block was on that do not list `to` (put it on them).
fn switch_follow(
    before: &[parse::Footswitch],
    after: &[parse::Footswitch],
    from: u32,
    to: u32,
) -> (Vec<u8>, Vec<u8>) {
    let on = |list: &[parse::Footswitch], position: u32| -> Vec<u8> {
        list.iter()
            .filter(|s| s.blocks.contains(&position))
            .map(|s| s.switch)
            .collect()
    };
    let now = on(after, to);
    let lost = on(before, from)
        .into_iter()
        .filter(|n| !now.contains(n))
        .collect();
    (on(after, from), lost)
}

/// The slot row (kind and model per index), the layout, and one raw slot.
fn print_preset(preset: &hx_proto::Preset, position: usize) {
    for (i, s) in preset.slots.iter().enumerate() {
        println!(
            "slot {i:>2} (position {:>2}): {:?} model {:?} paired {:?} enabled {} values {}",
            i + 1,
            s.kind,
            s.model,
            s.paired,
            s.enabled,
            s.values.len()
        );
    }
    println!("layout: {:?}", preset.layout());
    match position.checked_sub(1).and_then(|i| preset.raw_slot(i)) {
        Some(raw) => println!("raw slot at position {position}: {raw:?}"),
        None => println!("no raw slot at position {position}"),
    }
}

/// Print every notification for `wait`, keeping the session alive.
fn print_events(
    session: &mut Session,
    label: &str,
    wait: Duration,
) -> std::result::Result<(), String> {
    let start = Instant::now();
    let mut n = 0;
    while start.elapsed() < wait {
        for (event, args) in session.poll_notifications().map_err(|e| e.to_string())? {
            n += 1;
            println!(
                "  {label} {:>4} ms event {event}: {args:?}",
                start.elapsed().as_millis()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
    session.keepalive().map_err(|e| e.to_string())?;
    println!("  {label}: {n} notifications");
    Ok(())
}

/// The active snapshot, zero-based: tone key 10 (snapshots), key 6, as
/// TonePush's GUI reads it (`session.rs`, `active_snapshot`). Not verified
/// on the HX Effects yet.
fn active_snapshot(preset: &hx_proto::Preset) -> Option<u32> {
    const SNAPSHOT_SECTION: i64 = 10;
    const ACTIVE: i64 = 6;
    let index = preset
        .tone
        .get(SNAPSHOT_SECTION)?
        .get(ACTIVE)?
        .as_i64()
        .and_then(|n| u32::try_from(n).ok())?;
    ((index as usize) < preset.snapshots().len()).then_some(index)
}

/// One footswitch as opcode 33 answered, what the bridge needs of it.
#[derive(Debug, PartialEq)]
struct SwitchRead {
    /// One-based.
    switch: u8,
    label: Option<String>,
    /// Blocks named in its assignment list, as the device numbers them.
    blocks: Vec<i64>,
}

/// Decode an opcode 33 reply (PROTOCOL.md): `{102: index, 109: label,
/// 67: [{69: {98: block, 109: name, …}, …}]}`. Lenient: an entry without a
/// block, a label that is not text, a list that is not a list are skipped,
/// never an error, so an odd reply cannot end the session.
fn switch_read(switch: u8, reply: &Value) -> SwitchRead {
    use hx_proto::rpc::key;
    let label = match reply.get(key::NAME) {
        Some(Value::Str(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    let blocks = match reply.get(key::SWITCH_ASSIGNED) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get(key::SWITCH_TARGET)?.get(key::BLOCK)?.as_i64())
            .filter(|block| *block >= 0)
            .collect(),
        _ => Vec::new(),
    };
    SwitchRead {
        switch,
        label,
        blocks,
    }
}

/// Which slot holds which model: what a front-panel add or remove changes,
/// and with it possibly the footswitches.
fn slot_models(preset: &hx_proto::Preset) -> Vec<Option<u32>> {
    preset.slots.iter().map(|s| s.model).collect()
}

/// What each footswitch carries, by chain position (`block + 1`).
///
/// Opcode 33 lists everything a switch drives on a block, parameters
/// included, and does not say which. The document's controller table holds
/// every parameter assignment and no footswitch bypass (PROTOCOL.md), so an
/// entry the table accounts for is a parameter and is left out, as TonePush's
/// GUI does (`all_assignments`).
fn footswitches(switches: &[SwitchRead], tone: &Value) -> Vec<parse::Footswitch> {
    switches
        .iter()
        .map(|s| {
            let mut blocks: Vec<u32> = s
                .blocks
                .iter()
                .filter(|&&block| !drives_parameter(tone, block, s.switch))
                .filter_map(|&block| u32::try_from(block + 1).ok())
                .collect();
            blocks.sort_unstable();
            blocks.dedup();
            parse::Footswitch {
                switch: s.switch,
                label: s.label.clone(),
                blocks,
            }
        })
        .collect()
}

/// Whether the document's controller table (tone key 4, indexed by source
/// ordinal) has footswitch `switch` driving a parameter of `block`.
/// Footswitch n is ordinal n + 2 (PROTOCOL.md: 3-7 on an HX Stomp; 3-8 on
/// an HX Effects is inferred from its presets, not confirmed, upstream #9).
fn drives_parameter(tone: &Value, block: i64, switch: u8) -> bool {
    const ASSIGNMENTS: i64 = 4;
    const WHAT: i64 = 1;
    const ON: i64 = 5;
    const PARAM: i64 = 6;
    const BYPASS: i64 = 9;
    let Some(Value::Array(by_source)) = tone.get(ASSIGNMENTS) else {
        return false;
    };
    let Some(Value::Array(entries)) = by_source.get(usize::from(switch) + 2) else {
        return false;
    };
    entries.iter().filter_map(|e| e.get(WHAT)).any(|what| {
        what.get(ON).and_then(Value::as_i64) == Some(block)
            && what.get(PARAM).is_some()
            && what.get(BYPASS).is_none()
    })
}

/// `helix-bridge probe-unsaved`: read-only. Compare the loaded preset's edit
/// buffer with its stored copy, as the server does when a session opens
/// ([`Worker::check_stored`]): what it would decide, then every raw
/// difference with both values (snapshot recall included).
pub fn probe_unsaved() -> std::result::Result<(), String> {
    let mut session = probe_open()?;
    let (setlist, index, name) = session.preset_info().map_err(|e| e.to_string())?;
    println!("loaded: setlist {setlist}, index {index}, {name}");
    let edit = session.read_preset().map_err(|e| e.to_string())?;
    let Some(stored) = session
        .read_preset_at(setlist, index)
        .map_err(|e| e.to_string())?
    else {
        println!("the stored slot is empty: the server would mark the preset unsaved");
        return Ok(());
    };
    match unsaved_edits(&stored, &edit, session.profile.controls).as_slice() {
        [] => println!("the server would leave it saved"),
        edits => println!("the server would mark it unsaved: {}", edits.join(", ")),
    }
    let paths = tone_diff(&stored.tone, &edit.tone);
    let at = |tone: &Value, path: &str| {
        path.split('/').try_fold(tone.clone(), |v, k| {
            let k: i64 = k.parse().ok()?;
            match v {
                Value::Array(items) => items.get(usize::try_from(k).ok()?).cloned(),
                v => v.get(k).cloned(),
            }
        })
    };
    for path in &paths {
        println!("differs at {path}:");
        println!("  stored: {:?}", at(&stored.tone, path));
        println!("  edit:   {:?}", at(&edit.tone, path));
        if let Some((parent, _)) = path.rsplit_once('/') {
            println!("  in {parent}, stored: {:?}", at(&stored.tone, parent));
            println!("  in {parent}, edit:   {:?}", at(&edit.tone, parent));
        }
    }
    // Tone key 3 is undecoded (footswitch lights live under it); print it
    // whole when it differs, to see which switch an index is.
    if paths.iter().any(|p| p.starts_with("3/")) {
        println!("tone key 3, stored: {:?}", stored.tone.get(3));
        println!("tone key 3, edit:   {:?}", edit.tone.get(3));
    }
    Ok(())
}

/// `helix-bridge probe-switch-table`: read-only. For every stored preset of
/// the loaded setlist (opcode 4, which loads nothing), print what its
/// footswitch table (tone key 3/8, one entry list per switch) holds: per
/// entry, key 10 and the target's name. Key 10 is undecoded; this shows
/// which values the pedal keeps there.
pub fn probe_switch_table() -> std::result::Result<(), String> {
    let mut session = probe_open()?;
    let (setlist, _, _) = session.preset_info().map_err(|e| e.to_string())?;
    let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
    for index in 0..128 {
        let Some(preset) = session
            .read_preset_at(setlist, index)
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let Some(Value::Array(switches)) = preset.tone.get(3).and_then(|t| t.get(8)) else {
            println!("{index:>3}: no switch table: {:?}", preset.tone.get(3));
            continue;
        };
        let mut line = Vec::new();
        for (n, entries) in switches.iter().enumerate() {
            let Value::Array(entries) = entries else {
                continue;
            };
            for e in entries {
                let k10 = format!("{:?}", e.get(10));
                let name = e.get(11).and_then(|t| t.get(5)).cloned();
                line.push(format!("FS{} {k10} {name:?}", n + 1));
                *counts.entry(k10).or_default() += 1;
            }
        }
        if !line.is_empty() {
            println!("{index:>3}: {}", line.join(" | "));
        }
    }
    println!("key 10 values: {counts:?}");
    Ok(())
}

/// `helix-bridge probe-switches`: read-only. Print each footswitch's raw
/// opcode 33 reply and how hx-usb decodes it, the loaded preset's blocks
/// and its controller table (tone key 4) by ordinal, and what the bridge
/// would show. For confirming on the HX Effects which switch is which before
/// the bridge writes one (AGENTS.md 6). Stop the service first.
pub fn probe_switches() -> std::result::Result<(), String> {
    use hx_proto::rpc::{key, op};
    use hx_proto::ChannelId;

    let mut session = probe_open()?;
    let count = session.profile.switches;
    let preset = session.read_preset().map_err(|e| e.to_string())?;
    println!("== blocks (position = block + 1, as /api/chain)");
    for (slot, block) in preset.blocks() {
        println!(
            "position {:>2} (block {slot:>2}): model {:?} {}",
            slot + 1,
            block.model,
            if block.enabled { "on" } else { "bypassed" }
        );
    }
    println!("== controller table (tone key 4), by source ordinal");
    match preset.tone.get(4) {
        Some(Value::Array(by_source)) => {
            for (ordinal, entries) in by_source.iter().enumerate() {
                if !matches!(entries, Value::Nil) {
                    println!("ordinal {ordinal}: {entries:?}");
                }
            }
        }
        other => println!("none: {other:?}"),
    }
    println!("== footswitches 1 to {count} (opcode 33, one-based in)");
    let mut decoded = Vec::new();
    for n in 1..=count {
        let ask = hx_proto::msgmap! { key::SWITCH => Value::Int(i64::from(n)) };
        let reply = session.request_full(ChannelId::DATA, op::FOOTSWITCH_CONFIG, ask.clone());
        println!("FS{n} raw (status, body): {reply:?}");
        // hx-usb's strict decoder: an error here is what used to end the
        // server's session.
        match session.read_switch(n) {
            Ok(s) => println!("FS{n} hx-usb: {s:?}"),
            Err(e) => println!("FS{n} hx-usb decode failed: {e}"),
        }
        match session.request(ChannelId::DATA, op::FOOTSWITCH_CONFIG, ask) {
            Ok(body) => decoded.push(switch_read(n, &body)),
            Err(e) => println!("FS{n} read failed: {e}"),
        }
    }
    print_events(&mut session, "during the reads", Duration::from_millis(300))?;
    println!("== as the bridge shows it (position per switch)");
    for s in footswitches(&decoded, &preset.tone) {
        println!("FS{}: {:?} label {:?}", s.switch, s.blocks, s.label);
    }
    Ok(())
}

/// Whether the cached chain is due a check against the pedal: a minute since
/// the last one, and the session idle.
fn audit_due(now: Instant, last_audit: Instant, last_job: Instant) -> bool {
    now >= last_audit + AUDIT_EVERY && now >= last_job + AUDIT_IDLE
}

/// How long to wait before the next slider write so writes stay `gap` apart.
fn pace(last: Instant, now: Instant, gap: Duration) -> Duration {
    (last + gap).saturating_duration_since(now)
}

/// What a run of queued commands may fold into its last one.
#[derive(Clone, Copy, PartialEq)]
enum Fold {
    /// Slider writes to one parameter: (block, param).
    Param(u32, u32),
    /// Preset loads: only the last is where the clients meant to land, and
    /// a stack of switches is the load that wedges the pedal (TonePush GUI,
    /// `gather`).
    Select,
}

/// Fold commands queued behind `cmd` into one: a later slider write to the
/// same parameter, or a later preset load, replaces it (last one wins), and
/// every caller gets that command's answer. Stops at the first other job,
/// which is returned to run next, so nothing is reordered. Other commands
/// pass through untouched.
fn coalesce(
    mut cmd: Command,
    reply: Reply,
    jobs: &mpsc::Receiver<Job>,
) -> (Command, Vec<Reply>, Option<Job>) {
    let mut replies = vec![reply];
    let target = |c: &Command| match c {
        Command::SetNative { block, param, .. } => Some(Fold::Param(*block, *param)),
        Command::Select { .. } => Some(Fold::Select),
        _ => None,
    };
    let Some(wanted) = target(&cmd) else {
        return (cmd, replies, None);
    };
    while let Ok(job) = jobs.try_recv() {
        match job {
            Job::Run(next, reply) if target(&next) == Some(wanted) => {
                cmd = next;
                replies.push(reply);
            }
            other => return (cmd, replies, Some(other)),
        }
    }
    (cmd, replies, None)
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

/// A model's name from the catalog, or `model 388` without it.
fn model_name(catalog: Option<&hx_catalog::Catalog>, model: u32) -> String {
    catalog
        .and_then(|c| c.model_number(model))
        .map_or_else(|| format!("model {model}"), |m| m.name.clone())
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

    fn native(block: u32, native: f32) -> Command {
        Command::SetNative {
            block,
            param: 0,
            native,
            model: 269,
        }
    }

    #[test]
    fn slider_writes_coalesce() {
        let (tx, rx) = mpsc::channel();
        let mut answers = Vec::new();
        let mut send = |cmd| {
            let (reply, answer) = oneshot::channel();
            tx.send(Job::Run(cmd, reply)).unwrap();
            answers.push(answer);
        };
        send(native(2, 0.2));
        send(native(2, 0.3));
        send(native(2, 0.4));
        send(native(3, 0.9));
        send(native(2, 0.5));
        let Ok(Job::Run(first, reply)) = rx.try_recv() else {
            panic!("first job");
        };
        let (cmd, replies, next) = coalesce(first, reply, &rx);
        // Last value for block 2 wins; block 3 waits its turn, unreordered.
        assert_eq!(cmd, native(2, 0.4));
        assert_eq!(replies.len(), 3);
        let Some(Job::Run(next, reply)) = next else {
            panic!("block 3 kept");
        };
        assert_eq!(next, native(3, 0.9));
        let (cmd, replies, _) = coalesce(next, reply, &rx);
        assert_eq!((cmd, replies.len()), (native(3, 0.9), 1));
        // Other commands never coalesce.
        let (reply, _answer) = oneshot::channel();
        tx.send(Job::Run(Command::Save, oneshot::channel().0))
            .unwrap();
        let (cmd, replies, next) = coalesce(Command::Save, reply, &rx);
        assert_eq!((cmd, replies.len()), (Command::Save, 1));
        assert!(next.is_none());
        assert!(matches!(rx.try_recv(), Ok(Job::Run(Command::Save, _))));
    }

    #[test]
    fn preset_loads_coalesce() {
        let (tx, rx) = mpsc::channel();
        let select = |index| Command::Select { setlist: 0, index };
        for cmd in [select(5), select(6), native(2, 0.2), select(8)] {
            tx.send(Job::Run(cmd, oneshot::channel().0)).unwrap();
        }
        let Ok(Job::Run(first, reply)) = rx.try_recv() else {
            panic!("first job");
        };
        let (cmd, replies, next) = coalesce(first, reply, &rx);
        // Only the last load of the run goes out; the slider write after it
        // stays where it was, and so does the load behind that.
        assert_eq!((cmd, replies.len()), (select(6), 2));
        assert!(matches!(next, Some(Job::Run(Command::SetNative { .. }, _))));
        assert!(matches!(
            rx.try_recv(),
            Ok(Job::Run(Command::Select { index: 8, .. }, _))
        ));
    }

    #[test]
    fn slider_writes_are_paced() {
        let t = Instant::now();
        let gap = Duration::from_millis(40);
        assert_eq!(pace(t, t, gap), gap);
        assert_eq!(
            pace(t, t + Duration::from_millis(15), gap),
            Duration::from_millis(25)
        );
        assert_eq!(pace(t, t + Duration::from_millis(90), gap), Duration::ZERO);
    }

    #[test]
    fn checks_wait_for_a_quiet_minute() {
        let t = Instant::now();
        let s = Duration::from_secs;
        assert!(!audit_due(t + s(59), t, t));
        assert!(audit_due(t + s(60), t, t));
        // A command 5 s ago (a drag): wait until 10 s after it.
        assert!(!audit_due(t + s(60), t, t + s(55)));
        assert!(audit_due(t + s(65), t, t + s(55)));
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
        // Snapshot switched on the pedal (42/46 {92: index}, TonePush
        // session.rs): re-read, which also picks up the active snapshot.
        let picked = msgmap! { 92 => Value::Int(2) };
        assert_eq!(notice(42, &picked), Some(Cause::Other));
        assert_eq!(notice(46, &Value::Nil), Some(Cause::Other));
        assert_eq!(notice(77, &Value::Nil), Some(Cause::Other));
    }

    /// Paths to what differs; numbers equal whatever width they came in.
    #[test]
    fn tone_diff_names_what_changed() {
        let tone = |mix: Value, tempo: f32, slots: Vec<Value>| {
            msgmap! { 1 => Value::Array(slots), 3 => msgmap! { 2 => mix, 5 => Value::F32(tempo) } }
        };
        let slots = || vec![Value::Int(1), Value::Str("Fassel".into())];
        let stored = tone(Value::Int(5), 120.0, slots());
        assert!(tone_diff(&stored, &tone(Value::Wide(5, 1), 120.0, slots())).is_empty());
        assert_eq!(
            tone_diff(&stored, &tone(Value::Int(5), 96.0, slots())),
            ["3/5"]
        );
        let renamed = vec![Value::Int(1), Value::Str("Fassel 2".into())];
        assert_eq!(
            tone_diff(&stored, &tone(Value::Int(5), 120.0, renamed)),
            ["1/1"]
        );
        assert_eq!(
            tone_diff(&stored, &tone(Value::Int(5), 120.0, vec![])),
            ["1"]
        );
        let extra = msgmap! { 1 => Value::Array(slots()), 3 => msgmap! { 2 => Value::Int(5), 5 => Value::F32(120.0) }, 9 => Value::Nil };
        assert_eq!(tone_diff(&stored, &extra), ["9"]);
    }

    /// A clean comparison clears the flag rather than leaving it: after a
    /// pedal restart the edits are gone, the service's flag was not.
    #[test]
    fn a_clean_comparison_clears_unsaved() {
        assert_eq!(stored_cause(&[]), Cause::Preset);
        assert_eq!(stored_cause(&["0/22/1/20/10".to_owned()]), Cause::Edit);
        let empty_slot = ["(the slot is empty)".to_owned()];
        assert_eq!(stored_cause(&empty_slot), Cause::Edit);
    }

    /// A snapshot switch alone is no edit; an edit made on top of one is.
    /// Shaped as 01A on the HX Effects (2026-10-22): snapshot 2 turns the
    /// gate in slot 1 on and lights its footswitch.
    #[test]
    fn a_snapshot_switch_is_not_an_edit() {
        let slot = |on: bool, values: Vec<Value>| {
            msgmap! { 20 => msgmap! { 10 => Value::Bool(on), 11 => msgmap! { 4 => Value::Array(values) } } }
        };
        let tone = |active: i64, gate: bool, mix: f32, light: i64| {
            msgmap! {
                0 => msgmap! { 22 => Value::Array(vec![
                    Value::Nil,
                    slot(gate, vec![Value::F32(-87.0)]),
                    slot(true, vec![Value::F32(0.5), Value::F32(mix)]),
                ]) },
                3 => msgmap! { 8 => Value::Array(vec![msgmap! { 11 => msgmap! { 6 => Value::Int(light), 7 => Value::Bool(gate) } }]) },
                5 => msgmap! { 16 => Value::F32(120.0) },
                10 => msgmap! { 6 => Value::Int(active) },
            }
        };
        let snapshot = |gate: bool| hx_proto::preset::Snapshot {
            name: String::new(),
            tempo: None,
            valid: true,
            named: false,
            enabled: vec![None, Some(gate), Some(true)],
            colour: None,
        };
        let snapshots = [snapshot(false), snapshot(true)];
        let stored = tone(0, false, 0.2, 527_360);
        let diff =
            |edit: &Value, params: &[(i64, i64)]| recalled_diff(&stored, edit, &snapshots, params);

        // Snapshot 2 recalled: nothing to save.
        assert!(diff(&tone(1, true, 0.2, 8_716_032), &[]).is_empty());
        // A snapshot-controlled mix may differ too.
        assert!(diff(&tone(1, true, 0.7, 8_716_032), &[(2, 1)]).is_empty());
        // The same mix with no snapshot controller is an edit.
        assert_eq!(
            diff(&tone(1, true, 0.7, 8_716_032), &[]),
            ["0/22/2/20/11/4/1"]
        );
        // The gate off on snapshot 2, which recalls it on: an edit.
        assert_eq!(diff(&tone(1, false, 0.2, 8_716_032), &[]), ["0/22/1/20/10"]);
        // Same snapshot: everything counts, the light included.
        assert_eq!(diff(&tone(0, true, 0.2, 8_716_032), &[]).len(), 3);
    }

    /// A block moved to another footswitch and back is no edit, though
    /// opcode 56 left its mark (key 10: 0 stored, 7 after) on the entry.
    /// Shaped as 01A on the HX Effects (2026-10-25).
    #[test]
    fn a_claim_spans_the_line_and_sees_a_moved_split() {
        let bytes = include_bytes!("../../third_party/hx-proto/tests/preset.bin");
        let mut p = hx_proto::Preset::parse(bytes).expect("fixture parses");
        claim_line(&mut p).expect("the fixture has a split");
        let path = p.layout().paths.into_iter().next().expect("a path");
        let (input, output) = (path.input.unwrap(), path.output.unwrap());
        let want = attach_points(&p).expect("attached");
        assert_eq!([want[0].1, want[1].1], [input + 1, output]);
        // The pedal settles the split two cells on, as on 01A.
        assert!(p.set_attach(want[0].0, input + 3));
        assert_ne!(attach_points(&p), Some(want));
        claim_line(&mut p).expect("claimed again");
        assert_eq!(attach_points(&p), Some(want));
    }

    #[test]
    fn a_footswitch_round_trip_is_not_an_edit() {
        let tone = |mark: i64, on: Option<i64>| {
            let entry = |block: i64| {
                msgmap! { 10 => Value::Int(mark), 11 => msgmap! { 5 => Value::Str("Noise Gate".into()), 8 => Value::Int(block) } }
            };
            let switches = [Some(1), on]
                .iter()
                .map(|b| Value::Array(b.map(entry).into_iter().collect()))
                .collect();
            msgmap! { 3 => msgmap! { 8 => Value::Array(switches) } }
        };
        let diff = |edit: &Value| recalled_diff(&tone(0, None), edit, &[], &[]);
        assert!(diff(&tone(7, None)).is_empty());
        // Also on FS2: an edit, the mark aside.
        assert_eq!(diff(&tone(7, Some(1))), ["3/8/1"]);
    }

    /// A moved block goes back on its switches; one the pedal left on the
    /// emptied cell comes off it.
    #[test]
    fn moved_blocks_keep_their_switches() {
        let fs = |switch, blocks: &[u32]| parse::Footswitch {
            switch,
            label: None,
            blocks: blocks.to_vec(),
        };
        let before = [fs(1, &[2]), fs(5, &[9]), fs(6, &[8, 9])];
        // The HX Effects drops it (2026-10-22): FS5 and FS6 lost block 9.
        let dropped = [fs(1, &[2]), fs(5, &[]), fs(6, &[8])];
        assert_eq!(
            switch_follow(&before, &dropped, 9, 10),
            (vec![], vec![5, 6])
        );
        // A pedal that keeps the switch on the cell.
        let kept = [fs(1, &[2]), fs(5, &[9]), fs(6, &[8, 9])];
        assert_eq!(
            switch_follow(&before, &kept, 9, 10),
            (vec![5, 6], vec![5, 6])
        );
        // One that moves it along: nothing to do.
        let moved = [fs(1, &[2]), fs(5, &[10]), fs(6, &[8, 10])];
        assert_eq!(switch_follow(&before, &moved, 9, 10), (vec![], vec![]));
        // A block on no switch.
        assert_eq!(switch_follow(&before, &before, 4, 3), (vec![], vec![]));
    }

    fn switch(n: u8, blocks: &[i64]) -> SwitchRead {
        SwitchRead {
            switch: n,
            label: None,
            blocks: blocks.to_vec(),
        }
    }

    /// PROTOCOL.md's opcode 33 replies, and ones hx-usb's decoder refuses
    /// (an empty name, no enabled flag), which must not be errors here.
    #[test]
    fn switch_replies_decode_leniently() {
        let unassigned = msgmap! {
            102 => Value::Int(0), 65 => Value::Bool(false), 109 => Value::Nil,
            66 => Value::Nil, 67 => Value::Nil,
        };
        assert_eq!(switch_read(1, &unassigned), switch(1, &[]));
        let entry = |block: i64, name: &str| {
            msgmap! {
                59 => Value::Bool(true), 68 => Value::Int(1), 66 => Value::Int(16_711_683),
                69 => msgmap! {
                    109 => Value::Str(name.into()), 98 => Value::Int(block),
                    29 => Value::Bool(false), 26 => Value::Int(0), 28 => Value::Int(0),
                },
            }
        };
        let odd =
            msgmap! { 69 => msgmap! { 109 => Value::Str(String::new()), 98 => Value::Int(5) } };
        let no_block = msgmap! { 69 => msgmap! { 109 => Value::Str("x".into()) } };
        let carrying = msgmap! {
            102 => Value::Int(2), 65 => Value::Bool(false), 109 => Value::Str("Lead".into()),
            66 => Value::Nil,
            67 => Value::Array(vec![entry(2, "Jazz Rivet 120"), odd, no_block, Value::Nil]),
        };
        let got = switch_read(3, &carrying);
        assert_eq!(got.label.as_deref(), Some("Lead"));
        assert_eq!(got.blocks, [2, 5]);
        assert_eq!(switch_read(4, &Value::Nil), switch(4, &[]));
    }

    /// Opcode 33's lists, less what the document says is a parameter.
    #[test]
    fn footswitches_by_position() {
        // Footswitch 3 (ordinal 5) drives block 4's parameter 1, as in
        // PROTOCOL.md's `CT-Master Lead` sample; nothing else is in the table.
        let param = msgmap! {
            0 => Value::Int(0),
            1 => msgmap! {
                0 => Value::Int(5), 1 => Value::Int(4), 5 => Value::Int(4),
                6 => msgmap! { 28 => Value::Int(0), 29 => Value::Int(1) },
            },
        };
        let mut by_source = vec![Value::Nil; 11];
        by_source[5] = Value::Array(vec![param]);
        let tone = msgmap! { 4 => Value::Array(by_source) };
        let switches = [
            switch(1, &[2, 2]),
            switch(2, &[]),
            // Block 4 is the parameter; block 6 the bypass.
            switch(3, &[6, 4]),
        ];
        let got = footswitches(&switches, &tone);
        let blocks: Vec<(u8, Vec<u32>)> =
            got.iter().map(|s| (s.switch, s.blocks.clone())).collect();
        assert_eq!(blocks, [(1, vec![3]), (2, vec![]), (3, vec![7])]);
        // The same entry on another switch is a bypass.
        assert!(drives_parameter(&tone, 4, 3));
        assert!(!drives_parameter(&tone, 4, 1));
        assert!(!drives_parameter(&Value::Nil, 4, 3));
    }
}
