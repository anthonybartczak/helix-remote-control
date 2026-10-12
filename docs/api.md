# Helix Bridge HTTP API (phase 3, read-only)

All responses are JSON. Errors are `{"error": "<message>"}` with the status below.
Every device endpoint runs one `tonepush` command; calls are serialised, so expect
about 1–3 s per call on real hardware.

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

| Status | Cause |
|---|---|
| 400 | Bad query string, e.g. `setlist=x` |
| 404 | Fake backend has no fixture for the request (only setlist 0 is recorded) |
| 502 | `tonepush` exited with an error (message is its stderr) or printed unexpected output |
| 503 | `tonepush` could not be started (not installed, wrong `HELIX_TONEPUSH`) |
| 504 | `tonepush` ran longer than `HELIX_TONEPUSH_TIMEOUT_SECS` |

"could not claim interface" failures are retried 3 times (after 1 s, 3 s, 8 s) before
returning 502.

Not yet: WebSocket live state (phase 4/6), write operations (phase 6), auth (phase 6).
