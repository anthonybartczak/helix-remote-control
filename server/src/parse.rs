//! Parsers for `tonepush` v0.9.0 stdout.
//!
//! Formats follow `crates/tonepush-cli/src/main.rs` at that tag. Re-check them
//! when the pin moves.

use serde::Serialize;

use crate::device::{DeviceError, Result};

fn parse_err(what: &str, line: &str) -> DeviceError {
    DeviceError::Parse(format!("{what}: {line:?}"))
}

/// `tonepush info`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub presets: u32,
    pub device_id: String,
    pub firmware: String,
    pub build: String,
}

/// ```text
/// HX Effects (128 presets)
/// device id: 0x00210005
/// firmware:  3.80
/// build:     d972399
/// ```
pub fn info(out: &str) -> Result<DeviceInfo> {
    let mut lines = out.lines();
    let first = lines.next().unwrap_or_default();
    let (name, rest) = first
        .rsplit_once(" (")
        .ok_or_else(|| parse_err("info header", first))?;
    let presets = rest
        .strip_suffix(" presets)")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| parse_err("info header", first))?;

    let mut field = |key: &str| -> Result<String> {
        let line = lines.next().unwrap_or_default();
        line.strip_prefix(key)
            .map(|v| v.trim().to_owned())
            .ok_or_else(|| parse_err(key, line))
    };
    Ok(DeviceInfo {
        name: name.to_owned(),
        presets,
        device_id: field("device id:")?,
        firmware: field("firmware:")?,
        build: field("build:")?,
    })
}

/// One line of `tonepush presets`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PresetEntry {
    /// Zero-based slot. Names are not unique (factory slots are all
    /// `New Preset`), so this is the key.
    pub index: u32,
    /// Front-panel label, e.g. `01C`.
    pub label: String,
    pub name: String,
    pub loaded: bool,
}

/// `{label} {name:<24} {"<- loaded" | ""}`, one line per slot, in slot order.
pub fn presets(out: &str) -> Result<Vec<PresetEntry>> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let line = line.trim_end();
            let (rest, loaded) = match line.strip_suffix("<- loaded") {
                Some(rest) => (rest, true),
                None => (line, false),
            };
            let (label, name) = rest
                .split_once(' ')
                .ok_or_else(|| parse_err("preset line", line))?;
            Ok(PresetEntry {
                index: index as u32,
                label: label.to_owned(),
                name: name.trim().to_owned(),
                loaded,
            })
        })
        .collect()
}

/// `tonepush preset` (summary form).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CurrentPreset {
    pub setlist: u32,
    pub index: u32,
    pub label: String,
    pub name: String,
    pub firmware: String,
    pub build: String,
}

/// ```text
/// preset:   01C New Preset  (setlist 0, index 2)
/// firmware: 3.80
/// build:    d972399
/// sections: 48 bytes
/// ```
pub fn preset(out: &str) -> Result<CurrentPreset> {
    let mut lines = out.lines();
    let first = lines.next().unwrap_or_default();
    let head = first
        .strip_prefix("preset:")
        .map(str::trim)
        .ok_or_else(|| parse_err("preset header", first))?;
    let (label_name, tail) = head
        .rsplit_once("  (setlist ")
        .ok_or_else(|| parse_err("preset header (no metadata)", first))?;
    let (setlist, index) = tail
        .strip_suffix(')')
        .and_then(|t| t.split_once(", index "))
        .and_then(|(s, i)| Some((s.parse().ok()?, i.parse().ok()?)))
        .ok_or_else(|| parse_err("preset header", first))?;
    let (label, name) = label_name.split_once(' ').unwrap_or((label_name, ""));

    let mut field = |key: &str| -> Result<String> {
        let line = lines.next().unwrap_or_default();
        line.strip_prefix(key)
            .map(|v| v.trim().to_owned())
            .ok_or_else(|| parse_err(key, line))
    };
    Ok(CurrentPreset {
        setlist,
        index,
        label: label.to_owned(),
        name: name.trim().to_owned(),
        firmware: field("firmware:")?,
        build: field("build:")?,
    })
}

/// `tonepush chain`: the loaded preset's blocks with parameter values.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chain {
    pub label: Option<String>,
    pub name: Option<String>,
    pub tempo: Option<f32>,
    pub snapshots: Vec<String>,
    /// Active snapshot, zero-based. Only from backends that read the preset
    /// document (`usb`, `fake`); `tonepush chain` does not print it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<u32>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block {
    /// Block number as `chain` prints it. Write commands use this numbering;
    /// `topology` and `controllers` use `position - 1`.
    pub position: u32,
    /// Model name, or `model N` without HX Edit data.
    pub model: String,
    /// Model number for `/api/models/{id}`. Only with parameter metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<u32>,
    /// Category, short label and colour as HX Edit shows them. Only with the
    /// catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<crate::controls::CategoryInfo>,
    /// The real gear the model is based on ("MXR Dyna Comp"), from the
    /// Pilot's Guide via `vendor/based-on.json` (`crate::based_on`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    pub enabled: bool,
    pub params: Vec<Param>,
    /// Second model of an Amp+Cab block. Never present on HX Effects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cab: Option<Cab>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cab {
    pub model: String,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Param {
    /// Zero-based parameter index within the model.
    pub index: u32,
    /// Name from the catalog, or `param N` without it.
    pub name: String,
    /// Display value as printed, e.g. `2.2 kHz`, or the raw number.
    pub value: String,
    /// The value as the device stores it (`0.4` for `40 %`). Only with
    /// parameter metadata; primary model only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<f32>,
}

/// Parameter lines are `"      {name:<18} {value}"`: value starts at column 25.
const PARAM_INDENT: &str = "      ";
const PARAM_VALUE_COLUMN: usize = 25;

fn param_line(index: u32, line: &str) -> Result<Param> {
    let body = &line[PARAM_INDENT.len()..];
    let (name, value) = match (
        line.get(..PARAM_VALUE_COLUMN),
        line.get(PARAM_VALUE_COLUMN..),
    ) {
        // Name fits its column (the usual case): split on the column.
        (Some(head), Some(value)) if head.ends_with(' ') => {
            (head[PARAM_INDENT.len()..].trim(), value)
        }
        // A name longer than 18 characters pushes the value right, one space
        // after it. Names and values both hold spaces ("Fc Low", "2 %"), so
        // this is a guess: the value starts at the first word that looks
        // numeric, else it is the last word.
        _ => split_long_param(body.trim_end()).ok_or_else(|| parse_err("parameter line", line))?,
    };
    Ok(Param {
        index,
        name: name.trim().to_owned(),
        value: value.trim().to_owned(),
        native: None,
    })
}

fn split_long_param(body: &str) -> Option<(&str, &str)> {
    let numeric = body.match_indices(' ').find(|&(i, _)| {
        body[i + 1..].starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
    });
    match numeric {
        Some((i, _)) => Some((&body[..i], &body[i + 1..])),
        None => body.rsplit_once(' '),
    }
}

/// ```text
/// 01C New Preset   120.0 BPM
/// snapshots: SNAPSHOT 1, SNAPSHOT 2, SNAPSHOT 3, SNAPSHOT 4
///
///  2. Fassel                   (bypassed)
///       Position           2 %
///     + Some Cab                       (Amp+Cab only)
///       Mic                57 Dynamic
/// ```
pub fn chain(out: &str) -> Result<Chain> {
    let mut lines = out.lines().peekable();
    let mut chain = Chain {
        label: None,
        name: None,
        tempo: None,
        snapshots: Vec::new(),
        snapshot: None,
        blocks: Vec::new(),
    };

    // Header: label + name and/or tempo; either half may be missing.
    if let Some(header) = lines.next() {
        let mut head = header.trim();
        if let Some(before) = head.strip_suffix(" BPM") {
            if let Some((rest, bpm)) = before.trim_end().rsplit_once(' ') {
                chain.tempo = bpm.parse().ok();
                head = rest.trim_end();
            } else if let Ok(bpm) = before.trim().parse() {
                chain.tempo = Some(bpm);
                head = "";
            }
        }
        if !head.is_empty() {
            let (label, name) = head.split_once(' ').unwrap_or((head, ""));
            chain.label = Some(label.to_owned());
            chain.name = Some(name.trim().to_owned());
        }
    }
    if let Some(list) = lines.peek().and_then(|l| l.strip_prefix("snapshots:")) {
        chain.snapshots = list.split(", ").map(|s| s.trim().to_owned()).collect();
        lines.next();
    }

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(PARAM_INDENT) {
            let block = chain
                .blocks
                .last_mut()
                .ok_or_else(|| parse_err("parameter before any block", line))?;
            let params = match &mut block.cab {
                Some(cab) => &mut cab.params,
                None => &mut block.params,
            };
            params.push(param_line(params.len() as u32, line)?);
        } else if let Some(model) = line.strip_prefix("    + ") {
            let block = chain
                .blocks
                .last_mut()
                .ok_or_else(|| parse_err("cab before any block", line))?;
            block.cab = Some(Cab {
                model: model.trim().to_owned(),
                params: Vec::new(),
            });
        } else {
            let (num, rest) = line
                .trim_start()
                .split_once(". ")
                .ok_or_else(|| parse_err("block line", line))?;
            let position = num.parse().map_err(|_| parse_err("block number", line))?;
            let rest = rest.trim_end();
            let (model, enabled) = match rest.strip_suffix("(bypassed)") {
                Some(model) => (model, false),
                None => (rest, true),
            };
            chain.blocks.push(Block {
                position,
                model: model.trim().to_owned(),
                model_id: None,
                category: None,
                based_on: None,
                enabled,
                params: Vec::new(),
                cab: None,
            });
        }
    }
    Ok(chain)
}

/// Add what the backend knows beyond the text: active snapshot, natives.
pub fn add_extra(chain: &mut Chain, extra: &crate::device::ChainExtra) {
    chain.snapshot = extra
        .snapshot
        .filter(|&i| (i as usize) < chain.snapshots.len());
    if let Some(natives) = &extra.natives {
        add_natives(chain, natives);
    }
}

/// Add model numbers and native values to a parsed chain, matching blocks by
/// position. A block whose natives are missing keeps none.
pub fn add_natives(chain: &mut Chain, natives: &crate::device::Natives) {
    for block in &mut chain.blocks {
        let Some(n) = natives.0.iter().find(|n| n.position == block.position) else {
            continue;
        };
        block.model_id = Some(n.model);
        for param in &mut block.params {
            param.native = n.values.get(param.index as usize).copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
    }

    #[test]
    fn info_fixture() {
        assert_eq!(
            info(&fixture("info.txt")).unwrap(),
            DeviceInfo {
                name: "HX Effects".into(),
                presets: 128,
                device_id: "0x00210005".into(),
                firmware: "3.80".into(),
                build: "d972399".into(),
            }
        );
    }

    #[test]
    fn presets_fixture() {
        let list = presets(&fixture("presets.txt")).unwrap();
        assert_eq!(list.len(), 128);
        assert_eq!(list[104].label, "27A");
        assert_eq!(list[104].index, 104);
        assert_eq!(list[0].name, "New Preset");
        let loaded: Vec<_> = list.iter().filter(|p| p.loaded).collect();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "01C");
        assert_eq!(loaded[0].index, 2);
    }

    #[test]
    fn preset_names_keep_inner_spaces() {
        let out = "01A My  Clean Tone             \n01B X <- loaded\n";
        let list = presets(out).unwrap();
        assert_eq!(list[0].name, "My  Clean Tone");
        assert!(list[1].loaded);
        assert_eq!(list[1].name, "X");
    }

    #[test]
    fn preset_fixture() {
        assert_eq!(
            preset(&fixture("preset.txt")).unwrap(),
            CurrentPreset {
                setlist: 0,
                index: 2,
                label: "01C".into(),
                name: "New Preset".into(),
                firmware: "3.80".into(),
                build: "d972399".into(),
            }
        );
    }

    #[test]
    fn preset_without_metadata_is_an_error() {
        let out = "preset:   (metadata unavailable: boom)\nfirmware: 3.80\nbuild:    x\n";
        assert!(preset(out).is_err());
    }

    #[test]
    fn chain_fixture() {
        let chain = chain(&fixture("chain.txt")).unwrap();
        assert_eq!(chain.label.as_deref(), Some("01C"));
        assert_eq!(chain.name.as_deref(), Some("New Preset"));
        assert_eq!(chain.tempo, Some(120.0));
        assert_eq!(chain.snapshots.len(), 4);
        assert_eq!(chain.snapshots[3], "SNAPSHOT 4");

        let summary: Vec<_> = chain
            .blocks
            .iter()
            .map(|b| (b.position, b.model.as_str(), b.enabled, b.params.len()))
            .collect();
        assert_eq!(
            summary,
            [
                (2, "Fassel", false, 5),
                (3, "Simple Pitch", false, 6),
                (5, "FX Loop 1", true, 4),
                (10, "Noise Gate", true, 3),
            ]
        );
        let fassel = &chain.blocks[0].params;
        assert_eq!(
            (fassel[0].name.as_str(), fassel[0].value.as_str()),
            ("Position", "2 %")
        );
        assert_eq!(
            (fassel[2].name.as_str(), fassel[2].value.as_str()),
            ("Fc High", "2.2 kHz")
        );
        let pitch = &chain.blocks[1].params;
        assert_eq!(
            (pitch[3].name.as_str(), pitch[3].value.as_str()),
            ("Shift Level", "10.0")
        );
        let fx_loop = &chain.blocks[2].params;
        assert_eq!((fx_loop[3].index, fx_loop[3].name.as_str()), (3, "param 3"));
    }

    #[test]
    fn chain_without_catalog() {
        let chain = chain(&fixture("no-catalog/chain.txt")).unwrap();
        assert_eq!(chain.blocks.len(), 4);
        assert_eq!(chain.blocks[0].model, "model 269");
        let p = &chain.blocks[3].params[0];
        assert_eq!(
            (p.name.as_str(), p.value.as_str()),
            ("param 0", "-22.099998")
        );
    }

    #[test]
    fn chain_long_param_name_and_cab() {
        let out = "\
02A Big Amp\n\
\n \
1. A Very Long Model Name That Overflows (bypassed)\n      \
Mix                50 %\n      \
Extremely Long Param 7.5 dB\n    \
+ Some Cab\n      \
Mic                57 Dynamic\n";
        let chain = chain(out).unwrap();
        assert_eq!(chain.tempo, None);
        assert_eq!(chain.name.as_deref(), Some("Big Amp"));
        let block = &chain.blocks[0];
        assert_eq!(block.model, "A Very Long Model Name That Overflows");
        assert!(!block.enabled);
        assert_eq!(block.params[1].name, "Extremely Long Param");
        assert_eq!(block.params[1].value, "7.5 dB");
        let cab = block.cab.as_ref().unwrap();
        assert_eq!(cab.model, "Some Cab");
        assert_eq!(cab.params[0].value, "57 Dynamic");
    }

    #[test]
    fn natives_join_by_position() {
        use crate::device::{BlockNatives, Natives};
        let mut c = chain(&fixture("chain.txt")).unwrap();
        let natives = Natives(vec![BlockNatives {
            position: 10,
            model: 123,
            values: vec![-22.1, 0.431],
        }]);
        add_natives(&mut c, &natives);
        assert_eq!(c.blocks[0].model_id, None);
        assert_eq!(c.blocks[0].params[0].native, None);
        let gate = &c.blocks[3];
        assert_eq!(gate.model_id, Some(123));
        assert_eq!(gate.params[1].native, Some(0.431));
        // Fewer values than parameters: the rest stay typed.
        assert_eq!(gate.params[2].native, None);
    }

    #[test]
    fn chain_rejects_garbage() {
        assert!(chain("01A X\n\nnot a block\n").is_err());
    }
}
