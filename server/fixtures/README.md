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
| HX Edit data | top level: **present** (`HX_EDIT_RESOURCES` set); `no-catalog/`: absent |

## Files

| File | Command | Notes |
|---|---|---|
| `list.txt` | `tonepush list` | |
| `info.txt` | `tonepush info` | |
| `setlists.txt` | `tonepush setlists` | one setlist: `FACTORY 1` |
| `presets.txt` | `tonepush presets` | 128 lines; loaded one marked `<- loaded` |
| `preset.txt` | `tonepush preset` | summary of loaded preset |
| `chain.txt` | `tonepush chain` | named models, params with display units |
| `topology.txt` | `tonepush topology` | |
| `controllers.txt` | `tonepush controllers` | |
| `irs.txt` | `tonepush irs` | empty: HX Effects has no IR block |
| `export.json` | `tonepush export <file>` | loaded preset as JSON |
| `no-catalog/*` | same, without HX Edit data | models as `model 269`, params as `param 0`; `chain.stderr.txt` holds the "install HX Edit" hint |

`list`, `info`, `setlists`, `presets`, `preset` and `irs` do not depend on HX Edit data.

## Quirks to handle in the parser

- **Every preset is named `New Preset`.** Preset names are not unique; key on index.
- **Block numbers disagree.** `chain`/`export` number blocks 2, 3, 5, 10; `topology` and
  `controllers` use the slot index (1, 2, 4, 9). Write ops use the `chain` numbering.
- **`export.json` param values change type with the catalog.** With HX Edit data they are
  display strings (`"2.2 kHz"`, `"+0.0 dB"`); without it they are raw floats. Unknown
  params stay numeric even with the catalog (`FX Loop 1` → `param 3`).
- **Back-to-back sessions can fail on Windows** with `could not claim interface 0
  (error 5)`. A 3 s pause usually fixed it; once it needed 8 s. The server must serialise
  device access and retry with backoff — never re-handshake (see PROTOCOL.md).
- Routing labels (`Input [Multi (Guitar, Aux, Variax)]`) come from HX Edit's generic menu
  and are approximate on HX Effects (PROTOCOL.md, opcode 42).
