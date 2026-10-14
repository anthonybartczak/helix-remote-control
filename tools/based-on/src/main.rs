//! Builds `vendor/based-on.json`: for each effect model, the real gear it is
//! based on, from the "Model Lists" tables in HX Edit's Pilot's Guide.
//!
//! The guide is Line 6's, and so is the table, so the output stays in
//! `/vendor` with the rest of HX Edit's data and is never committed
//! (AGENTS.md 5.4). The Pi gets it the way it gets `vendor/res`
//! (docs/deploy.md step 6).
//!
//! Run on the dev machine, from the repository root:
//!
//! ```text
//! cargo run --release --manifest-path tools/based-on/Cargo.toml
//! ```
//!
//! Reads the guide and the model catalog from `HX_EDIT_RESOURCES` (or wherever
//! TonePush finds HX Edit) and writes `vendor/based-on.json`, keyed by the
//! catalog's model names. Arguments override: `[guide.pdf] [out.json]`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const GUIDE: &str = "HX Edit Pilot's Guide.pdf";
/// The Subcategories column's values, longest first so "Mono, Stereo" wins
/// over "Mono".
const SHELVES: [&str; 5] = ["Mono, Stereo", "Mono/Stereo", "Stereo", "Legacy", "Mono"];
/// Categories with no models on an HX Effects, nor rows with a Subcategories
/// column in the guide: amps, preamps, cabs, IRs.
const NOT_EFFECTS: [u32; 4] = [11, 12, 13, 14];

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let catalog = hx_catalog::Catalog::load().map_err(|e| format!("HX Edit catalog: {e}"))?;
    let mut args = std::env::args().skip(1);
    let guide = args
        .next()
        .map(PathBuf::from)
        .or_else(|| hx_catalog::resources_dir().map(|r| r.join(GUIDE)))
        .ok_or("no HX Edit resources; set HX_EDIT_RESOURCES or pass the guide's path")?;
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/based-on.json"));

    let bytes = std::fs::read(&guide).map_err(|e| format!("{}: {e}", guide.display()))?;
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|e| format!("{}: {e}", guide.display()))?;

    let effects: BTreeSet<String> = catalog
        .categories()
        .iter()
        .filter(|c| c.is_effect() && !c.paired && !NOT_EFFECTS.contains(&c.id))
        .flat_map(|c| c.models.iter())
        .filter_map(|id| catalog.model(id))
        .map(|m| m.name.clone())
        .collect();
    let rows = rows(&text);
    let table = match_names(&rows, &effects);

    let missing: Vec<&str> = effects
        .iter()
        .filter(|n| !table.contains_key(*n))
        .map(String::as_str)
        .collect();
    eprintln!(
        "{} of {} effect models found in {}",
        table.len(),
        effects.len(),
        guide.display()
    );
    if !missing.is_empty() {
        eprintln!("  not in the guide's tables: {}", missing.join(", "));
    }

    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&table).map_err(|e| e.to_string())?;
    std::fs::write(&out, json + "\n").map_err(|e| format!("{}: {e}", out.display()))?;
    eprintln!("wrote {}", out.display());
    Ok(())
}

/// The guide's table rows as (model name as printed, "Based On").
///
/// A row comes out as one line, `Red Squeeze Mono, Stereo MXR Dyna Comp`: the
/// Subcategories value is what marks it. A registered-trademark sign starts a
/// new line (`Xotic` / `® EP Booster`), long cells wrap onto the next one, and
/// a column's header can follow the column before it without a blank line.
/// Prose that happens to contain "Mono" also comes out as a row; matching
/// against the catalog drops it.
fn rows(text: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut row: Option<(String, String)> = None;
    let mut finish = |row: &mut Option<(String, String)>| {
        if let Some((name, based_on)) = row.take() {
            let based_on = clean(&based_on);
            if !based_on.is_empty() {
                rows.push((name, based_on));
            }
        }
    };
    for line in &footnoted(text) {
        let line = line.as_str();
        if line.is_empty() || is_header(line) {
            finish(&mut row);
        } else if let Some(split) = split_row(line) {
            finish(&mut row);
            row = Some(split);
        } else if let Some((_, based_on)) = row.as_mut() {
            if !line.starts_with(['®', '™']) {
                based_on.push(' ');
            }
            based_on.push_str(line);
        }
    }
    finish(&mut row);
    rows
}

/// Trimmed lines, with a footnote mark's line put back on the name it marks:
/// `Poly Pitch` / `† Mono Line 6 Original`. A footnote itself follows a blank
/// line and stays on its own.
fn footnoted(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        match lines.last_mut() {
            Some(last) if !last.is_empty() && line.starts_with(['†', '‡']) => {
                last.push(' ');
                last.push_str(line);
            }
            _ => lines.push(line.to_owned()),
        }
    }
    lines
}

fn is_header(line: &str) -> bool {
    line.starts_with("Model ") || line.ends_with(" Models") || line.starts_with(['*', '†', '‡'])
}

/// `Name Shelf Based-on` -> (name, based-on), at the first Subcategories value
/// that has a name before it.
fn split_row(line: &str) -> Option<(String, String)> {
    let (at, shelf) = SHELVES
        .iter()
        .filter_map(|s| line.find(&format!(" {s} ")).map(|at| (at, *s)))
        .min_by_key(|(at, s)| (*at, usize::MAX - s.len()))?;
    let name = line[..at].trim_end_matches(['†', '‡', '*', ' ']);
    let rest = &line[at + shelf.len() + 2..];
    (!name.is_empty()).then(|| (clean(name), rest.to_owned()))
}

/// Guide names onto catalog names. Exact (after `key`) first; then, for what
/// is left, the one guide name within two letters, if only one is: the guide
/// and the catalog spell a few models differently ("Alpaca Rogue" / "Alpaca
/// Rouge", "Autofilter" / "Auto Filter").
fn match_names(rows: &[(String, String)], names: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut by_key: BTreeMap<String, &str> = BTreeMap::new();
    for (name, based_on) in rows {
        by_key.entry(key(name)).or_insert(based_on);
    }
    let mut table = BTreeMap::new();
    let mut used = BTreeSet::new();
    for name in names {
        if let Some(based_on) = by_key.get(&key(name)) {
            table.insert(name.clone(), (*based_on).to_owned());
            used.insert(key(name));
        }
    }
    let left: Vec<&String> = names.iter().filter(|n| !table.contains_key(*n)).collect();
    for name in left {
        let k = key(name);
        if k.len() < 6 {
            continue;
        }
        let close: Vec<_> = by_key
            .iter()
            .filter(|(g, _)| !used.contains(*g) && distance(g, &k) <= 2)
            .collect();
        if let [(_, based_on)] = close[..] {
            table.insert(name.clone(), (*based_on).to_owned());
        }
    }
    table
}

/// Lower case, letters and digits only: "12-String" and "12 String" agree.
fn key(name: &str) -> String {
    clean(name)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Straight quotes, no trademark signs, single spaces.
fn clean(s: &str) -> String {
    s.replace(['‘', '’'], "'")
        .replace(['“', '”'], "\"")
        .replace(['®', '™'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_split_by_trademarks_wraps_and_headers_parse() {
        let text = "Model Subcategories Based On*\n\n\
                    Kinky Boost Mono, Stereo Xotic\n® EP Booster\n\n\
                    Red Squeeze Mono, Stereo MXR Dyna Comp\n Model Subcategories Based On*\n\n\
                    Feedbacker † Mono Line 6 Original, Feedback Generator\n\n\
                    Poly Pitch \n† Mono Line 6 Original\n\n\
                    † These models are DSP-heavy, Mono or not.\n\n\
                    Vital Dist Mono, Stereo Earthquaker Devices Life - Octave/\nDistortion circuit\n\n\
                    Stereo Width Mono, Stereo Line 6 Original\n\n\
                    ‘63 Spring Legacy Line 6 Original\n";
        let rows: BTreeMap<_, _> = rows(text).into_iter().collect();
        assert_eq!(rows["Kinky Boost"], "Xotic EP Booster");
        assert_eq!(rows["Red Squeeze"], "MXR Dyna Comp");
        assert_eq!(rows["Feedbacker"], "Line 6 Original, Feedback Generator");
        assert_eq!(rows["Poly Pitch"], "Line 6 Original");
        assert_eq!(rows.len(), 7, "a footnote is not a row");
        assert_eq!(
            rows["Vital Dist"],
            "Earthquaker Devices Life - Octave/ Distortion circuit"
        );
        assert_eq!(rows["Stereo Width"], "Line 6 Original");
        assert_eq!(rows["'63 Spring"], "Line 6 Original");
    }

    #[test]
    fn catalog_spellings_match_close_guide_spellings_only() {
        let rows = [
            ("Alpaca Rogue", "A"),
            ("Autofilter", "B"),
            ("12 String", "C"),
            ("Tube Comp", "D"),
            ("Blue Comp", "E"),
            ("effects sub-categories:", "prose"),
        ]
        .map(|(n, b)| (n.to_owned(), b.to_owned()));
        let names: BTreeSet<String> = [
            "Alpaca Rouge",
            "Auto Filter",
            "12-String",
            "Tube Comp",
            "Blue Comp",
            "Red Comp",
        ]
        .map(String::from)
        .into();
        let table = match_names(&rows, &names);
        assert_eq!(table["Alpaca Rouge"], "A");
        assert_eq!(table["Auto Filter"], "B");
        assert_eq!(table["12-String"], "C");
        assert_eq!(table["Tube Comp"], "D");
        assert_eq!(table["Blue Comp"], "E");
        assert!(!table.contains_key("Red Comp"), "too short to guess");
        assert_eq!(table.len(), 5);
    }
}
