//! [`FakeDevice`]: replays output recorded from the real device
//! (`server/fixtures/`) and keeps write commands in memory.
//!
//! Reads render the fixtures through the in-memory state, in the same text
//! formats `tonepush` v0.9.0 prints (`crates/tonepush-cli/src/main.rs`), so
//! the API parses fake and real output with the same code. Write replies copy
//! TonePush's own messages; they were not recorded on hardware.
//!
//! Limits: one chain was recorded, so every preset shows it; typed values
//! are stored as typed, not converted to display units.
//!
//! Sliders: model numbers and native values come from the no-catalog
//! recording (`fixtures/no-catalog/chain.txt`, the same preset). Parameter
//! ranges and formats are **synthetic**, written in [`synthetic`] below to
//! reproduce the recorded display values; they are not HX Edit's data.
//!
//! Adding blocks: from HX Edit's catalog when this machine has one
//! (`HX_EDIT_RESOURCES`), else only the four recorded models. A new block
//! starts on, with its model's default values, as the device does.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::controls::{self, CategoryInfo, Kind, ModelGroup, ModelInfo};
use crate::device::{
    BlockNatives, Cause, ChainExtra, Command, Device, DeviceError, Live, Natives, Result,
};
use crate::parse;
use crate::routing;

pub struct FakeDevice {
    dir: PathBuf,
    delay: Duration,
    state: Mutex<State>,
    /// Writes are echoed here, as the real pedal echoes them.
    live: broadcast::Sender<Live>,
}

/// What the writes changed, on top of the fixtures.
#[derive(Default)]
struct State {
    /// Loaded preset index; `None` = the one the fixtures recorded.
    loaded: Option<u32>,
    names: HashMap<u32, String>,
    /// Edit buffer of the loaded preset.
    buffer: Edits,
    /// Edits saved per preset index.
    saved: HashMap<u32, Edits>,
}

#[derive(Default, Clone)]
struct Edits {
    enabled: HashMap<u32, bool>,
    values: HashMap<(u32, u32), String>,
    /// Native values set by slider, by (block, param).
    natives: HashMap<(u32, u32), f32>,
    tempo: Option<f32>,
    /// Active snapshot; `None` = the first (the recording does not say).
    snapshot: Option<u32>,
    /// Recorded blocks taken out, by position.
    removed: HashSet<u32>,
    /// Blocks put in, by position: model number.
    added: BTreeMap<u32, u32>,
    /// Footswitch carrying each block's bypass, by position. The recording
    /// has none: `tonepush chain` does not print them.
    footswitches: BTreeMap<u32, u8>,
    /// Where the split and merge attach, as cells of Path A ([`ROW`]):
    /// (split before, merge before). Only counts while Path B has a block.
    split: Option<(u32, u32)>,
    /// Controller assignments by (position, parameter or `None` for on/off);
    /// `None` = as recorded (`controllers.txt`).
    assignments: Option<Assignments>,
}

/// One controller assignment: source ordinal, travel ends (native values).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Assigned {
    ordinal: i64,
    min: f32,
    max: f32,
}

type Assignments = BTreeMap<(u32, Option<u32>), Assigned>;

/// `tonepush controllers` lines, `block 1 Param(0)  Expression Pedal 1
/// 0%..100%`, as assignments. Blocks count from 0 there. Only expression
/// pedals; the recorded ends span the whole range, so they are the model's
/// range. `models`: model number by position.
fn recorded_assignments(text: &str, models: &HashMap<u32, u32>) -> Assignments {
    let mut found = Assignments::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (["block", block, target, rest @ ..], Some(&pedal)) = (words.as_slice(), words.get(5))
        else {
            continue;
        };
        if rest.get(..2) != Some(&["Expression", "Pedal"][..]) {
            continue;
        }
        let (Ok(block), Ok(ordinal)) = (block.parse::<u32>(), pedal.parse::<i64>()) else {
            continue;
        };
        let position = block + 1;
        let param = match target
            .strip_prefix("Param(")
            .and_then(|t| t.strip_suffix(')'))
        {
            Some(i) => match i.parse::<u32>() {
                Ok(i) => Some(i),
                Err(_) => continue,
            },
            None if *target == "Bypass" => None,
            None => continue,
        };
        let (min, max) = param
            .and_then(|i| {
                let p = describe(*models.get(&position)?)?
                    .params
                    .into_iter()
                    .nth(i as usize)?;
                Some((p.min, p.max))
            })
            .unwrap_or((0.0, 1.0));
        found.insert((position, param), Assigned { ordinal, min, max });
    }
    found
}

impl Edits {
    /// Drop the bypass, values and footswitch kept for a position whose
    /// block changed. A block added over USB gets no footswitch (verified on
    /// the HX Effects, 2026-10-15).
    fn forget(&mut self, position: u32) {
        self.footswitches.remove(&position);
        self.enabled.remove(&position);
        self.values.retain(|(b, _), _| *b != position);
        self.natives.retain(|(b, _), _| *b != position);
    }
}

/// Path A's block slots on the HX Effects, as `chain` numbers them
/// (`topology.txt`: input at slot 0, output at slot 10).
const ROW: RangeInclusive<u32> = 2..=10;

/// Path B: the split, its cells and the join, after the output. **Assumed**,
/// not recorded: TonePush's HX Stomp capture has an empty split and join
/// after the output with a branch cell per main-row cell, so an HX Effects
/// would put the split at 12, Path B at 13-21 and the join at 22.
const SPLIT: u32 = 12;
const BRANCH: RangeInclusive<u32> = 13..=21;
const JOIN: u32 = 22;

/// Path A, Path B and the split and merge, as `usb` reports them.
fn routing(edits: &Edits) -> parse::Routing {
    let divides = edits.added.keys().any(|p| BRANCH.contains(p));
    let at = edits.split.filter(|_| divides);
    let junction = |position, before| parse::Junction { position, before };
    parse::Routing {
        main: ROW.collect(),
        branch: BRANCH.collect(),
        split: Some(junction(SPLIT, at.map(|a| a.0))),
        join: Some(junction(JOIN, at.map(|a| a.1))),
    }
}

/// Footswitches an HX Effects can put a bypass on (TonePush's profile).
const SWITCHES: u8 = 6;

/// HX Edit's catalog, when this machine has one (`HX_EDIT_RESOURCES`).
fn catalog() -> Option<&'static hx_catalog::Catalog> {
    static CATALOG: std::sync::OnceLock<Option<hx_catalog::Catalog>> = std::sync::OnceLock::new();
    CATALOG
        .get_or_init(|| hx_catalog::Catalog::load().ok())
        .as_ref()
}

/// Parameter metadata: synthetic for the recorded models, else the catalog's.
fn describe(model: u32) -> Option<ModelInfo> {
    synthetic::model(model).or_else(|| controls::from_catalog(catalog()?, model))
}

/// A native value as the device would show it.
fn shown(model: u32, param: u32, native: f32) -> String {
    if synthetic::model(model).is_some() {
        return synthetic::format(model, param, native);
    }
    catalog()
        .and_then(|c| c.param(model, param as usize).map(|p| c.format(p, native)))
        .unwrap_or_else(|| native.to_string())
}

/// A typed display value back to a native one.
fn typed(model: u32, param: u32, text: &str) -> Option<f32> {
    synthetic::parse(model, param, text).or_else(|| {
        let c = catalog()?;
        c.parse(c.param(model, param as usize)?, text)
    })
}

/// The pedal the fixtures were recorded on.
const PEDAL: hx_proto::DeviceProfile = hx_proto::HX_EFFECTS;

/// Whether a block can be added with `model`.
fn addable(model: u32) -> bool {
    synthetic::model(model).is_some()
        || catalog().is_some_and(|c| controls::addable(c, &PEDAL, model))
}

/// A block just put in: on, with its model's default values.
fn added_block(position: u32, model: u32) -> Option<parse::Block> {
    let info = describe(model)?;
    Some(parse::Block {
        position,
        model: info.name,
        model_id: None,
        category: None,
        based_on: None,
        enabled: true,
        footswitch: None,
        params: info
            .params
            .iter()
            .map(|p| parse::Param {
                index: p.index,
                name: p.name.clone(),
                value: shown(model, p.index, p.default),
                native: None,
            })
            .collect(),
        cab: None,
        assignments: Vec::new(),
    })
}

impl FakeDevice {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FakeDevice {
            dir: dir.into(),
            delay: Duration::ZERO,
            state: Mutex::default(),
            live: broadcast::channel(16).0,
        }
    }

    /// Wait this long before each reply, to mimic a real `tonepush` call
    /// (1–3 s) when working on the UI's loading states.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Start the loaded preset with Path B in use (`HELIX_FAKE_SPLIT=1`), for
    /// working on the UI's two rows. **Synthetic**: Simple Pitch and Fassel
    /// on Path B's second and third cells, the split before Path A's second
    /// cell and the merge before its sixth (Path B's blocks start under the
    /// split, as in TonePush's DIR:Relief export). No preset with a split was
    /// recorded.
    pub fn with_split_demo(self) -> Self {
        {
            let mut state = self.state();
            state.buffer.added.insert(BRANCH.start() + 1, 296);
            state.buffer.added.insert(BRANCH.start() + 2, 269);
            state.buffer.split = Some((1, 5));
        }
        self
    }

    /// The fixtures committed with this crate.
    pub fn bundled() -> Self {
        Self::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    async fn fixture(&self, name: &str) -> Result<String> {
        let path = self.dir.join(name);
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| DeviceError::Spawn(format!("{}: {e}", path.display())))
    }

    /// The loaded preset's chain, edit buffer applied.
    async fn current(&self) -> Result<parse::Chain> {
        let chain = parse::chain(&self.fixture("chain.txt").await?)?;
        let presets = parse::presets(&self.fixture("presets.txt").await?)?;
        let state = self.state();
        let p = state.loaded_entry(&presets)?;
        Ok(state.apply_chain(chain, &p.label, state.name(p)))
    }

    async fn chain(&self) -> Result<String> {
        Ok(render_chain(&self.current().await?))
    }

    /// Model numbers and recorded natives (no-catalog fixture), with the
    /// edit buffer applied. `None` if that fixture is missing.
    async fn natives(&self) -> Option<Natives> {
        let raw = parse::chain(&self.fixture("no-catalog/chain.txt").await.ok()?).ok()?;
        let state = self.state();
        let edits = &state.buffer;
        let value = |position, model, index, recorded: Option<f32>| match (
            edits.natives.get(&(position, index)),
            edits.values.get(&(position, index)),
        ) {
            (Some(v), _) => Some(*v),
            (None, Some(text)) => typed(model, index, text),
            (None, None) => recorded,
        };
        let mut blocks: Vec<BlockNatives> = raw
            .blocks
            .iter()
            .filter(|b| !edits.removed.contains(&b.position))
            .filter(|b| !edits.added.contains_key(&b.position))
            .filter_map(|b| {
                let model = b.model.strip_prefix("model ")?.parse().ok()?;
                let values = b
                    .params
                    .iter()
                    .map(|p| value(b.position, model, p.index, p.value.parse().ok()))
                    .collect::<Option<Vec<f32>>>()?;
                Some(BlockNatives {
                    position: b.position,
                    model,
                    values,
                })
            })
            .collect();
        for (&position, &model) in &edits.added {
            let Some(info) = describe(model) else {
                continue;
            };
            let values = info
                .params
                .iter()
                .map(|p| value(position, model, p.index, Some(p.default)))
                .collect::<Option<Vec<f32>>>();
            if let Some(values) = values {
                blocks.push(BlockNatives {
                    position,
                    model,
                    values,
                });
            }
        }
        blocks.sort_by_key(|b| b.position);
        Some(Natives(blocks))
    }

    /// Model number recorded at a block position.
    async fn model_at(&self, position: u32) -> Option<u32> {
        let natives = self.natives().await?;
        natives
            .0
            .iter()
            .find(|b| b.position == position)
            .map(|b| b.model)
    }

    /// Publish the chain after a write, like the USB backend does.
    async fn echo(&self, cmd: &Command) {
        let cause = match cmd {
            Command::Select { .. } => Cause::Preset,
            Command::Enable { .. }
            | Command::Set { .. }
            | Command::SetNative { .. }
            | Command::Tempo { .. }
            | Command::AddBlock { .. }
            | Command::RemoveBlock { .. }
            | Command::Footswitch { .. }
            | Command::MoveBlock { .. }
            | Command::Junction { .. }
            | Command::Controller { .. }
            | Command::ControllerRange { .. } => Cause::Edit,
            Command::Rename { .. } | Command::Snapshot { .. } => Cause::Other,
            _ => return,
        };
        if let Ok(text) = self.chain().await {
            let extra = self.extra().await;
            let _ = self.live.send(Live::Chain { text, cause, extra });
        }
    }

    /// Model number by block position, from the natives.
    async fn models(&self) -> HashMap<u32, u32> {
        self.natives()
            .await
            .map(|n| n.0.iter().map(|b| (b.position, b.model)).collect())
            .unwrap_or_default()
    }

    /// The loaded preset's controller assignments: as edited, else as
    /// recorded.
    async fn assignments(&self) -> Assignments {
        if let Some(edited) = self.state().buffer.assignments.clone() {
            return edited;
        }
        let text = self.fixture("controllers.txt").await.unwrap_or_default();
        recorded_assignments(&text, &self.models().await)
    }

    /// Parameter `param` of the block at `block`, which must hold `model` if
    /// given.
    async fn param_info(
        &self,
        block: u32,
        param: u32,
        model: Option<u32>,
    ) -> Result<controls::ParamInfo> {
        let held = self.models().await.get(&block).copied().ok_or_else(|| {
            DeviceError::Stale(format!("no block at position {block}; reload the chain"))
        })?;
        if model.is_some_and(|m| m != held) {
            return Err(DeviceError::Stale(format!(
                "block {block} now holds model {held}, not {}; reload the chain",
                model.unwrap_or_default()
            )));
        }
        describe(held)
            .and_then(|m| m.params.into_iter().find(|p| p.index == param))
            .ok_or_else(|| DeviceError::Invalid(format!("model {held} has no parameter {param}")))
    }

    /// Change the assignments, starting from the recording if untouched.
    async fn edit_assignments(&self, change: impl FnOnce(&mut Assignments)) {
        let mut map = self.assignments().await;
        change(&mut map);
        self.state().buffer.assignments = Some(map);
    }

    async fn extra(&self) -> ChainExtra {
        let natives = self.natives().await;
        let models = self.models().await;
        let assignments = self
            .assignments()
            .await
            .into_iter()
            .map(|((position, param), a)| {
                let shown = |v| Some(shown(*models.get(&position)?, param?, v));
                let ends = param.map(|_| (a.min, a.max));
                (
                    position,
                    parse::Assignment {
                        param,
                        source: parse::source_name(a.ordinal),
                        min: ends.map(|e| e.0),
                        max: ends.map(|e| e.1),
                        min_value: ends.and_then(|e| shown(e.0)),
                        max_value: ends.and_then(|e| shown(e.1)),
                    },
                )
            })
            .collect();
        let state = self.state();
        let footswitches = (1..=SWITCHES)
            .map(|switch| parse::Footswitch {
                switch,
                label: None,
                blocks: state
                    .buffer
                    .footswitches
                    .iter()
                    .filter(|(_, &s)| s == switch)
                    .map(|(&position, _)| position)
                    .collect(),
            })
            .collect();
        ChainExtra {
            natives,
            snapshot: Some(state.buffer.snapshot.unwrap_or(0)),
            footswitches: Some(footswitches),
            routing: Some(routing(&state.buffer)),
            assignments: Some(assignments),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while holding the lock leaves plain data; keep using it.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Device for FakeDevice {
    fn kind(&self) -> &'static str {
        "fake"
    }

    async fn run(&self, cmd: Command) -> Result<String> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let echo = cmd.is_write().then(|| cmd.clone());
        let out = self.reply(cmd).await;
        if let (Ok(_), Some(cmd)) = (&out, echo) {
            self.echo(&cmd).await;
        }
        out
    }

    fn subscribe(&self) -> Option<broadcast::Receiver<Live>> {
        Some(self.live.subscribe())
    }

    async fn chain(&self) -> Result<(String, ChainExtra)> {
        let text = self.run(Command::Chain).await?;
        Ok((text, self.extra().await))
    }

    /// Synthetic for the recorded models; the catalog's for added ones.
    fn model(&self, id: u32) -> Option<ModelInfo> {
        describe(id)
    }

    /// From HX Edit's catalog when this machine has one (`HX_EDIT_RESOURCES`),
    /// so the UI can be checked against real colours without a pedal. The
    /// fixtures' model names are the catalog's.
    /// The fake's model numbers are its own, not the catalog's: go by name.
    fn category(&self, _model_id: Option<u32>, name: &str) -> Option<CategoryInfo> {
        controls::category(catalog()?, None, name)
    }

    /// The catalog's models when this machine has it, else the recorded four.
    fn catalog(&self) -> Option<Vec<ModelGroup>> {
        Some(catalog().map_or_else(synthetic::groups, |c| controls::browser(c, &PEDAL)))
    }
}

impl FakeDevice {
    async fn reply(&self, cmd: Command) -> Result<String> {
        match cmd {
            Command::List => self.fixture("list.txt").await,
            Command::Info => self.fixture("info.txt").await,
            Command::Setlists => self.fixture("setlists.txt").await,
            // Only setlist 0 was recorded.
            Command::Presets { setlist: 0 } => {
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                Ok(render_presets(&self.state().apply_presets(presets)))
            }
            Command::Preset => {
                let out = self.fixture("preset.txt").await?;
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                let state = self.state();
                let p = state.loaded_entry(&presets)?;
                let rest = out.split_once('\n').map_or("", |(_, rest)| rest);
                Ok(format!(
                    "preset:   {} {}  (setlist 0, index {})\n{rest}",
                    p.label,
                    state.name(p),
                    p.index
                ))
            }
            Command::Chain => self.chain().await,
            Command::Select { setlist: 0, index } => {
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                let label = &entry(&presets, index)?.label;
                let mut state = self.state();
                state.loaded = Some(index);
                state.buffer = state.saved.get(&index).cloned().unwrap_or_default();
                Ok(format!("selected {label} (setlist 0)\n"))
            }
            Command::Enable { block, on } => {
                find_block(&self.current().await?, block)?;
                self.state().buffer.enabled.insert(block, on);
                Ok(format!(
                    "block {block} {}\n",
                    if on { "engaged" } else { "bypassed" }
                ))
            }
            Command::Set {
                block,
                param,
                value,
            } => {
                let name = find_block(&self.current().await?, block)?
                    .params
                    .get(param as usize)
                    .map(|p| p.name.clone())
                    .ok_or_else(|| {
                        DeviceError::Failed(format!("no parameter {param} on this block"))
                    })?;
                let value = value.trim().to_owned();
                let mut state = self.state();
                state.buffer.natives.remove(&(block, param));
                state.buffer.values.insert((block, param), value.clone());
                Ok(format!("block {block}: {name} = {value}\n"))
            }
            Command::SetNative {
                block,
                param,
                native,
                model,
            } => {
                let name = find_block(&self.current().await?, block)?
                    .params
                    .get(param as usize)
                    .map(|p| p.name.clone())
                    .ok_or_else(|| {
                        DeviceError::Invalid(format!("no parameter {param} on this block"))
                    })?;
                match self.model_at(block).await {
                    Some(m) if m == model => {}
                    Some(m) => {
                        return Err(DeviceError::Stale(format!(
                            "block {block} now holds model {m}, not {model}; reload the chain"
                        )))
                    }
                    None => {
                        return Err(DeviceError::Invalid(
                            "no recorded model numbers (fixtures/no-catalog)".into(),
                        ))
                    }
                }
                let info = describe(model)
                    .and_then(|m| m.params.into_iter().nth(param as usize))
                    .filter(|p| p.kind != Kind::Text)
                    .ok_or_else(|| {
                        DeviceError::Invalid(format!("model {model} has no parameter {param}"))
                    })?;
                if !(native.is_finite() && native >= info.min && native <= info.max) {
                    return Err(DeviceError::Invalid(format!(
                        "{native} is outside {}..{} for {name}",
                        info.min, info.max
                    )));
                }
                let held = match info.kind {
                    Kind::Menu => native.round(),
                    Kind::Switch if native >= 0.5 => info.max,
                    Kind::Switch => info.min,
                    _ => native,
                };
                let shown = shown(model, param, held);
                let mut state = self.state();
                state.buffer.natives.insert((block, param), held);
                state.buffer.values.insert((block, param), shown.clone());
                Ok(format!("block {block}: {name} = {shown}\n"))
            }
            Command::Rename {
                setlist: 0,
                index,
                name,
            } => {
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                let label = &entry(&presets, index)?.label;
                self.state().names.insert(index, name.clone());
                Ok(format!("{label} renamed to {name}\n"))
            }
            Command::Save => {
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                let mut state = self.state();
                let p = state.loaded_entry(&presets)?;
                let (index, label, name) = (p.index, p.label.clone(), state.name(p).to_owned());
                let buffer = state.buffer.clone();
                state.saved.insert(index, buffer);
                Ok(format!("saved to {label} in setlist 0 as {name:?}\n"))
            }
            Command::Snapshot { index } => {
                let chain = parse::chain(&self.fixture("chain.txt").await?)?;
                if index as usize >= chain.snapshots.len() {
                    // What hx-usb reports for the pedal's refusal.
                    return Err(DeviceError::Failed("the device refused: error -46".into()));
                }
                self.state().buffer.snapshot = Some(index);
                Ok(format!("snapshot {}\n", index + 1))
            }
            Command::Tempo { bpm } => {
                self.state().buffer.tempo = Some(bpm);
                Ok(format!("tempo {bpm:.1} BPM\n"))
            }
            Command::AddBlock { block, model } => {
                let name = describe(model)
                    .filter(|_| addable(model))
                    .map(|m| m.name)
                    .ok_or_else(|| {
                        DeviceError::Invalid(format!(
                            "model {model} is not an effect this device offers"
                        ))
                    })?;
                let place = routing::check_cell(&routing(&self.state().buffer), block)?;
                if let Some(b) = self
                    .current()
                    .await?
                    .blocks
                    .iter()
                    .find(|b| b.position == block)
                {
                    return Err(DeviceError::Stale(format!(
                        "position {block} already holds {}; reload the chain",
                        b.model
                    )));
                }
                self.edit_assignments(|m| m.retain(|&(p, _), _| p != block))
                    .await;
                let mut state = self.state();
                state.buffer.forget(block);
                state.buffer.added.insert(block, model);
                if place == (routing::Place::Branch { claim: true }) {
                    state.buffer.split = Some((0, ROW.count() as u32));
                }
                Ok(format!("block {block} is now {name}\n"))
            }
            Command::RemoveBlock { block, model } => {
                let chain = self.current().await?;
                let name = chain
                    .blocks
                    .iter()
                    .find(|b| b.position == block)
                    .map(|b| b.model.clone())
                    .ok_or_else(|| {
                        DeviceError::Stale(format!(
                            "no block at position {block}; reload the chain"
                        ))
                    })?;
                if let (Some(want), Some(held)) = (model, self.model_at(block).await) {
                    if want != held {
                        return Err(DeviceError::Stale(format!(
                            "block {block} now holds model {held}, not {want}; reload the chain"
                        )));
                    }
                }
                self.edit_assignments(|m| m.retain(|&(p, _), _| p != block))
                    .await;
                let mut state = self.state();
                state.buffer.forget(block);
                state.buffer.added.remove(&block);
                state.buffer.removed.insert(block);
                // Path B's last block gone: the device zeroes the attach points.
                if !state.buffer.added.keys().any(|p| BRANCH.contains(p)) {
                    state.buffer.split = None;
                }
                Ok(format!("removed {name} from block {block}\n"))
            }
            Command::Footswitch {
                block,
                switch,
                model,
            } => {
                if switch.is_some_and(|s| !(1..=SWITCHES).contains(&s)) {
                    return Err(DeviceError::Invalid(format!(
                        "HX Effects has footswitches 1 to {SWITCHES}"
                    )));
                }
                if !self
                    .current()
                    .await?
                    .blocks
                    .iter()
                    .any(|b| b.position == block)
                {
                    return Err(DeviceError::Stale(format!(
                        "no block at position {block}; reload the chain"
                    )));
                }
                if let (Some(want), Some(held)) = (model, self.model_at(block).await) {
                    if want != held {
                        return Err(DeviceError::Stale(format!(
                            "block {block} now holds model {held}, not {want}; reload the chain"
                        )));
                    }
                }
                let mut state = self.state();
                Ok(match switch {
                    Some(s) => {
                        state.buffer.footswitches.insert(block, s);
                        format!("block {block} on footswitch {s}\n")
                    }
                    None => {
                        state.buffer.footswitches.remove(&block);
                        format!("block {block} on no footswitch\n")
                    }
                })
            }
            Command::MoveBlock { block, to, model } => {
                let chain = self.current().await?;
                let moving = chain
                    .blocks
                    .iter()
                    .find(|b| b.position == block)
                    .ok_or_else(|| {
                        DeviceError::Stale(format!(
                            "no block at position {block}; reload the chain"
                        ))
                    })?;
                let natives = self.natives().await;
                let held = natives
                    .as_ref()
                    .and_then(|n| n.0.iter().find(|b| b.position == block));
                if let (Some(want), Some(h)) = (model, held) {
                    if want != h.model {
                        return Err(DeviceError::Stale(format!(
                            "block {block} now holds model {}, not {want}; reload the chain",
                            h.model
                        )));
                    }
                }
                let held = held.ok_or_else(|| {
                    DeviceError::Failed(format!("no model number recorded for block {block}"))
                })?;
                let place = routing::check_cell(&routing(&self.state().buffer), to)?;
                if let Some(b) = chain.blocks.iter().find(|b| b.position == to) {
                    return Err(DeviceError::Stale(format!(
                        "position {to} already holds {}; reload the chain",
                        b.model
                    )));
                }
                // The block keeps its controller assignments too.
                self.edit_assignments(|m| {
                    let moved: Vec<_> = m.keys().filter(|k| k.0 == block).copied().collect();
                    for k in moved {
                        if let Some(a) = m.remove(&k) {
                            m.insert((to, k.1), a);
                        }
                    }
                })
                .await;
                let mut state = self.state();
                let edits = &mut state.buffer;
                // The block keeps its values, bypass and footswitch.
                let switch = edits.footswitches.get(&block).copied();
                edits.forget(block);
                edits.added.remove(&block);
                edits.removed.insert(block);
                edits.added.insert(to, held.model);
                edits.enabled.insert(to, moving.enabled);
                if let Some(s) = switch {
                    edits.footswitches.insert(to, s);
                }
                for (p, &v) in moving.params.iter().zip(&held.values) {
                    edits.natives.insert((to, p.index), v);
                    edits.values.insert((to, p.index), p.value.clone());
                }
                if place == (routing::Place::Branch { claim: true }) {
                    edits.split = Some((0, ROW.count() as u32));
                }
                let on_b = chain
                    .blocks
                    .iter()
                    .map(|b| b.position)
                    .filter(|&p| p != block)
                    .chain([to])
                    .any(|p| BRANCH.contains(&p));
                if !on_b {
                    edits.split = None;
                }
                Ok(format!(
                    "moved {} from block {block} to block {to}\n",
                    moving.model
                ))
            }
            Command::Junction { which, before } => {
                let chain = self.current().await?;
                let occupied = |p| chain.blocks.iter().any(|b| b.position == p);
                let mut state = self.state();
                routing::check_junction(&routing(&state.buffer), occupied, which, before)?;
                let (split, join) = state.buffer.split.unwrap_or((0, ROW.count() as u32));
                let (split, join, name) = match which {
                    routing::Junction::Split => (before, join, "split"),
                    routing::Junction::Join => (split, before, "merge"),
                };
                state.buffer.split = Some((split, join));
                Ok(format!("{name} now before cell {}\n", before + 1))
            }
            Command::Controller {
                block,
                param,
                pedal,
                model,
            } => {
                if let Some(n) = pedal.filter(|n| !(1..=2).contains(n)) {
                    return Err(DeviceError::Invalid(format!(
                        "expression pedals are 1 and 2; {n} is not one"
                    )));
                }
                let p = self.param_info(block, param, model).await?;
                self.edit_assignments(|map| match pedal {
                    Some(n) => {
                        let fresh = Assigned {
                            ordinal: i64::from(n),
                            min: p.min,
                            max: p.max,
                        };
                        map.insert((block, Some(param)), fresh);
                    }
                    None => {
                        map.remove(&(block, Some(param)));
                    }
                })
                .await;
                Ok(match pedal {
                    Some(n) => format!("{} on expression pedal {n}\n", p.name),
                    None => format!("{} on no controller\n", p.name),
                })
            }
            Command::ControllerRange {
                block,
                param,
                min,
                max,
                model,
            } => {
                let p = self.param_info(block, param, model).await?;
                if !self.assignments().await.contains_key(&(block, Some(param))) {
                    return Err(DeviceError::Invalid(format!(
                        "{} is not under a controller; assign one first",
                        p.name
                    )));
                }
                if min.is_none() && max.is_none() {
                    return Err(DeviceError::Invalid("give min, max or both".into()));
                }
                for v in [min, max].into_iter().flatten() {
                    if !v.is_finite() || !(p.min..=p.max).contains(&v) {
                        return Err(DeviceError::Invalid(format!(
                            "{} runs from {} to {}; {v} is outside it",
                            p.name, p.min, p.max
                        )));
                    }
                }
                self.edit_assignments(|map| {
                    if let Some(a) = map.get_mut(&(block, Some(param))) {
                        a.min = min.unwrap_or(a.min);
                        a.max = max.unwrap_or(a.max);
                    }
                })
                .await;
                Ok(format!("{} travel set\n", p.name))
            }
            other @ (Command::Presets { .. } | Command::Select { .. } | Command::Rename { .. }) => {
                Err(DeviceError::NoFixture(other))
            }
        }
    }
}

fn entry(presets: &[parse::PresetEntry], index: u32) -> Result<&parse::PresetEntry> {
    presets.get(index as usize).ok_or_else(|| {
        DeviceError::Failed(format!(
            "{index:?} is not a preset; use a label like 03B or an index like 7"
        ))
    })
}

fn find_block(chain: &parse::Chain, position: u32) -> Result<&parse::Block> {
    chain
        .blocks
        .iter()
        .find(|b| b.position == position)
        .ok_or_else(|| {
            DeviceError::Failed(format!(
                "no block at position {position}; try `tonepush chain`"
            ))
        })
}

impl State {
    fn loaded_entry<'a>(
        &self,
        presets: &'a [parse::PresetEntry],
    ) -> Result<&'a parse::PresetEntry> {
        match self.loaded {
            Some(index) => entry(presets, index),
            None => presets
                .iter()
                .find(|p| p.loaded)
                .ok_or_else(|| DeviceError::Parse("no loaded preset in presets.txt".into())),
        }
    }

    fn name<'a>(&'a self, p: &'a parse::PresetEntry) -> &'a str {
        self.names.get(&p.index).map_or(&p.name, String::as_str)
    }

    fn apply_presets(&self, presets: Vec<parse::PresetEntry>) -> Vec<parse::PresetEntry> {
        let loaded = self.loaded_entry(&presets).map(|p| p.index).ok();
        presets
            .iter()
            .map(|p| parse::PresetEntry {
                name: self.name(p).to_owned(),
                loaded: Some(p.index) == loaded,
                ..p.clone()
            })
            .collect()
    }

    fn apply_chain(&self, mut chain: parse::Chain, label: &str, name: &str) -> parse::Chain {
        chain.label = Some(label.to_owned());
        chain.name = Some(name.to_owned());
        if let Some(bpm) = self.buffer.tempo {
            chain.tempo = Some(bpm);
        }
        let edits = &self.buffer;
        chain.blocks.retain(|b| {
            !edits.removed.contains(&b.position) && !edits.added.contains_key(&b.position)
        });
        chain.blocks.extend(
            edits
                .added
                .iter()
                .filter_map(|(&position, &model)| added_block(position, model)),
        );
        chain.blocks.sort_by_key(|b| b.position);
        for block in &mut chain.blocks {
            if let Some(&on) = self.buffer.enabled.get(&block.position) {
                block.enabled = on;
            }
            for param in &mut block.params {
                if let Some(v) = self.buffer.values.get(&(block.position, param.index)) {
                    param.value = v.clone();
                }
            }
        }
        chain
    }
}

/// `list_presets` format: `{label} {name:<24} {marker}`.
fn render_presets(presets: &[parse::PresetEntry]) -> String {
    let mut out = String::new();
    for p in presets {
        let marker = if p.loaded { "<- loaded" } else { "" };
        let _ = writeln!(out, "{} {:<24} {marker}", p.label, p.name);
    }
    out
}

/// `show_chain` format.
fn render_chain(chain: &parse::Chain) -> String {
    let mut out = String::new();
    if let (Some(label), Some(name)) = (&chain.label, &chain.name) {
        let _ = write!(out, "{label} {name}");
    }
    if let Some(tempo) = chain.tempo {
        let _ = write!(out, "   {tempo:.1} BPM");
    }
    out.push('\n');
    if !chain.snapshots.is_empty() {
        let _ = writeln!(out, "snapshots: {}", chain.snapshots.join(", "));
    }
    out.push('\n');
    let params = |out: &mut String, params: &[parse::Param]| {
        for p in params {
            let _ = writeln!(out, "      {:<18} {}", p.name, p.value);
        }
    };
    for b in &chain.blocks {
        let _ = writeln!(
            out,
            "{:>2}. {:<24} {}",
            b.position,
            b.model,
            if b.enabled { "" } else { "(bypassed)" }
        );
        params(&mut out, &b.params);
        if let Some(cab) = &b.cab {
            let _ = writeln!(out, "    + {}", cab.model);
            params(&mut out, &cab.params);
        }
    }
    out
}

/// Made-up parameter metadata for the four recorded models, so sliders can
/// be built and tested without HX Edit's catalog. **Synthetic**: names come
/// from the recorded chain; kinds, ranges and formats were chosen to print the
/// recorded natives (`no-catalog/chain.txt`) as the recorded display values
/// (`chain.txt`). Real ranges come from the catalog on the Pi.
mod synthetic {
    use super::*;

    #[derive(Clone, Copy)]
    enum Fmt {
        /// 0..1 shown `40 %`.
        Percent,
        /// `+0.0 dB`.
        SignedDb,
        /// `-22.1 dB`.
        Db,
        /// `455 Hz`, `2.2 kHz`.
        Hz,
        /// Native ms, `0.0 ms`.
        Ms,
        /// Native seconds shown `431 ms`.
        SecondsAsMs,
        /// 0..1 shown `0.0`..`10.0`.
        Knob,
        /// `+0.00`.
        Signed2,
        /// Whole numbers with a sign, `+7`.
        Semitones,
        /// `Off` / `On`.
        OnOff,
    }

    /// (name, kind, min, max, default, format) per parameter.
    type Spec = (&'static str, Kind, f32, f32, f32, Fmt);

    fn specs(model: u32) -> Option<(&'static str, &'static [Spec])> {
        use Fmt::*;
        use Kind::*;
        const LEVEL: Spec = ("Level", Continuous, -60.0, 12.0, 0.0, SignedDb);
        const MIX: Spec = ("Mix", Continuous, 0.0, 1.0, 1.0, Percent);
        Some(match model {
            269 => (
                "Fassel",
                &[
                    ("Position", Continuous, 0.0, 1.0, 0.5, Percent),
                    ("Fc Low", Continuous, 100.0, 2000.0, 450.0, Hz),
                    ("Fc High", Continuous, 1000.0, 10000.0, 2200.0, Hz),
                    MIX,
                    LEVEL,
                ],
            ),
            296 => (
                "Simple Pitch",
                &[
                    ("Interval", Menu, -24.0, 24.0, 0.0, Semitones),
                    ("Cents", Continuous, -50.0, 50.0, 0.0, Signed2),
                    ("Delay", Continuous, 0.0, 300.0, 0.0, Ms),
                    ("Shift Level", Continuous, 0.0, 1.0, 1.0, Knob),
                    ("Mix", Continuous, 0.0, 1.0, 0.5, Percent),
                    LEVEL,
                ],
            ),
            141 => (
                "FX Loop 1",
                &[
                    ("Send", Continuous, -60.0, 12.0, 0.0, SignedDb),
                    ("Return", Continuous, -60.0, 12.0, 0.0, SignedDb),
                    MIX,
                    ("param 3", Switch, 0.0, 1.0, 0.0, OnOff),
                ],
            ),
            123 => (
                "Noise Gate",
                &[
                    ("Threshold", Continuous, -96.0, 0.0, -48.0, Db),
                    ("Decay", Continuous, 0.001, 1.0, 0.1, SecondsAsMs),
                    LEVEL,
                ],
            ),
            _ => return None,
        })
    }

    fn render(fmt: Fmt, v: f32) -> String {
        match fmt {
            Fmt::Percent => format!("{:.0} %", v * 100.0),
            Fmt::SignedDb => format!("{v:+.1} dB"),
            Fmt::Db => format!("{v:.1} dB"),
            Fmt::Hz if v >= 1000.0 => format!("{:.1} kHz", v / 1000.0),
            Fmt::Hz => format!("{v:.0} Hz"),
            Fmt::Ms => format!("{v:.1} ms"),
            Fmt::SecondsAsMs => format!("{:.0} ms", v * 1000.0),
            Fmt::Knob => format!("{:.1}", v * 10.0),
            Fmt::Signed2 => format!("{v:+.2}"),
            Fmt::Semitones => format!("{:+}", v.round() as i32),
            Fmt::OnOff => if v >= 0.5 { "On" } else { "Off" }.to_owned(),
        }
    }

    pub(super) fn model(id: u32) -> Option<ModelInfo> {
        let (name, specs) = specs(id)?;
        let params = specs
            .iter()
            .enumerate()
            .map(|(i, &(name, kind, min, max, default, fmt))| {
                controls::describe(i as u32, name, kind, min, max, default, &|v| render(fmt, v))
            })
            .collect();
        Some(ModelInfo {
            id,
            name: name.to_owned(),
            synthetic: true,
            params,
        })
    }

    /// The model browser without the catalog: the four recorded models, in
    /// their HX Edit categories and tints (app.css `--cat-*`).
    pub(super) fn groups() -> Vec<ModelGroup> {
        [
            ("Dynamics", "Dyn", "#ddcc00", 123),
            ("Pitch/Synth", "Pitch", "#ad46e2", 296),
            ("Wah", "Wah", "#a844db", 269),
            ("Send/Return", "Snd/Rtn", "#38a696", 141),
        ]
        .into_iter()
        .map(|(category, short, colour, id)| {
            let name = specs(id).map_or_else(String::new, |(n, _)| n.to_owned());
            let shelves = vec![controls::Shelf {
                name: None,
                models: vec![controls::ModelEntry {
                    id,
                    effect_type: crate::types::of(category, &name).map(str::to_owned),
                    name,
                    based_on: None,
                }],
            }];
            ModelGroup {
                name: category.into(),
                short: short.into(),
                colour: colour.into(),
                types: controls::used_types(category, &shelves),
                shelves,
            }
        })
        .collect()
    }

    pub(super) fn format(model: u32, param: u32, native: f32) -> String {
        specs(model)
            .and_then(|(_, s)| s.get(param as usize))
            .map_or_else(|| native.to_string(), |s| render(s.5, native))
    }

    /// A typed display value back to a native one: the number, unscaled.
    pub(super) fn parse(model: u32, param: u32, text: &str) -> Option<f32> {
        let spec = specs(model)?.1.get(param as usize)?;
        let end = text
            .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+'))
            .unwrap_or(text.len());
        let n: f32 = text[..end].trim().parse().ok()?;
        let unit = text[end..].trim();
        Some(match spec.5 {
            Fmt::Percent => n / 100.0,
            Fmt::Knob => n / 10.0,
            Fmt::SecondsAsMs => n / 1000.0,
            Fmt::Hz if unit == "kHz" => n * 1000.0,
            _ => n,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn chain(dev: &FakeDevice) -> parse::Chain {
        parse::chain(&dev.run(Command::Chain).await.unwrap()).unwrap()
    }

    async fn presets(dev: &FakeDevice) -> Vec<parse::PresetEntry> {
        parse::presets(&dev.run(Command::Presets { setlist: 0 }).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn replays_fixtures() {
        let out = FakeDevice::bundled().run(Command::Info).await.unwrap();
        assert!(out.starts_with("HX Effects"));
    }

    #[tokio::test]
    async fn has_no_other_setlists() {
        let err = FakeDevice::bundled()
            .run(Command::Presets { setlist: 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, DeviceError::NoFixture(_)));
    }

    /// Rendering through an empty state gives back what the fixtures hold.
    #[tokio::test]
    async fn renders_fixtures_unchanged() {
        let dev = FakeDevice::bundled();
        let recorded = parse::chain(&dev.fixture("chain.txt").await.unwrap()).unwrap();
        assert_eq!(chain(&dev).await, recorded);
        let recorded = parse::presets(&dev.fixture("presets.txt").await.unwrap()).unwrap();
        assert_eq!(presets(&dev).await, recorded);
        let recorded = parse::preset(&dev.fixture("preset.txt").await.unwrap()).unwrap();
        let rendered = parse::preset(&dev.run(Command::Preset).await.unwrap()).unwrap();
        assert_eq!(rendered, recorded);
    }

    #[tokio::test]
    async fn edits_select_and_save() {
        let dev = FakeDevice::bundled();
        let out = dev
            .run(Command::Set {
                block: 2,
                param: 0,
                value: "40 %".into(),
            })
            .await
            .unwrap();
        assert_eq!(out, "block 2: Position = 40 %\n");
        dev.run(Command::Enable { block: 2, on: true })
            .await
            .unwrap();
        let c = chain(&dev).await;
        assert_eq!(c.blocks[0].params[0].value, "40 %");
        assert!(c.blocks[0].enabled);

        // Loading another preset drops unsaved edits.
        let out = dev
            .run(Command::Select {
                setlist: 0,
                index: 7,
            })
            .await
            .unwrap();
        assert_eq!(out, "selected 02D (setlist 0)\n");
        let c = chain(&dev).await;
        assert_eq!(c.label.as_deref(), Some("02D"));
        assert_eq!(c.blocks[0].params[0].value, "2 %");
        assert!(presets(&dev).await[7].loaded);
        assert!(!presets(&dev).await[2].loaded);

        // Saved edits come back when the preset is loaded again.
        dev.run(Command::Enable { block: 3, on: true })
            .await
            .unwrap();
        let out = dev.run(Command::Save).await.unwrap();
        assert_eq!(out, "saved to 02D in setlist 0 as \"New Preset\"\n");
        dev.run(Command::Select {
            setlist: 0,
            index: 0,
        })
        .await
        .unwrap();
        assert!(!chain(&dev).await.blocks[1].enabled);
        dev.run(Command::Select {
            setlist: 0,
            index: 7,
        })
        .await
        .unwrap();
        assert!(chain(&dev).await.blocks[1].enabled);
    }

    #[tokio::test]
    async fn rename() {
        let dev = FakeDevice::bundled();
        let out = dev
            .run(Command::Rename {
                setlist: 0,
                index: 2,
                name: "Clean Verb".into(),
            })
            .await
            .unwrap();
        assert_eq!(out, "01C renamed to Clean Verb\n");
        assert_eq!(presets(&dev).await[2].name, "Clean Verb");
        assert_eq!(chain(&dev).await.name.as_deref(), Some("Clean Verb"));
        let p = parse::preset(&dev.run(Command::Preset).await.unwrap()).unwrap();
        assert_eq!(p.name, "Clean Verb");
    }

    /// The synthetic formats print every recorded native as recorded.
    #[tokio::test]
    async fn synthetic_formats_match_the_recording() {
        let dev = FakeDevice::bundled();
        let shown = chain(&dev).await;
        let natives = dev.natives().await.unwrap();
        assert_eq!(natives.0.len(), shown.blocks.len());
        for (block, n) in shown.blocks.iter().zip(&natives.0) {
            let info = synthetic::model(n.model).unwrap();
            assert!(info.synthetic);
            assert_eq!(info.params.len(), block.params.len(), "{}", block.model);
            for (p, native) in block.params.iter().zip(&n.values) {
                if p.name == "param 3" {
                    continue; // unnamed in the catalog: recorded raw
                }
                assert_eq!(info.params[p.index as usize].name, p.name);
                assert_eq!(
                    synthetic::format(n.model, p.index, *native),
                    p.value,
                    "{} {}",
                    block.model,
                    p.name
                );
            }
        }
    }

    #[tokio::test]
    async fn set_native() {
        let dev = FakeDevice::bundled();
        let set = |block, param, native, model| Command::SetNative {
            block,
            param,
            native,
            model,
        };
        let out = dev.run(set(2, 0, 0.4, 269)).await.unwrap();
        assert_eq!(out, "block 2: Position = 40 %\n");
        // Menus round, switches snap.
        let out = dev.run(set(3, 0, 4.6, 296)).await.unwrap();
        assert_eq!(out, "block 3: Interval = +5\n");
        let out = dev.run(set(5, 3, 0.8, 141)).await.unwrap();
        assert_eq!(out, "block 5: param 3 = On\n");

        let (text, extra) = Device::chain(&dev).await.unwrap();
        let mut c = parse::chain(&text).unwrap();
        parse::add_extra(&mut c, &extra);
        assert_eq!(c.blocks[0].params[0].value, "40 %");
        assert_eq!(c.blocks[0].params[0].native, Some(0.4));
        assert_eq!(c.blocks[1].params[0].native, Some(5.0));
        assert_eq!(c.blocks[2].params[3].native, Some(1.0));
        assert_eq!(c.blocks[0].model_id, Some(269));

        // A typed value moves the native too.
        dev.run(Command::Set {
            block: 2,
            param: 1,
            value: "1.2 kHz".into(),
        })
        .await
        .unwrap();
        let (text, extra) = Device::chain(&dev).await.unwrap();
        let mut c = parse::chain(&text).unwrap();
        parse::add_extra(&mut c, &extra);
        assert_eq!(c.blocks[0].params[1].native, Some(1200.0));

        for (cmd, stale) in [
            (set(2, 0, 1.5, 269), false),
            (set(2, 0, f32::NAN, 269), false),
            (set(2, 9, 0.5, 269), false),
            (set(2, 0, 0.5, 296), true),
        ] {
            match dev.run(cmd.clone()).await {
                Err(DeviceError::Stale(_)) if stale => {}
                Err(DeviceError::Invalid(_) | DeviceError::Failed(_)) if !stale => {}
                other => panic!("{cmd:?}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn add_and_remove_blocks() {
        let dev = FakeDevice::bundled();
        // Noise Gate (123) into the empty slot 4, between Simple Pitch and FX Loop.
        let out = dev
            .run(Command::AddBlock {
                block: 4,
                model: 123,
            })
            .await
            .unwrap();
        assert_eq!(out, "block 4 is now Noise Gate\n");
        let c = chain(&dev).await;
        let positions: Vec<u32> = c.blocks.iter().map(|b| b.position).collect();
        assert_eq!(positions, [2, 3, 4, 5, 10]);
        let added = &c.blocks[2];
        assert_eq!(added.model, "Noise Gate");
        assert!(added.enabled, "a new block starts on");
        assert_eq!(added.params[0].value, "-48.0 dB", "the model's default");

        // The new block takes edits like any other.
        dev.run(Command::SetNative {
            block: 4,
            param: 0,
            native: -30.0,
            model: 123,
        })
        .await
        .unwrap();
        dev.run(Command::Enable {
            block: 4,
            on: false,
        })
        .await
        .unwrap();
        let (text, extra) = Device::chain(&dev).await.unwrap();
        let mut c = parse::chain(&text).unwrap();
        parse::add_extra(&mut c, &extra);
        assert_eq!(c.blocks[2].params[0].value, "-30.0 dB");
        assert_eq!(c.blocks[2].params[0].native, Some(-30.0));
        assert_eq!(c.blocks[2].model_id, Some(123));
        assert!(!c.blocks[2].enabled);

        // Remove a recorded block, with the model the client saw.
        let out = dev
            .run(Command::RemoveBlock {
                block: 2,
                model: Some(269),
            })
            .await
            .unwrap();
        assert_eq!(out, "removed Fassel from block 2\n");
        // And the added one, without.
        let out = dev
            .run(Command::RemoveBlock {
                block: 4,
                model: None,
            })
            .await
            .unwrap();
        assert_eq!(out, "removed Noise Gate from block 4\n");
        let c = chain(&dev).await;
        let positions: Vec<u32> = c.blocks.iter().map(|b| b.position).collect();
        assert_eq!(positions, [3, 5, 10]);

        // A slot freed by a removal takes a new block, which starts afresh.
        dev.run(Command::AddBlock {
            block: 2,
            model: 296,
        })
        .await
        .unwrap();
        let c = chain(&dev).await;
        assert_eq!(c.blocks[0].model, "Simple Pitch");
        assert_eq!(c.blocks[0].params[0].value, "+0");

        // Loading another preset drops it all; saving keeps it.
        dev.run(Command::Save).await.unwrap();
        dev.run(Command::Select {
            setlist: 0,
            index: 0,
        })
        .await
        .unwrap();
        assert_eq!(chain(&dev).await.blocks.len(), 4);
        dev.run(Command::Select {
            setlist: 0,
            index: 2,
        })
        .await
        .unwrap();
        assert_eq!(chain(&dev).await.blocks[0].model, "Simple Pitch");
    }

    #[tokio::test]
    async fn add_and_remove_refusals() {
        let dev = FakeDevice::bundled();
        let add = |block, model| Command::AddBlock { block, model };
        let remove = |block, model| Command::RemoveBlock { block, model };
        for (cmd, stale) in [
            // Taken: add never replaces.
            (add(2, 123), true),
            // Outside Path A's slots: the input, the output, beyond.
            (add(1, 123), false),
            (add(11, 123), false),
            // Not a model this device offers.
            (add(4, 999_999), false),
            // Nothing there, or another model than the client saw.
            (remove(4, None), true),
            (remove(2, Some(296)), true),
        ] {
            match dev.run(cmd.clone()).await {
                Err(DeviceError::Stale(_)) if stale => {}
                Err(DeviceError::Invalid(_)) if !stale => {}
                other => panic!("{cmd:?}: {other:?}"),
            }
        }
        // Nothing changed.
        assert_eq!(chain(&dev).await.blocks.len(), 4);
    }

    /// The chain with footswitches joined in, as `/api/chain` builds it.
    async fn full(dev: &FakeDevice) -> parse::Chain {
        let (text, extra) = Device::chain(dev).await.unwrap();
        let mut c = parse::chain(&text).unwrap();
        parse::add_extra(&mut c, &extra);
        c
    }

    #[tokio::test]
    async fn one_row_until_path_b_has_a_block() {
        let routing = full(&FakeDevice::bundled()).await.routing.unwrap();
        assert_eq!(routing.main, (2..=10).collect::<Vec<_>>());
        assert_eq!(routing.branch, (13..=21).collect::<Vec<_>>());
        let (split, join) = (routing.split.unwrap(), routing.join.unwrap());
        assert_eq!((split.position, split.before), (12, None));
        assert_eq!((join.position, join.before), (22, None));
    }

    #[tokio::test]
    async fn the_split_demo_puts_blocks_on_path_b() {
        let c = full(&FakeDevice::bundled().with_split_demo()).await;
        let routing = c.routing.as_ref().unwrap();
        assert_eq!(routing.split.as_ref().unwrap().before, Some(1));
        assert_eq!(routing.join.as_ref().unwrap().before, Some(5));
        let on_b: Vec<_> = c
            .blocks
            .iter()
            .filter(|b| routing.branch.contains(&b.position))
            .map(|b| (b.position, b.model.as_str()))
            .collect();
        assert_eq!(on_b, [(14, "Simple Pitch"), (15, "Fassel")]);
    }

    fn switch_of(c: &parse::Chain, position: u32) -> Option<u8> {
        c.blocks
            .iter()
            .find(|b| b.position == position)
            .unwrap()
            .footswitch
    }

    #[tokio::test]
    async fn assign_and_unassign_footswitches() {
        let dev = FakeDevice::bundled();
        let fs = |block, switch, model| Command::Footswitch {
            block,
            switch,
            model,
        };
        let c = full(&dev).await;
        let switches = c.footswitches.as_ref().unwrap();
        assert_eq!(switches.len(), 6, "an HX Effects has six");
        assert!(switches.iter().all(|s| s.blocks.is_empty()));
        assert!(c.blocks.iter().all(|b| b.footswitch.is_none()));

        let out = dev.run(fs(5, Some(3), Some(141))).await.unwrap();
        assert_eq!(out, "block 5 on footswitch 3\n");
        dev.run(fs(10, Some(3), None)).await.unwrap();
        let c = full(&dev).await;
        assert_eq!(switch_of(&c, 5), Some(3));
        assert_eq!(switch_of(&c, 10), Some(3), "a switch can carry several");
        assert_eq!(c.footswitches.as_ref().unwrap()[2].blocks, [5, 10]);

        // Moving it takes it off the old switch.
        dev.run(fs(5, Some(6), None)).await.unwrap();
        let c = full(&dev).await;
        assert_eq!(switch_of(&c, 5), Some(6));
        assert_eq!(c.footswitches.as_ref().unwrap()[2].blocks, [10]);

        let out = dev.run(fs(5, None, Some(141))).await.unwrap();
        assert_eq!(out, "block 5 on no footswitch\n");
        assert_eq!(switch_of(&full(&dev).await, 5), None);

        // Kept in the edit buffer: saved with the preset, gone with a load.
        dev.run(Command::Save).await.unwrap();
        dev.run(Command::Select {
            setlist: 0,
            index: 0,
        })
        .await
        .unwrap();
        assert_eq!(switch_of(&full(&dev).await, 10), None);
        dev.run(Command::Select {
            setlist: 0,
            index: 2,
        })
        .await
        .unwrap();
        assert_eq!(switch_of(&full(&dev).await, 10), Some(3));

        // A removed block takes its footswitch with it.
        dev.run(Command::RemoveBlock {
            block: 10,
            model: None,
        })
        .await
        .unwrap();
        let c = full(&dev).await;
        assert!(c.footswitches.unwrap().iter().all(|s| s.blocks.is_empty()));
    }

    #[tokio::test]
    async fn footswitch_refusals() {
        let dev = FakeDevice::bundled();
        let fs = |block, switch, model| Command::Footswitch {
            block,
            switch,
            model,
        };
        for (cmd, stale) in [
            // No such switch on an HX Effects.
            (fs(5, Some(0), None), false),
            (fs(5, Some(7), None), false),
            // Nothing there, or another model than the client saw.
            (fs(4, Some(1), None), true),
            (fs(5, Some(1), Some(123)), true),
        ] {
            match dev.run(cmd.clone()).await {
                Err(DeviceError::Stale(_)) if stale => {}
                Err(DeviceError::Invalid(_)) if !stale => {}
                other => panic!("{cmd:?}: {other:?}"),
            }
        }
        let c = full(&dev).await;
        assert!(c.blocks.iter().all(|b| b.footswitch.is_none()));
    }

    #[tokio::test]
    async fn offers_models() {
        let groups = FakeDevice::bundled().catalog().unwrap();
        let ids: Vec<u32> = groups
            .iter()
            .flat_map(|g| &g.shelves)
            .flat_map(|s| &s.models)
            .map(|m| m.id)
            .collect();
        // Without the catalog: the four recorded models. With it: those too.
        for id in [123, 296] {
            assert!(ids.contains(&id), "{id}");
        }
        assert!(ids.iter().all(|&id| addable(id)));
    }

    #[tokio::test]
    async fn rejects_missing_targets() {
        let dev = FakeDevice::bundled();
        for cmd in [
            Command::Enable { block: 4, on: true },
            Command::Set {
                block: 2,
                param: 9,
                value: "1".into(),
            },
            Command::Select {
                setlist: 0,
                index: 128,
            },
        ] {
            let err = dev.run(cmd.clone()).await.unwrap_err();
            assert!(matches!(err, DeviceError::Failed(_)), "{cmd:?}: {err}");
        }
    }
}
