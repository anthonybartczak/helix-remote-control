# Helix Bridge HTTP API

All responses are JSON. Errors are `{"error": "<message>"}` with the status below.
Every device endpoint runs one `tonepush` command; calls are serialised, so expect
about 1.5 s for a call after a pause, about 5 s for back-to-back calls: the server keeps
`tonepush` sessions 3 s apart, because Windows holds the USB interface briefly after each
one. `/api/device` is cached after the first success (cleared when the device is unplugged).

| Method | Path | Device command | Returns |
|---|---|---|---|
| GET | `/api/health` | none | `{status, backend, version, writes, live}` |
| GET | `/api/device` | `tonepush info` | `{name, presets, device_id, firmware, build}` |
| GET | `/api/presets?setlist=0` | `tonepush presets --setlist=N` | `{setlist, presets: [{index, label, name, loaded}]}` |
| GET | `/api/preset` | `tonepush preset` | `{setlist, index, label, name, firmware, build}` |
| GET | `/api/chain` | `tonepush chain` | `{label, name, tempo, snapshots, blocks, unsaved}` |
| GET | `/api/models/{id}` | none (HX Edit catalog) | Parameter metadata for sliders, see below. 404 `no_catalog` without it |
| GET | `/api/catalog` | none (HX Edit catalog) | Models a block can be added with, see below. 404 `no_catalog` without it |

`unsaved` is true after any edit of the buffer, through the bridge or on the pedal (a
knob or bypass notification), until the next load or save. When the `usb` backend opens a
session (start, restart, reconnect) it compares the edit buffer with the stored copy of
the loaded preset (opcode 4, which loads nothing) and sets `unsaved` if they differ, so
edits survive a restart; `helix-bridge probe-unsaved` (read-only, service stopped)
prints the differences. A snapshot switch alone is no edit, as for `/api/snapshot`: when
the snapshots differ, the stored copy is first recalled to the buffer's snapshot (index
and block on/off from its snapshot table), and values under the snapshot controller,
the tempo and footswitch lights are left out. Verified on the HX Effects 3.80
(2026-10-22): a freshly loaded preset stays saved; so does one switched to another
snapshot; a block turned off on top of that, and a Path B block added on the pedal, are
found after a restart, and Load asks first. Key 10 of a footswitch table entry
(`3/8/switch/entry/10`) is left out too: opcode 56 sets it to 7 on every assign, presets
made on the pedal hold 0, and no opcode sets it back, so a block moved to another
footswitch and back reads saved after a restart. Other backends start saved.

## Writes (phase 6)

All need `Authorization: Bearer <HELIX_TOKEN>` and a JSON body. Without `HELIX_TOKEN` on
the server they return 403 `writes_disabled`; `/api/health` reports `writes: true|false`.
Each returns `{message, unsaved}`, where `message` is what `tonepush` printed. A parameter
write also returns `value`, the new display value (`40 %`), so clients need not re-read
the chain; the UI does this for parameter and bypass edits.

| Method | Path | Body | Device command | Effect |
|---|---|---|---|---|
| POST | `/api/select` | `{index, setlist?}` | `tonepush select --setlist=S -- I` | Load a preset. **Drops unsaved edits** |
| POST | `/api/blocks/{position}/enabled` | `{enabled}` | `tonepush enable -- P on\|off` | Bypass on/off. Edit buffer only |
| POST | `/api/blocks/{position}/params/{index}` | `{value}` or `{native, model}` | `tonepush set -- P I V`; native: `usb`/`fake` only | Set a parameter. Edit buffer only |
| POST | `/api/presets/{index}/rename` | `{name, setlist?}` | `tonepush rename --setlist=S -- I N` | Rename a stored preset. Written at once |
| POST | `/api/save` | `{}` | `tonepush save` | Commit the edit buffer over the loaded preset |
| POST | `/api/snapshot` | `{index}` | `tonepush snapshot -- N` (1-based) | Switch snapshot, zero-based. Does not mark the preset unsaved |
| POST | `/api/tempo` | `{bpm}` | `tonepush tempo -- BPM` | Set the preset's tempo, 40–240, rounded to 0.1. Edit buffer only |
| POST | `/api/blocks/{position}` | `{model}` | `tonepush model -- P M` | Add a block into an empty slot. Edit buffer only |
| DELETE | `/api/blocks/{position}?model=M` | none | `tonepush clear -- P` | Remove a block. `model` optional. Edit buffer only |
| POST | `/api/blocks/{position}/footswitch` | `{switch, model?}` | `tonepush switch-assign -- P S`; `null`: `usb`/`fake` only | Put the block's bypass on footswitch `switch` (1–6), or take it off (`null`). Edit buffer only |
| POST | `/api/blocks/{position}/move` | `{to, model?}` | none: `usb`/`fake` only | Move a block into the empty cell `to` on Path A or Path B. Edit buffer only |
| POST | `/api/routing` | `{junction, before}` | none: `usb`/`fake` only | Put the split (`"split"`) or merge (`"join"`) before cell `before` of `routing.main`. Edit buffer only |
| POST | `/api/blocks/{position}/params/{index}/controller` | `{pedal, model?}` | none: `usb`/`fake` only | Put a parameter under expression pedal `pedal` (1 or 2), or take it off whatever controls it (`null`). Edit buffer only |
| POST | `/api/blocks/{position}/params/{index}/controller/range` | `{min?, max?, model?}` | none: `usb`/`fake` only | Move the heel (`min`) and/or toe (`max`) end of its travel, native values. Edit buffer only |

- `position` and `index` are the numbers `/api/chain` returns. `setlist` defaults to 0.
- `value` is typed in display units, as `/api/chain` shows them (`40`, `40 %`,
  `2.2 kHz`, `-22.1 dB`); TonePush converts with the HX Edit catalog. Without the catalog
  it must be the raw number. 1–32 printable ASCII characters.
- `{native, model}` sets the value as the device stores it (a slider position, see
  "Parameter metadata"). `model` is the block's `model_id` from `/api/chain`; if the slot
  now holds another model the answer is 409 `stale` (re-read the chain). Outside the
  catalog's `min..max`: 400 `bad_request`. Menus are sent as whole numbers and switches as
  booleans, as the device expects (TonePush #7). The `cli` backend refuses it (400).
- `name`: 1–16 printable ASCII characters (HX preset name limit), trimmed.
- Edits change the device's edit buffer: audible at once, lost on the next load unless
  saved. `save` keeps the current name and slot; it never writes another slot.
- User text goes after `--`, so `-22.1` is not read as a flag.
- Snapshot switch is opcode 88 (`{92: index}`); a snapshot the preset lacks is refused by
  the pedal (502 `failed`, "error -46"). Tempo has no opcode: TonePush rewrites the whole
  preset document (opcode 21) and reads it back, about 1 s. Both are confirmed on an HX
  Stomp by TonePush and verified on the HX Effects 3.80 (Pi 4, 2026-10-14).
- Adding a block is op78 (select the slot) then op40 (set its model), as HX Edit does;
  removing is op78 then op28 (clear). Without the select the device answers success and
  does nothing (PROTOCOL.md). `model` is a number from `/api/catalog`; anything else is
  400 `bad_request`. Adding never replaces: a taken slot is 409 `stale`. Any cell of
  `routing.main` or, under the split and merge, of `routing.branch` (`server/src/routing.rs`);
  elsewhere 400. The first block on an empty Path B (any of its cells) is followed by a
  preset document write (op21) attaching the split after the input and the merge before
  the output, as TonePush's GUI does: the device zeroes them when the model lands. The
  HX Effects can then settle the split elsewhere about 0.25 s after that write reads back
  (before cell 2 on 01A), so the server reads it back for a second and writes the claim
  again if it moved; a move onto an empty Path B does the same. Path A and Path B verified
  on the HX Effects 3.80 (2026-10-15, 2026-10-25): first add, a second add, a move onto
  Path B and back, all with the split and merge kept around the whole line. Removing a
  Path B block leaves the split where it is; removing the last one empties Path B. The device may refuse a model (not enough DSP for a polyphonic pitch model, an FX Loop the
  device lacks): 502 `failed` with its error. `usb` reads the preset back until the
  change shows (up to 1 s). Removing with `?model=M` (the block's `model_id`) refuses
  with 409 `stale` if the slot now holds another model; an empty slot is 409 too. `cli`
  cannot check either. Confirmed on an HX Stomp by TonePush and verified on the HX
  Effects 3.80 (Pi 4, 2026-10-15). On the HX Effects op40 answers exactly as on the
  Stomp: the reply carries the rebuilt slot, then notifications 49/47 and 49/10
  `{98: slot}` (slot rebuilt); the slot it writes is byte-for-byte the one the pedal
  writes for a front-panel add (`helix-bridge probe-add`, `probe-slot`). The block gets
  **no footswitch**, while a front-panel add takes a free one; the HX Effects home screen
  shows only footswitch blocks, so look in Signal Flow view, or put it on one with
  `/api/blocks/{position}/footswitch`.
- Moving a block and moving the split or merge rewrite the whole preset document (op21),
  as TonePush's GUI does: read it, change it, write it back, read it again. hx-usb's write
  waits for the commit and zeroes an emptied Path B's attach points first (a document
  saying otherwise is one the pedal settles by wiping the edit buffer, PROTOCOL.md).
  Confirmed on an HX Stomp by TonePush and verified on the HX Effects 3.80 (Pi 4,
  2026-10-22): moves on Path A, onto and off Path B, and split and merge moves read back
  from the pedal as written. Two HX Effects habits the server works around (fixed and
  retested the same day): written in one go with the swap, the first block onto an empty
  Path B left the split where the pedal put it (before cell 3 on 01A), so the line is
  claimed in a second write after reading the pedal back; and a moved block lost its
  footswitch (on neither cell), so the server puts it back on the switches it was on.
  A move swaps the block with the empty cell `to`; nothing else shifts. `to` must be
  empty (else 409 `stale`) and a cell a block may go into: Path A, or Path B under the
  bracket (any Path B cell while Path B is empty: the move claims the whole line). The
  split and merge stay in order, inside Path A (`before` up to `main.length`, the output),
  and around every Path B block; anything else is 400. `model` as for removing. A
  footswitch is not in the document: the switches are read before and after a move, and
  the block is put back on any it lost (opcode 56) and taken off the emptied cell
  (opcode 57). `cli` refuses both (400).
- Expression pedals: opcode 37 (hx-usb `assign_parameter`, source ordinal 1 or 2) puts a
  parameter under a pedal, or off any controller (`null`, ordinal 0); the device starts
  the travel at the parameter's full range. Opcodes 65 and 66 (`set_assign_range`) move
  the heel and toe as a fraction of the parameter's range: the server converts the native
  values with the catalog (`min..max` of the parameter), as TonePush's GUI does; outside
  the range is 400, and so is a range for a parameter nothing controls. Needs HX Edit
  data on the server. Confirmed on an HX Stomp by TonePush and verified on the HX Effects
  3.80 (Pi 4, 2026-10-22): EXP 1 and EXP 2 assign, take off, and heel/toe range all read
  back from the pedal as written. A moved block's assignments stay with the cell on `usb` (the table is keyed
  by slot and a move does not rewrite it); the fake moves them with the block.
- Footswitch: `switch` is one-based as printed on the pedal (FS1–FS6 on the HX Effects),
  `null` takes the block off. `usb` sends opcode 57 (unassign) for any other switch the
  block is on, then opcode 56 (assign), the calls TonePush's GUI makes; both take a
  zero-based switch index on the wire. It then reads every switch back (opcode 33), so
  `/api/chain` shows what the pedal did. A switch can carry several blocks; a block is on
  one switch at most. 0 is 400 `bad_request`, a number past the device's last switch 400
  too; an empty slot, or `model` other than the slot's, 409 `stale`. `cli` can assign but
  not take off (400) and does not take a block off a previous switch. Confirmed on an HX
  Stomp by TonePush; assigning from the iPad worked on the HX Effects 3.80 (Pi 4,
  2026-10-15, checked by hand). The numbering agrees with the preset's own footswitch
  table (opcode 33 and tone key 3/8 on 01A, 2026-10-25).
- Not exposed: changing an existing block's model, snapshot rename, IRs, settings,
  restore. Moves and routing are exposed (`/api/blocks/{position}/move`,
  `/api/routing`) as the server's own preset writes, `usb`/`fake` only; `tonepush move`
  (marked untested on hardware by TonePush) is not used.

## `/api/chain` blocks

```json
{
  "position": 2,
  "model": "Fassel",
  "enabled": false,
  "params": [{ "index": 0, "name": "Position", "value": "2 %" }]
}
```

- `position` is the number `tonepush chain` prints. Phase 6 write commands use it.
  `topology`/`controllers` number the same block `position - 1`.
- `value` is the display string (`"2.2 kHz"`, `"+0.0 dB"`). Without HX Edit data on the
  host, models read `model 269`, params `param 0` and values are raw numbers.
- `cab` (model + params) appears only on Amp+Cab blocks; never on HX Effects.
- `snapshot` (top level): the active snapshot, zero-based, from the preset document
  (tone key 10, key 6, as TonePush's GUI reads it). `usb` and `fake` only; `tonepush chain`
  does not print it, so `cli` omits it. The fake starts on the first snapshot.
- `model_id` (block) and `native` (param) appear only when the server has parameter
  metadata: `usb` with HX Edit data in `/vendor`, or `fake` (synthetic). `native` is the
  value as stored (`0.02` for `2 %`); primary model only.
- `category` (block) is `{"name": "Dynamics", "short": "Dyn", "colour": "#ddcc00",
  "subcategory": "Mono"}`, from HX Edit's catalog exactly as HX Edit labels and tints the
  block; `subcategory` is the Mono / Stereo / Legacy shelf. Only with the catalog (`usb`
  with `/vendor`, or `fake` on a machine with HX Edit installed); without it the UI
  guesses from the model name.
- `based_on` (block) is the real gear the model is based on (`"MXR Dyna Comp"`), from the
  Pilot's Guide via `vendor/based-on.json` (docs/deploy.md step 6). Absent without that
  file, and for models the guide lists no gear for (FX Loop, Send, Return, Volume).
- `footswitches` (top level) lists every footswitch as `{"switch": 3, "label": "Boost",
  "blocks": [4]}`: one-based switch, the name typed for it (absent when none), and the
  `position`s of the blocks whose bypass it switches. `footswitch` (block) is the switch
  the block is on, absent when none. Read with opcode 33, one round trip per switch, on
  every chain read except a live re-read after a knob or bypass edit that left the slots
  as they were; a footswitch bypass is not in the preset document (PROTOCOL.md). `usb`
  decodes the reply itself and skips entries it cannot read: hx-usb's strict decoder
  counts one as a protocol error, which would end the session (a precaution; not seen
  on hardware).
  Opcode 33 also lists parameters a switch drives; an entry the document's controller
  table (key 4, footswitch n = ordinal n + 2) holds as a parameter is left out, as
  TonePush's GUI does. `usb` and `fake` only (`footswitches` absent with `cli`, or when
  the pedal refuses the read). The fake starts with no block on a footswitch and has
  six switches. Only a block on a footswitch shows in the HX Effects' Stomp view.
- `assignments` (block) lists what controllers move on it, from the preset's controller
  table (document key 4, TonePush `Preset::assignments`):
  `{"param": 0, "source": "exp1", "min": 0.0, "max": 1.0, "min_value": "0 %",
  "max_value": "100 %"}`. `param` is absent when the controller switches the block on and
  off. `source` is `exp1`/`exp2` for the expression pedals (confirmed on the HX Effects by
  the recorded `controllers.txt`), else `ctlN` with the device's source ordinal N
  (footswitches, MIDI, snapshots; their numbering is unconfirmed on the HX Effects,
  TonePush #9, so the UI only shows them). `min`/`max` are the heel and toe ends in the
  parameter's native units, `*_value` as the device shows them (with the catalog).
  Absent when empty, and with `cli`. The fake starts with the recording: Fassel's Position
  and on/off on pedal 1, full range.
- `routing` (top level) is how the blocks are wired:
  `{"main": [2, …, 10], "branch": [13, …, 21], "split": {"position": 12, "before": 1},
  "join": {"position": 22, "before": 5}}`. `main` and `branch` are the `position`s of
  Path A's and Path B's cells, left to right, occupied or not; Path B's cell `n` is drawn
  under Path A's cell `n`, as HX Edit does. `split` and `join` are the fork and the
  merge: their own `position`, and `before`, the cell of `main` (zero-based) they sit just
  before (`main.length` = before the output). Path A's blocks before the split and after
  the merge carry the whole signal; between them Path A and Path B run side by side.
  `before` is absent while Path B has no block (the device keeps an empty branch's attach
  points at zero), and so is all of `routing` with `cli`. From TonePush's
  `Preset::layout` (PROTOCOL.md, "The slot array is a topology"). Confirmed on the HX
  Effects 3.80 with `probe-slot` (2026-10-22): split 12, Path B 13–21, join 22, the
  layout of TonePush's HX Stomp capture, which the fake uses too. The fake
  starts with Path B empty; `HELIX_FAKE_SPLIT=1` gives it a synthetic one.
- Preset names are not unique (factory slots are all `New Preset`). Key on `index`.

## Status codes

Device errors carry a stable `code` so clients need not parse messages:
`{"error": "tonepush failed: …", "code": "no_device"}`.

| Status | `code` | Cause |
|---|---|---|
| 400 | – | Bad query string or JSON body, e.g. `setlist=x` (plain-text body from axum; 415/422 for wrong content type or fields) |
| 400 | `bad_request` | Write input failed checks: block 0, empty or too-long name/value, non-ASCII, native value out of range, neither or both of `value`/`native` |
| 401 | `unauthorized` | Write without the right `Authorization: Bearer` token |
| 403 | `writes_disabled` | Write, but the server has no `HELIX_TOKEN` |
| 403 | `bad_host` | Any request, the UI's pages included, whose `Host` is not an IP address, a single name, a local name (`.local`, `.lan`, `.home`, `.home.arpa`, `.internal`, `.localhost`), the machine's hostname under a `search`/`domain` of `/etc/resolv.conf` (read per refused request, so a new network needs no restart), or in `HELIX_ALLOWED_HOSTS`. Stops DNS rebinding: a site pointing its own name at the Pi. A request that accepts `text/html` gets an HTML page linking to `http://<hostname>.local` instead of JSON |
| 403 | `cross_origin` | Any request with an `Origin` that is not this server (scheme default port assumed). Browsers send it on writes and WebSockets; CORS alone does not protect `/api/live` |
| 404 | `no_fixture` | Fake backend has no fixture for the request (only setlist 0 is recorded) |
| 404 | `no_hotspot` | `/api/hotspot` off the Pi: no state directory to save the own-Wi-Fi setting in |
| 404 | `no_catalog` | `/api/models/{id}`: no HX Edit data on the server, or an unknown model. `/api/catalog`: no HX Edit data |
| 409 | `stale` | Native write, remove or footswitch for a model that is no longer in that slot; add to a taken slot; remove from or footswitch an empty one |
| 500 | `storage` | `/api/hotspot` could not save its file (message says why) |
| 502 | `failed` | `tonepush` exited with an error (message is its stderr) |
| 502 | `parse` | `tonepush` printed unexpected output |
| 503 | `no_device` | `tonepush` found no HX device on USB (`no HX device found`) |
| 503 | `spawn` | `tonepush` could not be started (not installed, wrong `HELIX_TONEPUSH`) |
| 504 | `timeout` | `tonepush` ran longer than `HELIX_TONEPUSH_TIMEOUT_SECS` |

"could not claim interface" failures are retried 3 times (after 1 s, 3 s, 8 s) before
returning 502.

## Parameter metadata: `GET /api/models/{id}`

Read from HX Edit's catalog (`hx_catalog::Param`), no device call. Fetch once per model
and keep it: it never changes while the server runs.

```json
{
  "id": 269, "name": "Fassel",
  "params": [
    { "index": 0, "name": "Position", "kind": "continuous", "min": 0, "max": 1,
      "default": 0.5, "scale": "linear", "steps": 100, "labels": ["0 %", "1 %", "…", "100 %"] }
  ]
}
```

- `kind`: `continuous` (slider), `menu` (picker), `switch` (toggle) or `text` (no control,
  type the value).
- Grid position `i` (0..=`steps`) is the native value `min + i/steps·(max−min)`, or
  `min·(max/min)^(i/steps)` when `scale` is `log` (ranges spanning 20× or more: frequencies,
  decay times). `labels[i]` is what HX Edit shows there, so a UI can show the value under
  the finger without a request. Menus: `labels[i]` is the value `min + i`; values HX Edit
  has no name for are left off the ends (`min`/`max` narrowed; 70s Chorus Note Sync
  1..19 has names for 1..18). Switches: `labels` is `[off, on]`.
- Steps: 0.01 on a 0..1 knob, 1 dB on −120..+12 dB, 200 log steps; at most about 150 linear
  steps. Values between grid positions stay reachable by typing.
- `synthetic: true` marks the fake backend's made-up ranges (`server/src/fake.rs`,
  `synthetic`). They reproduce the recorded display values, not HX Edit's data.

Rate limits for native writes:

- **Client** (`web/latest.js`): at most one write in flight, starts at least 100 ms
  apart, last value wins per parameter; nothing queues.
- **Server** (`usb`): parameter writes queued behind each other for the same parameter
  are folded into the newest (every caller gets its answer), and op30s go out at least
  40 ms apart. HX Edit sends one op30 per move step (TonePush opcodes.md 4.3); this
  device wedges when hammered (PROTOCOL.md), so the bridge stays well below that.
- A native write uses the cached document for the model (no preset read per step).

## Model list: `GET /api/catalog`

The models a block can be added with, from HX Edit's catalog. No device call; fetch once.
The server builds the answer at the first request and keeps it: the catalog and
`based-on.json` are read at start-up, so new files in `/vendor` need a restart.

```json
{
  "categories": [
    { "name": "Delay", "short": "Delay", "colour": "#00cc00",
      "types": ["Digital", "Analog", "Tape & Echo", "Multi-tap & Rhythmic", "Creative"],
      "shelves": [
        { "name": "Mono", "models": [{ "id": 80, "name": "Simple Delay", "type": "Digital", "based_on": "Line 6 Original" }] },
        { "name": "Stereo", "models": [{ "id": 92, "name": "Simple Delay", "type": "Digital", "based_on": "Line 6 Original" }] }
      ] }
  ]
}
```

- `type` and `types` are effect types inside a category: Distortion splits into Boost,
  Overdrive, Distortion, Fuzz, Bass, Lo-fi. **Ours, not Line 6's**: HX Edit and the
  device group only by category and shelf. The table is `server/src/types.rs`, written
  from the gear each model is based on; a test fails if a model in HX Edit's catalog has
  no type. `types` lists, in display order, only types some model has; empty (and no
  `type`) for Wah, Volume/Pan and Send/Return.

- Categories and shelves in HX Edit's order: Distortion, Dynamics, EQ, Modulation, Delay,
  Reverb, Pitch/Synth, Filter, Wah, Volume/Pan, Send/Return. Left out, as the HX Effects
  has none: Amp, Amp+Cab, Preamp, Cab, IR, Looper; also Favorites and the structural
  categories (input, output, split, merge).
- `id` is the device's model number for `POST /api/blocks/{position}`. HX Edit's catalog
  keeps one model for the mono and stereo versions; the device numbers them apart
  (`HD2_DelaySimpleDelayMono` 80, `…Stereo` 92), so the same name has another `id` on
  each shelf.
- Models HX Edit marks `hidden` (older versions kept so old presets load, e.g. a second
  Glitch Delay and Poly Sustain) are left out, as HX Edit's browser does. So are models
  whose `devices` list in HX Edit's `.models` files lacks the HX Effects (`0x00210005`):
  FX Loop, Send and Return 3, 4 and 3/4 are Helix-only.
- `based_on` needs `vendor/based-on.json`, as in `/api/chain`.
- `fake`: the catalog when the machine has HX Edit, else the 4 recorded models.

## Own Wi-Fi for gigs: `/api/hotspot`

The Pi's own Wi-Fi, so nobody on a venue's network can reach the bridge. The server runs
unprivileged, so it only saves the setting, to `hotspot.conf` in systemd's
`STATE_DIRECTORY` (`/var/lib/helix-bridge`; `HELIX_HOTSPOT_FILE` overrides). The root
`helix-bridge-hotspot` service applies it at the next start-up (`deploy/hotspot.sh`):
nothing switches at once, as that would cut off the client that asked. Elsewhere (no
state directory) both calls return 404 `no_hotspot` and the UI hides the setting.

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/api/hotspot` | – | `{mode, name, password_set}`; with the token also `password` (reads are open to anyone on the network) |
| POST | `/api/hotspot` | `{mode, name?, password?}` (token) | The same, with `password` |

- `mode`: `off` (the default: the saved Wi-Fi only), `auto` (a saved Wi-Fi if one is in
  range at start-up, else its own; "Away" in the UI) or `on` (always its own).
- `name`: 1–32 printable ASCII characters, trimmed; default `Helix Bridge`. Left out:
  kept.
- `password`: WPA2, 8–63 printable ASCII characters. Left out: kept. `auto` and `on`
  need one (400 `bad_request` otherwise), so the Pi never makes an open Wi-Fi.

## Live updates: `GET /api/live` (WebSocket)

With `HELIX_BACKEND=usb` (and `fake`, which echoes its own writes) `/api/health` reports
`live: true` and this socket pushes JSON messages. Read-only: no token, but a page from
another site is refused (403 `cross_origin`).

| Message | When |
|---|---|
| `{"type": "chain", "chain": {…same as /api/chain…}}` | The loaded preset's chain changed: footswitch, knob, preset or snapshot switch on the pedal, or any client's write. Carries `model_id`/`native` like `/api/chain` |
| `{"type": "device", "connected": true\|false}` | The USB session came up or was lost |

How the `usb` backend sees changes: it reads the pedal's notifications every 700 ms
(TonePush's own pace). Bypass (event 49) and parameter (event 30) notifications carry
the new value, which is applied to the last-read preset and pushed at once: a re-read
right after a change can still return the old document. Other events (preset or
snapshot switch, anything unknown) re-read the chain 250 ms after a burst settles (at
most 1 s after it starts). Ids follow TonePush `docs/_reference/opcodes.md` section 3; any event
not known to be noise triggers a re-read. Only real changes are sent; a preset load
always is. `unsaved` turns true on a pedal edit (events 30, 49) and false on a preset
load (4, 8, 22/object 28).

Because those notifications keep the server's copy current, the `usb` backend answers
`GET /api/chain` from that copy when the session's events channel answered, no re-read is
due and the footswitches are known; otherwise it reads the pedal. Once a minute, after
10 s without a command, it reads the chain again anyway and pushes it if it differs from
what clients were last sent (a lost notification), logging
`periodic check: the pedal changed without a notification`. The UI, for its part,
compares its screen with `GET /api/chain` every 30 s and takes the server's if they
differ; after a write it waits for the push instead of re-reading (a preset load still
re-reads).

## Web UI

`GET /`, `/app.css`, `/app.js`, `/latest.js` serve the UI from `/web`. The files are compiled into
the binary (`include_str!`), so edit them and rebuild; sent with `Cache-Control: no-cache` and an
`ETag` (a hash of the file). The browser keeps them and asks before each use
(`If-None-Match`); while the file is the same the answer is `304 Not Modified` with no body, so
only the first page load after an update downloads the UI. `/lib/motion-13.4.6.js` has its
version in the path and is kept for a year without asking.

Live state over a WebSocket (`/api/live`, below) needs the `usb` backend, which holds
one device session; the `cli` backend opens a new one per command.
