# TonePush feature gap

What TonePush can do on an HX device that Helix Bridge cannot yet. Written 2026-10-14
against TonePush `main` at `76f3bc9` (newest tag `v0.9.1`; the bridge stays pinned to
`v0.9.0`, which has the same HX feature set: 0.9.1 only changes the Windows installer, and `main`
adds only the events-channel ACK fix already in `third_party/hx-usb`).

Sources: TonePush `README.md`, `docs/_guide/*.md`, `packaging/release-notes/*.md`, the CLI
(`crates/tonepush-cli/src/main.rs`), the GUI (`crates/tonepush-gui/src/`), `PROTOCOL.md`
and `docs/_reference/opcodes.md`. Bridge side: `docs/api.md`, `web/app.js`,
`server/src/usb.rs`.

## Keys

- **TonePush**: `CLI` = a `tonepush` subcommand, `GUI` = the desktop editor.
- **Bridge**: have / partial / missing.
- **HX Effects?** TonePush is hardware-tested on an HX Stomp 3.80 only.
  **confirmed** = shown on our HX Effects 3.80 (phase 6, fixtures) or in upstream #9;
  **unverified** = confirmed on the HX Stomp, never tried on an HX Effects;
  **no** = the HX Effects lacks it.
- **Doc write** = the operation reads the whole preset document, edits it and writes it
  back with opcode 21. It works (byte-exact round trip, `PROTOCOL.md` "Writing a preset
  document back"), but any bug costs the whole edit buffer, not one value. Untouched
  HX Effects factory presets use the older 10-word section table (AGENTS.md 2).
- **Effort** assumes the `usb` backend, `FakeDevice` support and UI, in the cloud;
  local hardware checks come on top.

## Gap table

| Feature | TonePush | Bridge | HX Effects? | R/W | Risk | Effort |
|---|---|---|---|---|---|---|
| Device info, preset list, load preset | CLI, GUI | have | confirmed (phase 6) | R + W | low | – |
| Chain with named models and formatted values | CLI `chain`, GUI | have | confirmed | R | low | – |
| Bypass block (op41) | CLI `enable`, GUI | have | confirmed | W | low | – |
| Set parameter, typed display value (op30) | CLI `set`, GUI type-in | have | confirmed | W | low | – |
| Save, rename preset (op71, op6) | CLI, GUI | have | confirmed | W | low (flash) | – |
| Live sync of front-panel changes | CLI `watch`, GUI | have (patched hx-usb) | confirmed | R | low | – |
| **Parameter metadata** (kind, min, max, default, menu choices) | GUI knobs, menus, switches (`pane.rs`) | missing | catalog data, device-independent | R | none (catalog only) | 2 h |
| **Set parameter by native value** (slider, op30 via `Kind::wire`) | GUI dial drag | missing | op30 confirmed; menus need Int (#7, fixed 0.7.0) | W | low; device wedges if hammered | 3 h with rate limit |
| **Snapshot switch** (op88 `{92: n}`) | CLI `snapshot`, GUI snapshot bar | missing | unverified | W (edit buffer) | low: one opcode, out-of-range answers -46 | 2 h |
| **Active snapshot shown** (doc `10.6`, events 42/46) | GUI | partial: names only, none lit | unverified (events 42/46 not yet recorded, `hx-effects-notifications.md`) | R | none | 1 h |
| Snapshot rename | CLI `snapshot-name`, GUI | missing | unverified | W | doc write | 2 h |
| Copy snapshot over another | CLI `copy-snapshot`, GUI matrix | missing | unverified | W | doc write | 3 h |
| Snapshot matrix (what each snapshot turns on) | GUI (`snapshot_details`) | missing | unverified (4 snapshots in fixture) | R | none | 3 h |
| **Tempo set** (doc `5.16`, 40–240 BPM) | CLI `tempo`, GUI | partial: shown, read-only | unverified | W | doc write | 2 h |
| **Tap tempo** (client side: last 5 taps, reset after 2 s) | GUI (`shell.rs` `tap`) | missing | n/a (sends a tempo write) | W | doc write per tap burst; send once taps settle | 1 h after tempo set |
| Tempo under external MIDI clock (op99) | GUI shows `[External]` | missing | unverified | R | low | 1 h |
| **Model categories, colours, short names, artwork** | GUI board tiles (`Catalog::category`, `artwork`) | partial: category guessed from model name (`app.js` `CATEGORY_RULES`) | catalog data | R | none; artwork is Line 6 data, serve from `/vendor` only | 3 h |
| Topology: parallel paths, split/join, lanes | CLI `topology`, GUI board | missing (fixture recorded) | confirmed read (`fixtures/topology.txt`) | R | none | 1 evening |
| Change a block's model (op78 + op40) | CLI `model`, GUI browser with audition | missing | unverified | W | medium: rebuilds the slot; amp+cab pair inferred | 1 evening |
| Model browser (catalog by category) | CLI `models`, GUI | missing | catalog data | R | none | 1 evening, with model change |
| Clear block (op78 then op28) | CLI `clear`, GUI | missing | unverified | W | low, but silent no-op without op78 | 1 h |
| Reorder blocks | CLI `move` (marked untested), GUI drag | missing | unverified | W | doc write; CLI says untested | 3 h |
| Copy/paste block | CLI `copy-block`, GUI | missing | unverified | W | doc write | 3 h |
| Undo / redo | GUI (client-side stack, op21) | missing | unverified | W | doc write per undo | 1 evening |
| Route input/output (op42) | CLI `route`, GUI | missing | unverified | W | low | 2 h |
| Controller assignments, read (doc key 4) | CLI `controllers`, `assignments`, GUI floor | missing (fixture recorded) | read confirmed (`fixtures/controllers.txt`); source ordinals 3–10 unconfirmed (#9) | R | none | 3 h |
| Controller assign, CC, min/max (op37, 64, 65, 66) | CLI `assign-*`, GUI | missing | unverified; ordinals unconfirmed (#9) | W | medium: wrong ordinal = wrong source | 1 evening |
| Footswitch read: label, colour, type (op33) | CLI `switch`, GUI floor | missing | unverified (6 switches) | R | low | 2 h |
| Footswitch write: bypass, label, LED, momentary (op56–62) | CLI `switch-assign`, `switch-set`, GUI | missing | unverified | W | medium | 1 evening |
| Preset backup to files (op4, read each slot in place) | CLI `backup`, `backup-all`, `back-up`, GUI backups page | missing | confirmed: 119 presets read (#9, 0.7.0); settings read can time out (#9) | R | low; skip the settings part | 3 h |
| Preset restore / paste / import into a slot (op5, op8, op16) | CLI `restore`, `restore-all`, GUI | missing | unverified | W | high: writes stored slots | 1 evening + local test |
| JSON export, `.hlx` import as edits (`--dry-run`), `.hlx`/`.hxb` conversion | CLI `export`, `import`, `export-hlx`, `export-hxb` | missing | export confirmed (`fixtures/export.json`) | R / W | import = many op30s | 1 evening |
| Device settings read/write (op24 / op25) | CLI `setting`, `set-setting`, GUI settings page | missing | unverified; ids are numeric, few named | R / W | write: type must match, ids mostly unnamed | 3 h read, more for write |
| Global EQ read/write (op76, op25 ids 192–203) | GUI EQ page with curve | missing | unverified | R / W | write via op25 | 1 evening |
| Favourite blocks (op112–117) | GUI | missing | unverified | R / W | low | 3 h |
| Library: keep tones, setlists, audition a tone in the edit buffer | GUI (Library pane, `audition.rs`) | missing | unverified | W | doc write per audition | several evenings |
| Impulse responses (op9–15) | CLI `ir-*`, GUI | missing | **no**: HX Effects has no IR block (`fixtures/irs.txt` empty) | – | – | – |

## Out of scope

StompStation PRO (`tonepush pro`, `voidx-*`), and desktop-only parts: installers and the
self-updater, file dialogs, drag and drop, the TonePush cloud (publish, download), USB
capture decoding (`decode`), firmware updates (AGENTS.md 6 forbids them anyway).

## Ranked top 5

1. **Parameter metadata + sliders.** Read-only catalog data plus the op30 the bridge
   already sends. Biggest daily win on the iPad. *(Task 2; UI revamp)*
2. **Snapshot switch + active snapshot lit.** One opcode, refused cleanly when out of
   range; the snapshot bar is already drawn. *(a)*
3. **Tempo set + tap tempo.** Tap is client-side; the write is a doc write, so send one
   write after the taps settle, never one per tap. *(a)*
4. **Catalog categories and colours in `/api/chain`.** Replaces the name-guessing in
   `app.js`; read-only. *(UI revamp)*
5. **Preset backup from the iPad.** Read-only, already shown to work on all presets of
   an HX Effects (#9); a safety net before more write features. *(neither; safety)*

Next in line: model change with a browser (UI revamp), topology lanes (UI revamp),
snapshot rename and the snapshot matrix (a).

## Grouping

- **(a) Snapshots + tap tempo:** snapshot switch, active snapshot, snapshot rename,
  copy snapshot, snapshot matrix, tempo set, tap tempo, external-clock flag.
- **(b) UI revamp:** parameter metadata and sliders, categories/colours/artwork,
  topology lanes, model browser and model change, controller and footswitch views.
