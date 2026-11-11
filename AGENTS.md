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

- Line 6 **HX Effects**, firmware 3.80 (latest). USB ID expected `0e41:4245`.
  - 32 banks × 4 presets (`A`–`D`) = 128 presets; label = `index / 4 + 1` + letter
    (index 104 = `27A`). Six footswitches.
  - Not hardware-tested by TonePush's author; support comes from a user report (upstream
    issue #9). Controller source ordinals (3–8 footswitches, 9 MIDI CC, 10 Snapshots) are
    **unconfirmed** on this device.
  - Untouched factory presets may use the older 10-word section table; writes must keep the
    form the document arrived in (TonePush handles this).
- Dev target 1: Raspberry Pi 4 (already owned) — used first for testing.
- Dev target 2: Raspberry Pi 3 Model A+ (512 MB RAM, 1.4 GHz quad Cortex-A53, one USB-A port,
  dual-band Wi-Fi) — final deployment target.
- OS on the Pi: Raspberry Pi OS Lite, **64-bit**, headless.
- Build target: `aarch64-unknown-linux-gnu`.

## 3. Foundation: TonePush

Upstream: https://github.com/crmne/tonepush

- Open-source editor for Line 6 HX-family devices, written in Rust.
- **Pinned to tag `v0.12.1`** (commit `dcdedcf`). Never track `main` blindly. Bump only on
  purpose, with a note in the commit message.
- **License: MIT.** Linking, modifying and redistributing are allowed; keep the copyright and
  license notice with any shipped TonePush code.
- Workspace crates: `hx-proto` (pure codec, no I/O, no deps), `hx-usb` (USB transport on
  `nusb`), `hx-catalog` (reads HX Edit's model catalog), `tonepush-cli` (the `tonepush` CLI),
  `tonepush-gui` (egui desktop GUI — not used here). `voidx-*` crates are for another vendor's
  pedal — not used here.
- `PROTOCOL.md`, `docs/_reference/opcodes.md` and `docs/_reference/model-catalog.md` document
  the reverse-engineered protocol. Read them before designing the API.
- Hardware-tested on HX Stomp firmware 3.80 only. HX Effects: see section 2.
- Device hazards (from `PROTOCOL.md`): never call USB reset; handshake once per session; keep a
  read posted so device notifications never back up; a wedged device needs its power adapter
  pulled, not a USB replug. Do not send MIDI clock while a session is open.
- Local reference clone (dev machine only): `../tonepush`. Cloud sessions fetch it from GitHub.
- Dev machine: HX Edit is installed at `C:\Program Files (x86)\Line6\HX Edit`. TonePush does
  not find it there; set `HX_EDIT_RESOURCES=C:\Program Files (x86)\Line6\HX Edit\res`. On the
  Pi, copy that `res` folder into `/vendor` and point `HX_EDIT_RESOURCES` at it.

## 4. Architecture (decided)

- **Server:** Rust, `axum` + `tokio`. One binary, runs as a systemd service on the Pi.
- **Device access, phase 1:** wrap the `tonepush` CLI (spawn process, parse output).
- **Device backend trait:** all device access goes through one Rust trait with two
  implementations: `CliDevice` (real, spawns `tonepush`) and `FakeDevice` (replays recorded
  output from `server/fixtures/`). Selected at startup by config/env. See section 10.
- **Device access, phase 2 (optional):** link `hx-usb` / `hx-catalog` as library crates directly,
  only if the CLI wrapper proves too slow or too limited.
- **Web UI:** static HTML + vanilla JS (or a tiny framework with no build step), served by the
  same binary. No Node.js toolchain on the Pi.
- **Transport to UI:** REST for actions, WebSocket for live state (current preset, param changes).
- **Network:** LAN only. Reachable as `http://helix-bridge.local` via Avahi/mDNS.
- **Repo layout:**
  ```
  /server        Rust crate (axum)
  /server/fixtures  recorded real-device output for FakeDevice (committed)
  /web           static UI files
  /deploy        systemd unit, udev rule, install script
  /docs          notes, API description
  /vendor        (gitignored) HX Edit data files
  ```

## 5. Hard constraints

1. **Never compile on the Pi.** 512 MB RAM is not enough for `cargo build`. Cross-compile on the
   dev machine with `cross` or `cargo-zigbuild` (or in GitHub Actions, see section 10), then
   copy the binary.
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

Tags: **[local]** needs the dev machine, device or Pi; **[cloud]** pure code, runs anywhere;
**[cloud → local]** code written in the cloud, verified locally. See section 10.

1. **Recon (≈30 min).** **[local] — DONE.** Clone TonePush, read README + PROTOCOL.md +
   LICENSE. Report: supported operations for the user's device, license summary, CLI commands.
2. **Desktop smoke test + fixtures (≈1 h).** **[local]** Build TonePush on the dev machine,
   connect the device, run read-only CLI commands (`list`, `info`, `presets`, `chain`,
   `topology`, `setlists`, `export`). Report exact output. Save each command's raw stdout to
   `server/fixtures/` (section 10) and push.
3. **Server skeleton, read-only (≈1 evening).** **[cloud → local]** axum server with
   endpoints: health, list presets, current preset, current blocks + params. Developed against
   `FakeDevice`; then verified locally against the real device with `CliDevice`.
4. **Web UI, read-only (≈1 evening).** **[cloud]** iPad-first layout: large touch targets
   (≥44 px), landscape and portrait, works in Safari. Shows presets and the current signal
   chain. Served by the server running on `FakeDevice`.
5. **Deploy to Pi 4 (≈1–2 h).** **[cloud → local]** Cloud writes the GitHub Actions
   cross-compile workflow, systemd unit, udev rule, mDNS config and install script. Local:
   run the install on the Pi, verify from iPad.
6. **Write operations (after backup confirmation).** **[cloud → local]** Preset switch first,
   then param edits, then rename/reorder/save. Add shared-token auth here. Code and
   `FakeDevice` tests in the cloud; every first write on real hardware happens locally, with
   the user's confirmation (section 6).
7. **Move to Pi 3A+.** **[local]** Same binary and install script. Measure RAM with `free -h`
   and report. **Deferred (2026-10-14):** the Pi 4 is the working target for now. When resumed,
   the Pi 4's SD card can move to the 3A+ as is (same 64-bit OS); it needs a micro-USB 2.5 A
   supply. Pi 4 baseline: 218 MB used for the whole system with the bridge running.

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
8. Say whether the next step is **local** or **cloud**.

## 10. Local vs cloud work

The repo lives at https://github.com/anthonybartczak/helix-remote-control (private). Work is
split so pure code can be done in cloud sessions and only hardware work needs the dev machine.

**Rule:** anything touching the HX Effects, the Pi, or HX Edit data files is local. Everything
else may run in the cloud.

| Local only | Cloud OK |
|---|---|
| Running `tonepush` against the device | Server code, tests, `clippy`, `fmt` |
| Recording fixtures | Web UI (HTML/JS/CSS) |
| `/vendor` (HX Edit data files) | Deploy scripts, systemd unit, udev rule |
| SSH to the Pi, install, `free -h` | GitHub Actions cross-compile workflow |
| First write op on real hardware | Docs, README |

**Handoff is git.** Local records → push → cloud codes against it → push → local pulls and
verifies on hardware. Every cloud session starts with `git pull`; ends with a push.

**Fixtures (`server/fixtures/`):**
- One file per CLI command, raw stdout, named after the command: `presets.txt`, `chain.txt`,
  `topology.txt`, `info.txt`, `setlists.txt`, `export.json`, …
- A `fixtures/README.md` records device, firmware, TonePush tag and date of capture.
- Recorded on the real HX Effects only; never hand-edited. Re-record when the TonePush pin
  changes.
- They may contain model names (rendered from HX Edit data) and the user's preset names.
  This is fine while the repo is private; **never commit the HX Edit data files themselves.**
  If the repo is ever made public, review fixtures first.

**In cloud sessions:**
- The device is not reachable. Use `FakeDevice`. Never claim hardware was tested.
- `../tonepush` does not exist. Depend on TonePush via its git URL at tag `v0.12.1`, or fetch
  it to read docs.
- Report which parts still need local verification, as a numbered list.

**Binary for the Pi:** built by GitHub Actions for `aarch64-unknown-linux-gnu` (with
`cargo-zigbuild` or `cross`) and attached to a GitHub release. The Pi downloads it; it never
compiles.
