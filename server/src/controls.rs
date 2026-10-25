//! Parameter metadata for sliders, menus and switches (`GET /api/models/{id}`).
//!
//! A model's parameters are described once, from HX Edit's catalog
//! (`hx_catalog::Param`: kind, min, max, default and display format), so the
//! UI can draw a control and show the value under the finger without asking
//! the server at every step. Values stay native, as the device stores them
//! (TonePush opcodes.md 4.2): `0.4` for a knob shown `40 %`, `-22.1` for dB.
//!
//! A continuous parameter becomes a grid of `steps + 1` positions, each with
//! the label HX Edit would show for it. A drag moves between grid positions;
//! exact values stay reachable by typing.
//!
//! Also the model browser for adding blocks (`GET /api/catalog`, [`browser`]).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Serialize;

use crate::types;

/// A continuous range with no more grid positions than this uses a round step
/// (0.01 on a 0..1 knob, 1 dB on -120..12 dB).
const MAX_STEPS: f32 = 150.0;
/// Positions on a logarithmic grid (frequencies, times spanning decades).
const LOG_STEPS: u32 = 200;
/// A range whose ends are at least this far apart (as a ratio) is logarithmic:
/// frequencies (Damping 500 Hz..20 kHz, Low Cut 20 Hz..1 kHz) and decay times.
/// Linear steps there were 200 Hz wide at the low end (Pi, 2026-10-14).
const LOG_RATIO: f32 = 20.0;
/// Longest menu sent; longer ones fall back to typing.
const MAX_MENU: f32 = 256.0;

/// How the UI edits a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A slider over `steps + 1` grid positions.
    Continuous,
    /// A picker; `labels[i]` is the value `min + i`.
    Menu,
    /// A toggle; `labels` is `[off, on]`, natives `min` and `max`.
    Switch,
    /// No control: type the value.
    Text,
}

impl From<hx_catalog::Kind> for Kind {
    fn from(kind: hx_catalog::Kind) -> Self {
        match kind {
            hx_catalog::Kind::Continuous => Kind::Continuous,
            hx_catalog::Kind::Enum => Kind::Menu,
            hx_catalog::Kind::Switch => Kind::Switch,
            hx_catalog::Kind::Text => Kind::Text,
        }
    }
}

/// How grid positions map to native values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scale {
    Linear,
    Log,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParamInfo {
    /// Zero-based index, as in `/api/chain`.
    pub index: u32,
    pub name: String,
    pub kind: Kind,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub scale: Scale,
    /// Grid positions minus one: `native_at(i)` for `i` in `0..=steps`.
    pub steps: u32,
    /// Display text at each grid position (`steps + 1` entries); empty for `text`.
    pub labels: Vec<String>,
}

/// A block's category the way HX Edit draws it, from HX Edit's catalog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CategoryInfo {
    /// "Distortion", "Send/Return".
    pub name: String,
    /// The label HX Edit puts under a block: "Dist", "Verb", "Snd/Rtn".
    pub short: String,
    /// HX Edit's tint for the category, `#rrggbb`.
    pub colour: String,
    /// HX Edit's shelf for this model: "Mono", "Stereo" or "Legacy".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcategory: Option<String>,
}

/// The category of the model numbered `model_id`, or else named `name`.
pub fn category(
    catalog: &hx_catalog::Catalog,
    model_id: Option<u32>,
    name: &str,
) -> Option<CategoryInfo> {
    let model = model_id
        .and_then(|id| catalog.model_number(id))
        .or_else(|| catalog.models().find(|m| m.name == name))?;
    let id = catalog.category_of(&model.id).unwrap_or(model.category);
    let c = catalog.categories().iter().find(|c| c.id == id)?;
    Some(CategoryInfo {
        name: c.name.clone(),
        short: c.short_name.clone(),
        colour: format!("#{:06x}", c.colour & 0xff_ffff),
        subcategory: c
            .subcategories
            .iter()
            .find(|s| s.models.contains(&model.id))
            .map(|s| s.name.clone()),
    })
}

/// A category in the model browser (`GET /api/catalog`), as HX Edit lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelGroup {
    /// "Distortion".
    pub name: String,
    /// "Dist".
    pub short: String,
    /// HX Edit's tint, `#rrggbb`.
    pub colour: String,
    /// Effect types in display order ("Boost", "Overdrive", …), only those
    /// some model has. Ours, not Line 6's ([`crate::types`]); empty for a
    /// category without types (Wah, Volume/Pan, Send/Return).
    pub types: Vec<String>,
    /// HX Edit's shelves ("Mono", "Stereo", "Legacy"), in its order. One
    /// unnamed shelf for a category without them.
    pub shelves: Vec<Shelf>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shelf {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelEntry {
    /// Model number, for `POST /api/blocks/{position}`.
    pub id: u32,
    pub name: String,
    /// Effect type within the category ("Fuzz"), from [`crate::types`].
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub effect_type: Option<String>,
    /// The real gear behind it (`crate::based_on`); filled in by the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
}

/// HX Edit's catalog ids with no constant in `hx_catalog`.
const LOOPER: u32 = 17;
const FAVORITES: u32 = 23;

/// Whether blocks of this category can be added. The HX Effects has no amp,
/// preamp, cab or IR blocks (Pilot's Guide 3.80, p. 57) and no looper; the
/// structural categories (input, output, split, merge) are not blocks at all,
/// and Favorites re-lists models from the others.
fn offered(c: &hx_catalog::Category) -> bool {
    use hx_catalog::Category as C;
    let not_here = [
        C::AMP,
        C::PREAMP,
        C::CAB,
        C::IR,
        C::AMP_CAB,
        LOOPER,
        FAVORITES,
    ];
    c.is_effect() && !c.paired && !not_here.contains(&c.id)
}

/// The numbers the device uses (`Helix.sym`) for each catalog model. The
/// catalog keeps one model for a mono and a stereo version, listed on both
/// shelves; the device numbers them apart (`HD2_DelaySimpleDelayMono` is 80,
/// `HD2_DelaySimpleDelayStereo` 92).
struct Numbers<'a>(HashMap<&'a str, Variants>);

/// Device numbers of one catalog model: the symbol named as the model, and
/// the ones with a `Mono` or `Stereo` suffix.
#[derive(Default)]
struct Variants {
    plain: Option<u32>,
    mono: Option<u32>,
    stereo: Option<u32>,
}

impl<'a> Numbers<'a> {
    fn new(catalog: &'a hx_catalog::Catalog) -> Self {
        let mut numbers: HashMap<&str, Variants> = HashMap::new();
        for s in catalog.symbols() {
            let Some(id) = s.model.as_deref() else {
                continue;
            };
            let v = numbers.entry(id).or_default();
            let slot = match s.symbol.strip_prefix(id) {
                Some("Mono") => &mut v.mono,
                Some("Stereo") => &mut v.stereo,
                _ => &mut v.plain,
            };
            slot.get_or_insert(s.number);
        }
        Numbers(numbers)
    }

    /// The number for model `id` on shelf `shelf`.
    fn get(&self, id: &str, shelf: Option<&str>) -> Option<u32> {
        let v = self.0.get(id)?;
        match shelf {
            Some("Mono") => v.mono.or(v.plain),
            Some("Stereo") => v.stereo.or(v.plain),
            _ => v.plain.or(v.mono).or(v.stereo),
        }
    }
}

/// Catalog ids the browser leaves out, read once from HX Edit's files
/// (`hx_catalog` reads neither flag):
/// - marked `"hidden"` in `HX_ModelCatalog.json`: kept only so old presets
///   still load (Victoria_ShufflingDelay, an older Glitch Delay);
/// - with a `devices` list in a `.models` file that lacks the HX Effects
///   (FX Loop, Send and Return 3 and 4 are Helix-only). A model without the
///   list runs on every device.
fn left_out() -> &'static HashSet<String> {
    static LEFT_OUT: OnceLock<HashSet<String>> = OnceLock::new();
    LEFT_OUT.get_or_init(|| {
        let mut ids = HashSet::new();
        let Some(dir) = hx_catalog::resources_dir() else {
            return ids;
        };
        let json = |path: &std::path::Path| -> Option<serde_json::Value> {
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
        };
        if let Some(catalog) = json(&dir.join("HX_ModelCatalog.json")) {
            collect_hidden(&catalog, &mut ids);
        }
        let files = std::fs::read_dir(&dir).into_iter().flatten().flatten();
        for path in files.map(|f| f.path()) {
            if path.extension().is_some_and(|e| e == "models") {
                if let Some(models) = json(&path) {
                    collect_elsewhere(&models, hx_proto::HX_EFFECTS.device_id, &mut ids);
                }
            }
        }
        ids
    })
}

/// Models of a `.models` file whose `devices` list lacks `device`.
fn collect_elsewhere(models: &serde_json::Value, device: u32, ids: &mut HashSet<String>) {
    let Some(models) = models.as_array() else {
        return;
    };
    for m in models {
        let (Some(id), Some(devices)) = (
            m.get("symbolicID").and_then(|v| v.as_str()),
            m.get("devices").and_then(|v| v.as_array()),
        ) else {
            continue;
        };
        let here = devices
            .iter()
            .any(|d| d.get("id").and_then(|v| v.as_u64()) == Some(u64::from(device)));
        if !here {
            ids.insert(id.to_owned());
        }
    }
}

fn collect_hidden(value: &serde_json::Value, ids: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let (Some(serde_json::Value::String(id)), Some(serde_json::Value::Bool(true))) =
                (map.get("id"), map.get("hidden"))
            {
                ids.insert(id.clone());
            }
            map.values().for_each(|v| collect_hidden(v, ids));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_hidden(v, ids)),
        _ => {}
    }
}

/// Every model a block can be added with, by category and shelf, in HX
/// Edit's order, each with its effect type ([`crate::types`]). Left out:
/// models the device has no number for, the ones HX Edit hides, and the ones
/// another device has but the HX Effects not ([`left_out`]).
pub fn browser(catalog: &hx_catalog::Catalog) -> Vec<ModelGroup> {
    let numbers = Numbers::new(catalog);
    let left_out = left_out();
    catalog
        .categories()
        .iter()
        .filter(|c| offered(c))
        .filter_map(|c| {
            let entries = |ids: &[String], shelf: Option<&str>| -> Vec<ModelEntry> {
                ids.iter()
                    .filter(|id| !left_out.contains(*id))
                    .filter_map(|id| {
                        let name = catalog.model(id)?.name.clone();
                        Some(ModelEntry {
                            id: numbers.get(id, shelf)?,
                            effect_type: types::of(&c.name, &name).map(str::to_owned),
                            name,
                            based_on: None,
                        })
                    })
                    .collect()
            };
            let shelves: Vec<Shelf> = if c.subcategories.is_empty() {
                vec![Shelf {
                    name: None,
                    models: entries(&c.models, None),
                }]
            } else {
                c.subcategories
                    .iter()
                    .map(|s| Shelf {
                        name: Some(s.name.clone()),
                        models: entries(&s.models, Some(&s.name)),
                    })
                    .collect()
            };
            let shelves: Vec<Shelf> = shelves
                .into_iter()
                .filter(|s| !s.models.is_empty())
                .collect();
            (!shelves.is_empty()).then(|| ModelGroup {
                name: c.name.clone(),
                short: c.short_name.clone(),
                colour: format!("#{:06x}", c.colour & 0xff_ffff),
                types: used_types(&c.name, &shelves),
                shelves,
            })
        })
        .collect()
}

/// A category's effect types in display order, only those some model has.
pub fn used_types(category: &str, shelves: &[Shelf]) -> Vec<String> {
    types::order(category)
        .into_iter()
        .filter(|t| {
            shelves
                .iter()
                .flat_map(|s| &s.models)
                .any(|m| m.effect_type.as_deref() == Some(*t))
        })
        .map(str::to_owned)
        .collect()
}

/// Whether model number `number` is one [`browser`] offers.
pub fn addable(catalog: &hx_catalog::Catalog, number: u32) -> bool {
    let Some(model) = catalog.model_number(number) else {
        return false;
    };
    !left_out().contains(&model.id)
        && catalog
            .categories()
            .iter()
            .any(|c| offered(c) && c.models.contains(&model.id))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelInfo {
    /// Model number, as `model_id` in `/api/chain`.
    pub id: u32,
    pub name: String,
    /// True for the fake backend's made-up ranges (not from HX Edit).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
    pub params: Vec<ParamInfo>,
}

/// The grid for a parameter: its scale and number of steps. `None` if the
/// range cannot carry a control (the UI falls back to typing).
pub fn grid(kind: Kind, min: f32, max: f32) -> Option<(Scale, u32)> {
    if !(min.is_finite() && max.is_finite() && max > min) {
        return None;
    }
    let span = max - min;
    match kind {
        Kind::Text => None,
        Kind::Switch => Some((Scale::Linear, 1)),
        Kind::Menu => (span <= MAX_MENU).then(|| (Scale::Linear, span.round().max(1.0) as u32)),
        Kind::Continuous if min > 0.0 && max / min >= LOG_RATIO => Some((Scale::Log, LOG_STEPS)),
        Kind::Continuous => {
            let step = nice_step(span);
            Some((Scale::Linear, (span / step).round().max(1.0) as u32))
        }
    }
}

/// The smallest 1, 2 or 5 times a power of ten that splits `span` into at
/// most [`MAX_STEPS`] steps.
fn nice_step(span: f32) -> f32 {
    let mut magnitude = 10f32.powi((span / MAX_STEPS).log10().floor() as i32);
    loop {
        for factor in [1.0, 2.0, 5.0] {
            let step = factor * magnitude;
            if span / step <= MAX_STEPS + 1e-3 {
                return step;
            }
        }
        magnitude *= 10.0;
    }
}

/// Native value at grid position `i`. The web UI has the same formula
/// (`nativeAt` in `web/app.js`).
pub fn native_at(scale: Scale, steps: u32, min: f32, max: f32, i: u32) -> f32 {
    if i >= steps {
        return max;
    }
    let t = i as f32 / steps as f32;
    match scale {
        Scale::Linear => min + t * (max - min),
        Scale::Log => min * (max / min).powf(t),
    }
}

/// Describe one parameter, labelling every grid position with `format`.
#[allow(clippy::too_many_arguments)]
pub fn describe(
    index: u32,
    name: &str,
    kind: Kind,
    min: f32,
    max: f32,
    default: f32,
    format: &dyn Fn(f32) -> String,
) -> ParamInfo {
    let (kind, scale, steps) = match grid(kind, min, max) {
        Some((scale, steps)) => (kind, scale, steps),
        None => (Kind::Text, Scale::Linear, 0),
    };
    let mut labels: Vec<String> = match kind {
        Kind::Text => Vec::new(),
        _ => (0..=steps)
            .map(|i| format(native_at(scale, steps, min, max, i)))
            .collect(),
    };
    let (mut min, mut max, mut steps) = (min, max, steps);
    if kind == Kind::Menu {
        // HX Edit's range can run past its list of names: 70s Chorus Note
        // Sync is 1..=19 with names for 1..=18, so 19 printed as a bare
        // number. Leave unnamed values off the ends of a menu of names.
        let unnamed = |l: &String| l.trim().parse::<f32>().is_ok();
        if labels.iter().any(|l| !unnamed(l)) {
            while labels.last().is_some_and(unnamed) {
                labels.pop();
                max -= 1.0;
            }
            while labels.first().is_some_and(unnamed) {
                labels.remove(0);
                min += 1.0;
            }
            steps = labels.len() as u32 - 1;
        }
    }
    ParamInfo {
        index,
        name: name.to_owned(),
        kind,
        min,
        max,
        default,
        scale,
        steps,
        labels,
    }
}

/// A model from HX Edit's catalog, parameters in the order the device
/// indexes them (`Helix.sym`, as `Catalog::param`).
pub fn from_catalog(catalog: &hx_catalog::Catalog, id: u32) -> Option<ModelInfo> {
    let model = catalog.model_number(id)?;
    let params = (0..)
        .map_while(|i| catalog.param(id, i))
        .enumerate()
        .map(|(i, p)| {
            describe(
                i as u32,
                &p.name,
                p.kind.into(),
                p.min,
                p.max,
                p.default,
                &|v| catalog.format(p, v),
            )
        })
        .collect();
    Some(ModelInfo {
        id,
        name: model.name.clone(),
        synthetic: false,
        params,
    })
}

/// The value the device holds after a write of `native`: menus take whole
/// numbers, switches 0 or 1 (`hx_catalog::Kind::wire`).
pub fn settle(kind: hx_catalog::Kind, native: f32) -> f32 {
    match kind {
        hx_catalog::Kind::Enum => native.round(),
        hx_catalog::Kind::Switch => {
            if native >= 0.5 {
                1.0
            } else {
                0.0
            }
        }
        hx_catalog::Kind::Continuous | hx_catalog::Kind::Text => native,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids() {
        // 0..1 knobs and percentages: 0.01 steps.
        assert_eq!(grid(Kind::Continuous, 0.0, 1.0), Some((Scale::Linear, 100)));
        // A dB level: 1 dB steps.
        assert_eq!(
            grid(Kind::Continuous, -120.0, 12.0),
            Some((Scale::Linear, 132))
        );
        assert_eq!(
            grid(Kind::Continuous, -1.0, 1.0),
            Some((Scale::Linear, 100))
        );
        assert_eq!(
            grid(Kind::Continuous, -24.0, 24.0),
            Some((Scale::Linear, 96))
        );
        // Frequencies and decay times span decades.
        for (min, max) in [
            (20.0, 20000.0),
            (500.0, 20000.0),
            (19.9, 1000.0),
            (0.1, 3.0),
        ] {
            assert_eq!(
                grid(Kind::Continuous, min, max),
                Some((Scale::Log, 200)),
                "{min}..{max}"
            );
        }
        // A predelay from 0 cannot be logarithmic.
        assert_eq!(grid(Kind::Continuous, 0.0, 0.1), Some((Scale::Linear, 100)));
        assert_eq!(grid(Kind::Menu, 0.0, 11.0), Some((Scale::Linear, 11)));
        assert_eq!(grid(Kind::Switch, 0.0, 1.0), Some((Scale::Linear, 1)));
        for (kind, min, max) in [
            (Kind::Continuous, 1.0, 1.0),
            (Kind::Continuous, 1.0, 0.0),
            (Kind::Continuous, f32::NAN, 1.0),
            (Kind::Menu, 0.0, 1000.0),
            (Kind::Text, 0.0, 1.0),
        ] {
            assert_eq!(grid(kind, min, max), None, "{kind:?} {min} {max}");
        }
    }

    #[test]
    fn grid_ends_are_exact() {
        for (scale, min, max) in [
            (Scale::Linear, -120.0, 12.0),
            (Scale::Log, 20.0, 20000.0),
            (Scale::Linear, 0.0, 1.0),
        ] {
            assert_eq!(native_at(scale, 100, min, max, 0), min);
            assert_eq!(native_at(scale, 100, min, max, 100), max);
        }
        assert!((native_at(Scale::Linear, 100, 0.0, 1.0, 40) - 0.4).abs() < 1e-6);
        // Log midpoint is the geometric mean.
        assert!((native_at(Scale::Log, 2, 20.0, 20000.0, 1) - 632.456).abs() < 0.01);
    }

    #[test]
    fn describes_each_kind() {
        let pct = |v: f32| format!("{:.0} %", v * 100.0);
        let mix = describe(3, "Mix", Kind::Continuous, 0.0, 1.0, 0.5, &pct);
        assert_eq!(mix.steps, 100);
        assert_eq!(mix.labels.len(), 101);
        assert_eq!(mix.labels[0], "0 %");
        assert_eq!(mix.labels[40], "40 %");
        assert_eq!(mix.labels[100], "100 %");

        // Menu labels are the values min..=max, offsets included.
        let interval = describe(0, "Interval", Kind::Menu, -2.0, 2.0, 0.0, &|v| {
            format!("{v:+}")
        });
        assert_eq!(interval.labels, ["-2", "-1", "+0", "+1", "+2"]);

        // Unnamed values at the ends of a named menu are left off (Note
        // Sync: 1..=19, names for 1..=18); all-number menus stay whole.
        let names = ["", "1/2 Dotted", "1/2", "1/64 Triplet"];
        let sync = describe(0, "Note Sync", Kind::Menu, 0.0, 4.0, 1.0, &|v| {
            names
                .get(v as usize)
                .filter(|n| !n.is_empty())
                .map_or_else(|| format!("{v}"), |n| n.to_string())
        });
        assert_eq!(sync.labels, ["1/2 Dotted", "1/2", "1/64 Triplet"]);
        assert_eq!((sync.min, sync.max, sync.steps), (1.0, 3.0, 2));
        assert_eq!(interval.labels.len(), 5);

        let toggle = describe(1, "Type", Kind::Switch, 0.0, 1.0, 0.0, &|v| {
            if v >= 0.5 { "Limit" } else { "Compress" }.to_owned()
        });
        assert_eq!(toggle.labels, ["Compress", "Limit"]);

        // A range that cannot carry a control is typed instead.
        let broken = describe(2, "X", Kind::Continuous, 1.0, 1.0, 1.0, &pct);
        assert_eq!((broken.kind, broken.labels.len()), (Kind::Text, 0));
    }

    /// Against HX Edit's catalog, when this machine has one (dev machine,
    /// `HX_EDIT_RESOURCES`); skipped elsewhere.
    #[test]
    fn browser_offers_effects_only() {
        let Ok(catalog) = hx_catalog::Catalog::load() else {
            return;
        };
        let groups = browser(&catalog);
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        // HX Edit 3.80's effect categories, in its order (Pilot's Guide p. 57).
        assert_eq!(
            names,
            [
                "Distortion",
                "Dynamics",
                "EQ",
                "Modulation",
                "Delay",
                "Reverb",
                "Pitch/Synth",
                "Filter",
                "Wah",
                "Volume/Pan",
                "Send/Return"
            ]
        );
        let shelves: Vec<_> = groups[0]
            .shelves
            .iter()
            .map(|s| s.name.as_deref())
            .collect();
        assert_eq!(shelves, [Some("Mono"), Some("Stereo"), Some("Legacy")]);
        let all: Vec<&ModelEntry> = groups
            .iter()
            .flat_map(|g| &g.shelves)
            .flat_map(|s| &s.models)
            .collect();
        assert!(all.iter().all(|m| addable(&catalog, m.id)));
        let symbol = |id: u32| catalog.symbol(id).unwrap().symbol.clone();
        // Mono and stereo are one catalog model but two device numbers.
        let delay = &groups[4];
        let simple = |shelf: usize| {
            delay.shelves[shelf]
                .models
                .iter()
                .find(|m| m.name == "Simple Delay")
                .unwrap()
                .id
        };
        assert_eq!(symbol(simple(0)), "HD2_DelaySimpleDelayMono");
        assert_eq!(symbol(simple(1)), "HD2_DelaySimpleDelayStereo");
        // HX Edit's hidden models (an older Glitch Delay) are left out: no
        // name twice on a shelf.
        for shelf in groups.iter().flat_map(|g| &g.shelves) {
            let mut names: Vec<&str> = shelf.models.iter().map(|m| m.name.as_str()).collect();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "{:?}", shelf.name);
        }
        let numbers = Numbers::new(&catalog);
        let old_glitch = numbers.get("Victoria_ShufflingDelay", Some("Mono"));
        assert!(old_glitch.is_some_and(|n| !addable(&catalog, n)));
        // An amp is a real model, but not one this device can add.
        let amp = catalog
            .models_in(hx_catalog::Category::AMP)
            .first()
            .and_then(|m| numbers.get(&m.id, None));
        assert!(amp.is_some_and(|n| !addable(&catalog, n)));

        // Helix-only loops are left out; the HX Effects' own two stay.
        let names: Vec<&str> = all.iter().map(|m| m.name.as_str()).collect();
        for gone in [
            "FX Loop 3",
            "FX Loop 4",
            "FX Loop 3/4",
            "Send 3",
            "Return 4",
        ] {
            assert!(!names.contains(&gone), "{gone}");
        }
        for kept in [
            "FX Loop 1",
            "FX Loop 2",
            "FX Loop 1/2",
            "Send 1",
            "Return 2",
        ] {
            assert!(names.contains(&kept), "{kept}");
        }

        // Every model in a category with types has one from the table: a
        // model HX Edit adds later shows up here as "Other".
        for g in &groups {
            for m in g.shelves.iter().flat_map(|s| &s.models) {
                assert_ne!(
                    m.effect_type.as_deref(),
                    Some(types::OTHER),
                    "{}: {} has no type",
                    g.name,
                    m.name
                );
            }
        }
        let distortion = &groups[0];
        assert_eq!(
            distortion.types,
            ["Boost", "Overdrive", "Distortion", "Fuzz", "Bass", "Lo-fi"]
        );
        let kinky = distortion.shelves[0]
            .models
            .iter()
            .find(|m| m.name == "Kinky Boost")
            .unwrap();
        assert_eq!(kinky.effect_type.as_deref(), Some("Boost"));
        assert!(groups[8].types.is_empty(), "Wah has no types");
    }

    #[test]
    fn device_lists_leave_other_devices_models_out() {
        let models = serde_json::json!([
            { "symbolicID": "Everywhere", "name": "FX Loop 1" },
            { "symbolicID": "HelixOnly", "devices": [{ "id": 2162689 }, { "id": 2162690 }] },
            { "symbolicID": "AlsoHere", "devices": [{ "id": 2162689 }, { "id": 2162693 }] }
        ]);
        let mut ids = HashSet::new();
        collect_elsewhere(&models, hx_proto::HX_EFFECTS.device_id, &mut ids);
        assert_eq!(ids, HashSet::from(["HelixOnly".to_owned()]));
    }

    #[test]
    fn hidden_flags_anywhere_in_the_tree() {
        let json = serde_json::json!({
            "categories": [{ "models": [
                { "id": "Old", "name": "Glitch Delay", "hidden": true },
                { "id": "New", "name": "Glitch Delay", "hidden": false },
                { "id": "Ref", "use_subcategory": 4 }
            ]}]
        });
        let mut ids = HashSet::new();
        collect_hidden(&json, &mut ids);
        assert_eq!(ids, HashSet::from(["Old".to_owned()]));
    }

    #[test]
    fn settles_like_the_wire() {
        use hx_catalog::Kind as K;
        assert_eq!(settle(K::Enum, 2.6), 3.0);
        assert_eq!(settle(K::Switch, 0.7), 1.0);
        assert_eq!(settle(K::Switch, 0.2), 0.0);
        assert_eq!(settle(K::Continuous, 0.123), 0.123);
        // What settle keeps is what the wire carries (#7: menus go as Int).
        assert_eq!(K::Enum.wire(2.6), hx_proto::msgpack::Value::Int(3));
        assert_eq!(K::Switch.wire(0.7), hx_proto::msgpack::Value::Bool(true));
    }
}
