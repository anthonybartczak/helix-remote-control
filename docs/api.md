# Helix Bridge HTTP API (read-only)

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
| GET | `/api/chain` | `tonepush chain` | `{label, name, tempo, snapshots, blocks}` |

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
| 400 | – | Bad query string, e.g. `setlist=x` (plain-text body from axum) |
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

Not yet: WebSocket live state (phase 6, once the bridge holds one device session),
write operations (phase 6), auth (phase 6).
