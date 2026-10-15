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

`unsaved` is true after a bypass or parameter change made through the bridge, until the
next load or save. Edits made on the pedal itself are not seen; it resets on restart.

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
  400 `bad_request`. Adding never replaces: a taken slot is 409 `stale`. Only Path A's
  slots (positions 2–10 on the HX Effects, between input and output): the first block on
  an empty parallel branch also needs its split and join rewired, a document write. The
  device may refuse a model (not enough DSP for a polyphonic pitch model, an FX Loop the
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
- Footswitch: `switch` is one-based as printed on the pedal (FS1–FS6 on the HX Effects),
  `null` takes the block off. `usb` sends opcode 57 (unassign) for any other switch the
  block is on, then opcode 56 (assign), the calls TonePush's GUI makes; both take a
  zero-based switch index on the wire. It then reads every switch back (opcode 33), so
  `/api/chain` shows what the pedal did. A switch can carry several blocks; a block is on
  one switch at most. 0 is 400 `bad_request`, a number past the device's last switch 400
  too; an empty slot, or `model` other than the slot's, 409 `stale`. `cli` can assign but
  not take off (400) and does not take a block off a previous switch. Confirmed on an HX
  Stomp by TonePush; assigning from the iPad worked on the HX Effects 3.80 (Pi 4,
  2026-10-15, checked by hand). Switch numbering not cross-checked with
  `probe-switches` yet (README).
- Not exposed: reorder (`tonepush move`, marked untested on hardware by TonePush),
  changing an existing block's model, snapshot rename, routing, IRs, settings, restore.

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
| 404 | `no_fixture` | Fake backend has no fixture for the request (only setlist 0 is recorded) |
| 404 | `no_catalog` | `/api/models/{id}`: no HX Edit data on the server, or an unknown model. `/api/catalog`: no HX Edit data |
| 409 | `stale` | Native write, remove or footswitch for a model that is no longer in that slot; add to a taken slot; remove from or footswitch an empty one |
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

## Live updates: `GET /api/live` (WebSocket)

With `HELIX_BACKEND=usb` (and `fake`, which echoes its own writes) `/api/health` reports
`live: true` and this socket pushes JSON messages. Read-only: no token.

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

## Web UI

`GET /`, `/app.css`, `/app.js`, `/latest.js` serve the UI from `/web`. The files are compiled into
the binary (`include_str!`), so edit them and rebuild; sent with `Cache-Control: no-cache`.

Not yet: WebSocket live state (needs the bridge to hold one device session; the CLI
backend opens a new one per command).
