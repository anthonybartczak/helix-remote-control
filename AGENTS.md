# AGENTS.md — Helix Bridge

Project rules for AI coding agents. Read this file fully before starting any task.

## 1. Goal

Build a small Linux "bridge" that controls a Line 6 HX-family device over USB and exposes
a touch-friendly web UI, so the device can be edited from an iPad (Safari) over Wi-Fi.

```
[Line 6 HX device] --USB--> [Raspberry Pi, headless Linux] --Wi-Fi/HTTP--> [iPad Safari]
```

iPadOS cannot talk to the device directly (vendor USB interface, no raw USB access for apps),
so all USB work happens on the Pi.

## 2. Hardware and environment

- Line6 Helix Effect, firmware 3.80.
- Dev target 1: Raspberry Pi 4 (already owned) — used first for testing.
- Dev target 2: Raspberry Pi 3 Model A+ (512 MB RAM, 1.4 GHz quad Cortex-A53, one USB-A port,
  dual-band Wi-Fi) — final deployment target.
- OS on the Pi: Raspberry Pi OS Lite, **64-bit**, headless.
- Build target: `aarch64-unknown-linux-gnu`.

## 3. Foundation: TonePush

Upstream: https://github.com/crmne/tonepush

- Open-source editor for Line 6 HX-family devices, written in Rust.
- Workspace crates: `hx-usb` (USB transport on `nusb`), `hx-catalog` (reads HX Edit's model
  catalog), `hx-cli` (the `tonepush` CLI), `hx-gui` (egui desktop GUI — not used here).
- `PROTOCOL.md`, `docs/_reference/opcodes.md` and `docs/_reference/model-catalog.md` document
  the reverse-engineered protocol. Read them before designing the API.
- Verified on HX Stomp firmware 3.80 only. Helix Floor / LT opcodes are partly unverified.

Before depending on TonePush:
1. Read its LICENSE and tell the user what it allows (linking, redistribution).
2. Pin it to a specific commit or tag. Never track `main` blindly.

## 4. Architecture (decided)

- **Server:** Rust, `axum` + `tokio`. One binary, runs as a systemd service on the Pi.
- **Device access, phase 1:** wrap the `tonepush` CLI (spawn process, parse output).
- **Device access, phase 2 (optional):** link `hx-usb` / `hx-catalog` as library crates directly,
  only if the CLI wrapper proves too slow or too limited.
- **Web UI:** static HTML + vanilla JS (or a tiny framework with no build step), served by the
  same binary. No Node.js toolchain on the Pi.
- **Transport to UI:** REST for actions, WebSocket for live state (current preset, param changes).
- **Network:** LAN only. Reachable as `http://helix-bridge.local` via Avahi/mDNS.
- **Repo layout:**
  ```
  /server        Rust crate (axum)
  /web           static UI files
  /deploy        systemd unit, udev rule, install script
  /docs          notes, API description
  /vendor        (gitignored) HX Edit data files
  ```

## 5. Hard constraints

1. **Never compile on the Pi.** 512 MB RAM is not enough for `cargo build`. Cross-compile on the
   dev machine with `cross` or `cargo-zigbuild`, then copy the binary.
2. **RAM budget on the Pi 3A+:** whole stack (OS + server + tonepush) under ~250 MB.
3. **No desktop GUI on the Pi.** `hx-gui` / egui is out of scope.
4. **HX Edit data files are Line 6's property.** Extract them on the dev machine with TonePush's
   `tools/hxresources/extract.sh`, copy them to the Pi, keep them in `/vendor`, and never commit
   or redistribute them.
5. **HX Edit must be closed** whenever the bridge connects to the device.
6. **USB permissions:** add a udev rule for the Line 6 vendor ID (expected `0e41` — confirm with
   `lsusb`) so the service runs as a non-root user.
7. **No auth in phase 1, LAN only.** Do not expose the server to the internet. Add a simple
   shared-token check before phase 4 (write operations).

## 6. Safety rules for the agent

- **Read-only first.** Do not implement or run any command that writes to the device until the
  user confirms they have a full preset backup (exported from HX Edit).
- **Ask before every first write.** The first time any write operation (rename, change param,
  reorder, save) is run against real hardware, stop and ask the user to confirm.
- **Never update device firmware** and never run firmware-related opcodes.
- **Unknown opcodes stay unused.** If `PROTOCOL.md` marks an operation as unverified for the
  user's device, do not call it on hardware without explicit approval.
- Do not add tokens, passwords or Wi-Fi credentials to the repo. Use a `.env` file that is
  gitignored.

## 7. Phases

Work one phase at a time. Finish a phase, report, wait for the user's go-ahead.

1. **Recon (≈30 min).** Clone TonePush, read README + PROTOCOL.md + LICENSE. Report: supported
   operations for the user's device, license summary, CLI commands available.
2. **Desktop smoke test (≈1 h).** Build TonePush on the dev machine, connect the device, run
   read-only CLI commands (list presets, read current preset). Report exact output.
3. **Server skeleton, read-only (≈1 evening).** axum server with endpoints: health, list
   presets, current preset, current blocks + params. Runs on the dev machine first.
4. **Web UI, read-only (≈1 evening).** iPad-first layout: large touch targets (≥44 px),
   landscape and portrait, works in Safari. Shows presets and the current signal chain.
5. **Deploy to Pi 4 (≈1–2 h).** Cross-compile, systemd unit, udev rule, mDNS. Verify from iPad.
6. **Write operations (after backup confirmation).** Preset switch first, then param edits,
   then rename/reorder/save. Add shared-token auth here.
7. **Move to Pi 3A+.** Same binary and install script. Measure RAM with `free -h` and report.

## 8. Definition of done (per phase)

- Code builds with no warnings (`cargo clippy -- -D warnings`).
- `cargo fmt` applied.
- Anything touching hardware was tested on real hardware, and the report says which device and
  firmware.
- README updated with how to run what was just built.

## 9. How to report to the user

Reports must be quick to scan. Shape every report like this:

1. **First line = what works now or what the user must do next.** No preamble.
2. Numbered steps for anything multi-step. One action per step.
3. Concrete time estimates ("about 20 minutes"), never "a bit of work".
4. Errors: state cause and fix, flat tone. Example: "Build fails: missing libudev-dev.
   Fix: `sudo apt install libudev-dev`."
5. Max 5 items per list. Extra items only on request.
6. End with **one** concrete next action that takes under 2 minutes.
7. Always restate where we are: "Phase 3 of 7 done: …. Next: …".
