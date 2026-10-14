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

use serde::Serialize;

/// A continuous range with no more grid positions than this uses a round step
/// (0.01 on a 0..1 knob, 1 dB on -120..12 dB).
const MAX_STEPS: f32 = 150.0;
/// Positions on a logarithmic grid (frequencies, times spanning decades).
const LOG_STEPS: u32 = 200;
/// A range whose ends are at least this far apart (as a ratio) is logarithmic.
const LOG_RATIO: f32 = 100.0;
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
    let labels = match kind {
        Kind::Text => Vec::new(),
        _ => (0..=steps)
            .map(|i| format(native_at(scale, steps, min, max, i)))
            .collect(),
    };
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
        // Frequencies span decades.
        assert_eq!(
            grid(Kind::Continuous, 20.0, 20000.0),
            Some((Scale::Log, 200))
        );
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

        let toggle = describe(1, "Type", Kind::Switch, 0.0, 1.0, 0.0, &|v| {
            if v >= 0.5 { "Limit" } else { "Compress" }.to_owned()
        });
        assert_eq!(toggle.labels, ["Compress", "Limit"]);

        // A range that cannot carry a control is typed instead.
        let broken = describe(2, "X", Kind::Continuous, 1.0, 1.0, 1.0, &pct);
        assert_eq!((broken.kind, broken.labels.len()), (Kind::Text, 0));
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
