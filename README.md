# Helix Bridge

Control a Line 6 HX-family device from an iPad over Wi-Fi, via a Raspberry Pi bridge.

```
[Line 6 HX device] --USB--> [Raspberry Pi] --Wi-Fi/HTTP--> [iPad Safari]
```

Built on [TonePush](https://github.com/crmne/tonepush) (MIT), pinned to tag `v0.9.0`.
See [AGENTS.md](AGENTS.md) for architecture, constraints and phases.

Device: HX Effects, firmware 3.80.

Status: Phase 2 (desktop smoke test) done. Next: Phase 3 (server skeleton, cloud).

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
