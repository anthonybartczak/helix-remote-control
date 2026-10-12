# Helix Bridge

Control a Line 6 HX-family device from an iPad over Wi-Fi, via a Raspberry Pi bridge.

```
[Line 6 HX device] --USB--> [Raspberry Pi] --Wi-Fi/HTTP--> [iPad Safari]
```

Built on [TonePush](https://github.com/crmne/tonepush) (MIT), pinned to tag `v0.9.0`.
See [AGENTS.md](AGENTS.md) for architecture, constraints and phases.

Device: HX Effects, firmware 3.80.

Status: Phase 3 (read-only server) done. All four device endpoints verified on HX Effects
firmware 3.80 (build `d972399`) via `tonepush` v0.9.0 on Windows 11, 2026-10-12.
Next: Phase 4 (web UI, cloud).

## Running tonepush on Windows (dev machine)

Uses the official TonePush v0.9.0 release binary (building from source needs the
Windows SDK). Quit HX Edit first.

1. Download `tonepush-v0.9.0-x86_64-pc-windows-msvc.zip` from
   https://github.com/crmne/tonepush/releases/tag/v0.9.0 and check its SHA-256 against
   the release's `checksums.txt`.
2. Unzip next to this repo, e.g. `..\tonepush-bin\`.
3. For model and parameter names, point TonePush at HX Edit's data (it does not find the
   default Windows install on its own):
   `$env:HX_EDIT_RESOURCES = 'C:\Program Files (x86)\Line6\HX Edit\res'`
4. Run read-only commands, e.g. `tonepush.exe list`, `info`, `presets`, `chain`.
5. Leave about 3 seconds between commands, or the next one may fail to claim the device.

Recorded output lives in `server/fixtures/` (see its README).

Local vs cloud split: see AGENTS.md section 10.

## Running the server (`server/`)

Read-only HTTP API over the device. Endpoints: [docs/api.md](docs/api.md).

1. Against recorded fixtures (no device needed):
   `cd server && cargo run` then open http://localhost:8080/api/chain
2. Against the real device (quit HX Edit first, `tonepush` on `PATH`):
   - Linux/macOS: `HELIX_BACKEND=cli cargo run`
   - Windows PowerShell, from the repo root:
     `$env:HELIX_BACKEND='cli'; $env:HELIX_TONEPUSH=(Resolve-Path ..\tonepush-bin\tonepush.exe).Path; cd server; cargo run`
   - Building on Windows needs the MSVC linker: install Visual Studio Build Tools with the
     "Desktop development with C++" workload
     (`winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive"`).
     The `windows-gnu` toolchain fails without a separate `dlltool.exe`.
3. Checks before pushing: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

| Variable | Default | Meaning |
|---|---|---|
| `HELIX_BACKEND` | `fake` | `cli` = real device via `tonepush`; `fake` = replay `server/fixtures/` |
| `HELIX_BIND` | `0.0.0.0:8080` | Listen address. LAN only (AGENTS.md 5.7) |
| `HELIX_TONEPUSH` | `tonepush` | Path to the `tonepush` binary |
| `HELIX_TONEPUSH_TIMEOUT_SECS` | `30` | Kill a `tonepush` call that runs longer |
| `HELIX_FIXTURES` | bundled `server/fixtures` | Fixture directory for `fake` |
| `RUST_LOG` | `info` | Log filter |

`HX_EDIT_RESOURCES` is read by `tonepush` itself, not by the server; set it for model names.
