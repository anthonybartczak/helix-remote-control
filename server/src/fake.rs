//! [`FakeDevice`]: replays output recorded from the real device
//! (`server/fixtures/`) and keeps write commands in memory.
//!
//! Reads render the fixtures through the in-memory state, in the same text
//! formats `tonepush` v0.9.0 prints (`crates/tonepush-cli/src/main.rs`), so
//! the API parses fake and real output with the same code. Write replies copy
//! TonePush's own messages; they were not recorded on hardware.
//!
//! Limits: one chain was recorded, so every preset shows it; set values are
//! stored as typed, not converted to display units.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::device::{Command, Device, DeviceError, Result};
use crate::parse;

pub struct FakeDevice {
    dir: PathBuf,
    delay: Duration,
    state: Mutex<State>,
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
}

impl FakeDevice {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        FakeDevice {
            dir: dir.into(),
            delay: Duration::ZERO,
            state: Mutex::default(),
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
            Command::Chain => {
                let chain = parse::chain(&self.fixture("chain.txt").await?)?;
                let presets = parse::presets(&self.fixture("presets.txt").await?)?;
                let state = self.state();
                let p = state.loaded_entry(&presets)?;
                let chain = state.apply_chain(chain, &p.label, state.name(p));
                Ok(render_chain(&chain))
            }
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
                self.state()
                    .buffer
                    .values
                    .insert((block, param), value.clone());
                Ok(format!("block {block}: {name} = {value}\n"))
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
