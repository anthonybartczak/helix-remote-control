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

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::controls::{self, CategoryInfo, Kind, ModelInfo};
use crate::device::{
    BlockNatives, Cause, ChainExtra, Command, Device, DeviceError, Live, Natives, Result,
};
use crate::parse;

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

    async fn chain(&self) -> Result<String> {
        let chain = parse::chain(&self.fixture("chain.txt").await?)?;
        let presets = parse::presets(&self.fixture("presets.txt").await?)?;
        let state = self.state();
        let p = state.loaded_entry(&presets)?;
        let chain = state.apply_chain(chain, &p.label, state.name(p));
        Ok(render_chain(&chain))
    }

    /// Model numbers and recorded natives (no-catalog fixture), with the
    /// edit buffer applied. `None` if that fixture is missing.
    async fn natives(&self) -> Option<Natives> {
        let raw = parse::chain(&self.fixture("no-catalog/chain.txt").await.ok()?).ok()?;
        let state = self.state();
        let blocks = raw
            .blocks
            .iter()
            .filter_map(|b| {
                let model = b.model.strip_prefix("model ")?.parse().ok()?;
                let values = b
                    .params
                    .iter()
                    .map(|p| {
                        let set = state.buffer.natives.get(&(b.position, p.index));
                        let typed = state.buffer.values.get(&(b.position, p.index));
                        match (set, typed) {
                            (Some(v), _) => Some(*v),
                            (None, Some(text)) => synthetic::parse(model, p.index, text),
                            (None, None) => p.value.parse().ok(),
                        }
                    })
                    .collect::<Option<Vec<f32>>>()?;
                Some(BlockNatives {
                    position: b.position,
                    model,
                    values,
                })
            })
            .collect();
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
            | Command::Tempo { .. } => Cause::Edit,
            Command::Rename { .. } | Command::Snapshot { .. } => Cause::Other,
            _ => return,
        };
        if let Ok(text) = self.chain().await {
            let extra = self.extra().await;
            let _ = self.live.send(Live::Chain { text, cause, extra });
        }
    }

    async fn extra(&self) -> ChainExtra {
        let natives = self.natives().await;
        ChainExtra {
            natives,
            snapshot: Some(self.state().buffer.snapshot.unwrap_or(0)),
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

    fn model(&self, id: u32) -> Option<ModelInfo> {
        synthetic::model(id)
    }

    /// From HX Edit's catalog when this machine has one (`HX_EDIT_RESOURCES`),
    /// so the UI can be checked against real colours without a pedal. The
    /// fixtures' model names are the catalog's.
    /// The fake's model numbers are its own, not the catalog's: go by name.
    fn category(&self, _model_id: Option<u32>, name: &str) -> Option<CategoryInfo> {
        static CATALOG: std::sync::OnceLock<Option<hx_catalog::Catalog>> =
            std::sync::OnceLock::new();
        let catalog = CATALOG.get_or_init(|| hx_catalog::Catalog::load().ok());
        controls::category(catalog.as_ref()?, None, name)
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
                let chain = parse::chain(&self.fixture("chain.txt").await?)?;
                find_block(&chain, block)?;
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
                let chain = parse::chain(&self.fixture("chain.txt").await?)?;
                let name = find_block(&chain, block)?
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
                let chain = parse::chain(&self.fixture("chain.txt").await?)?;
                let name = find_block(&chain, block)?
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
                let info = synthetic::model(model)
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
                let shown = synthetic::format(model, param, held);
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
