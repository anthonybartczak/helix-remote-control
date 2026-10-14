//! The real gear each model is based on ("Based On" in HX Edit's Pilot's
//! Guide), from `vendor/based-on.json`.
//!
//! The file holds Line 6's table, so it lives in `/vendor` beside HX Edit's
//! data and is never committed (AGENTS.md 5.4). `tools/based-on` builds it on
//! the dev machine; docs/deploy.md step 6 copies it to the Pi. Without it the
//! UI simply shows no "Based on" line.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Model name (as the catalog spells it) -> the gear it is based on.
pub type BasedOn = BTreeMap<String, String>;

const FILE: &str = "based-on.json";

/// Where to look: `HELIX_BASED_ON`, else next to HX Edit's `res` folder
/// (`/opt/helix-bridge/vendor/based-on.json` on the Pi), else `vendor/` in
/// the working directory (a dev checkout).
fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("HELIX_BASED_ON") {
        return p.into();
    }
    std::env::var_os("HX_EDIT_RESOURCES")
        .map(PathBuf::from)
        .and_then(|res| res.parent().map(|vendor| vendor.join(FILE)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("vendor").join(FILE))
}

/// The table, or an empty one if there is none or it does not read.
pub fn load() -> BasedOn {
    let path = path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        tracing::info!(path = %path.display(), "no based-on table; blocks show no real gear");
        return BasedOn::new();
    };
    match serde_json::from_str::<BasedOn>(&text) {
        Ok(table) => {
            tracing::info!(path = %path.display(), models = table.len(), "based-on table loaded");
            table
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "based-on table unreadable: {e}");
            BasedOn::new()
        }
    }
}
