# Helix Bridge HTTP API

All responses are JSON. Errors are `{"error": "<message>"}` with the status below.
Every device endpoint runs one `tonepush` command; calls are serialised, so expect
about 1.5 s for a call after a pause, about 5 s for back-to-back calls: the server keeps
`tonepush` sessions 3 s apart, because Windows holds the USB interface briefly after each
one. `/api/device` is cached after the first success (cleared when the device is unplugged).

| Method | Path | Device command | Returns |
|---|---|---|---|
| GET | `/api/health` | none | `{status, backend, version}` |
| GET | `/api/device` | `tonepush info` | `{name, presets, device_id, firmware, build}` |
| GET | `/api/presets?setlist=0` | `tonepush presets --setlist=N` | `{setlist, presets: [{index, label, name, loaded}]}` |
| GET | `/api/preset` | `tonepush preset` | `{setlist, index, label, name, firmware, build}` |
| GET | `/api/chain` | `tonepush chain` | `{label, name, tempo, snapshots, blocks, unsaved}` |

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
| POST | `/api/blocks/{position}/params/{index}` | `{value}` | `tonepush set -- P I V` | Set a parameter. Edit buffer only |
| POST | `/api/presets/{index}/rename` | `{name, setlist?}` | `tonepush rename --setlist=S -- I N` | Rename a stored preset. Written at once |
| POST | `/api/save` | `{}` | `tonepush save` | Commit the edit buffer over the loaded preset |

- `position` and `index` are the numbers `/api/chain` returns. `setlist` defaults to 0.
- `value` is typed in display units, as `/api/chain` shows them (`40`, `40 %`,
  `2.2 kHz`, `-22.1 dB`); TonePush converts with the HX Edit catalog. Without the catalog
  it must be the raw number. 1–32 printable ASCII characters.
- `name`: 1–16 printable ASCII characters (HX preset name limit), trimmed.
- Edits change the device's edit buffer: audible at once, lost on the next load unless
  saved. `save` keeps the current name and slot; it never writes another slot.
- User text goes after `--`, so `-22.1` is not read as a flag.
- Not exposed: reorder (`tonepush move`, marked untested on hardware by TonePush),
  model change, snapshots, tempo, routing, IRs, settings, restore.

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
- Preset names are not unique (factory slots are all `New Preset`). Key on `index`.

## Status codes

Device errors carry a stable `code` so clients need not parse messages:
`{"error": "tonepush failed: …", "code": "no_device"}`.

| Status | `code` | Cause |
|---|---|---|
| 400 | – | Bad query string or JSON body, e.g. `setlist=x` (plain-text body from axum; 415/422 for wrong content type or fields) |
| 400 | `bad_request` | Write input failed checks: block 0, empty or too-long name/value, non-ASCII |
| 401 | `unauthorized` | Write without the right `Authorization: Bearer` token |
| 403 | `writes_disabled` | Write, but the server has no `HELIX_TOKEN` |
| 404 | `no_fixture` | Fake backend has no fixture for the request (only setlist 0 is recorded) |
| 502 | `failed` | `tonepush` exited with an error (message is its stderr) |
| 502 | `parse` | `tonepush` printed unexpected output |
| 503 | `no_device` | `tonepush` found no HX device on USB (`no HX device found`) |
| 503 | `spawn` | `tonepush` could not be started (not installed, wrong `HELIX_TONEPUSH`) |
| 504 | `timeout` | `tonepush` ran longer than `HELIX_TONEPUSH_TIMEOUT_SECS` |

"could not claim interface" failures are retried 3 times (after 1 s, 3 s, 8 s) before
returning 502.

## Web UI

`GET /`, `/app.css`, `/app.js` serve the UI from `/web`. The files are compiled into
the binary (`include_str!`), so edit them and rebuild; sent with `Cache-Control: no-cache`.

Not yet: WebSocket live state (needs the bridge to hold one device session; the CLI
backend opens a new one per command).
