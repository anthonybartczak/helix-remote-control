# Fixtures

Raw stdout of read-only `tonepush` commands, recorded on real hardware.
`FakeDevice` replays these so cloud sessions can work without the device.

Never hand-edit. Re-record when the TonePush pin changes. See AGENTS.md section 10.

| Field | Value |
|---|---|
| Device | HX Effects (`0e41:4245`), device id `0x00210005` |
| Firmware | 3.80, build `d972399` |
| TonePush | `v0.9.0`, official Windows x86_64 release binary |
| Host | Windows 11 |
| Recorded | 2026-10-12 |
| HX Edit data | **absent** — models show as `model 269`, params as `param 0` |

## Files

| File | Command | Notes |
|---|---|---|
| `list.txt` | `tonepush list` | |
| `info.txt` | `tonepush info` | |
| `setlists.txt` | `tonepush setlists` | one setlist: `FACTORY 1` |
| `presets.txt` | `tonepush presets` | 128 lines; loaded one marked `<- loaded` |
| `preset.txt` | `tonepush preset` | summary of loaded preset |
| `chain.txt` | `tonepush chain` | `chain.stderr.txt` holds the "install HX Edit" hint |
| `topology.txt` | `tonepush topology` | |
| `controllers.txt` | `tonepush controllers` | |
| `irs.txt` | `tonepush irs` | empty: HX Effects has no IR block |
| `export.json` | `tonepush export <file>` | loaded preset as JSON |

## Quirks to handle in the parser

- **Every preset is named `New Preset`.** Preset names are not unique; key on index.
- **Block numbers disagree.** `chain`/`export` number blocks 2, 3, 5, 10; `topology`
  numbers the same blocks 1, 2, 4, 9 (slot index). Write ops use the `chain` numbering.
- **Back-to-back sessions can fail on Windows** with `could not claim interface 0
  (error 5)` when a command starts right after the previous one exits. A 3 s pause fixed
  it. The server must serialise device access and retry once after a short delay — never
  re-handshake (see PROTOCOL.md).
- Models/params are numeric until HX Edit data is in `/vendor` on the machine running
  `tonepush`. The server must handle both forms.
