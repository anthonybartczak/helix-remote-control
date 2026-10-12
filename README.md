# Helix Bridge

Control a Line 6 HX-family device from an iPad over Wi-Fi, via a Raspberry Pi bridge.

```
[Line 6 HX device] --USB--> [Raspberry Pi] --Wi-Fi/HTTP--> [iPad Safari]
```

Built on [TonePush](https://github.com/crmne/tonepush) (MIT), pinned to tag `v0.9.0`.
See [AGENTS.md](AGENTS.md) for architecture, constraints and phases.

Device: HX Effects, firmware 3.80.

Status: Phase 1 (recon) done. Next: Phase 2 (local smoke test + fixtures).

Local vs cloud split: see AGENTS.md section 10.
