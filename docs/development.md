# Development

Notes for working on Helix Bridge itself. Project rules, architecture and phases:
[AGENTS.md](../AGENTS.md). HTTP API: [api.md](api.md). Pi install: [deploy.md](deploy.md).

## Running the server (`server/`)

1. Against recorded fixtures (no device needed):
   `cd server && cargo run` then open http://localhost:8080/api/chain.
   With writes: `HELIX_TOKEN=dev cargo run`. The fake keeps writes in memory until
   restart; it has one recorded chain, so every preset shows it.
   - `HELIX_FAKE_DELAY_MS=1500` shows loading states; `HELIX_FAKE_SPLIT=1` starts the
     loaded preset with two blocks on Path B (synthetic; the fixtures have no split).
2. Against the real device (quit HX Edit first, `tonepush` on `PATH`):
   - Linux/macOS: `HELIX_BACKEND=cli cargo run`
   - Windows PowerShell, from the repo root:
     `$env:HELIX_BACKEND='cli'; $env:HELIX_TONEPUSH=(Resolve-Path ..\tonepush-bin\tonepush.exe).Path; cd server; cargo run`
   - Building on Windows needs the MSVC linker: install Visual Studio Build Tools with the
     "Desktop development with C++" workload
     (`winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive"`).
     The `windows-gnu` toolchain fails without a separate `dlltool.exe`.
3. Checks before pushing: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`, `cargo test -p hx-usb`, `shellcheck deploy/*.sh`, and the web tests below.

Development-only settings, next to the ones in the README:

| Variable | Default | Meaning |
|---|---|---|
| `HELIX_FIXTURES` | bundled `server/fixtures` | Fixture directory for `fake` |
| `HELIX_SESSION_COOLDOWN_MS` | `3000` | `cli` only: gap between two `tonepush` sessions. 3 s was needed on Windows; try `500` on the Pi |
| `HELIX_FAKE_DELAY_MS` | `0` | `fake` only: wait before each reply |
| `HELIX_FAKE_SPLIT` | unset | `fake` only: `1` puts two synthetic blocks on Path B |

## Running `tonepush` on Windows

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

Recorded output lives in `server/fixtures/` (see its README). Local vs cloud work:
AGENTS.md section 10.

## Web UI (`web/`)

Plain HTML, CSS and JS, no build step. The server compiles the files into its binary, so
rebuild (`cargo run`) after editing them. Design: `docs/design/stage-and-studio-layouts`
(direction 2A "Rig"); the bank tiles from that design are gone, the Presets sheet loads
presets. Never use rotary knobs for parameters: the UI is for touch screens.

Animation: [Motion](https://motion.dev) 13.4.6 (MIT), vendored as one file in `web/lib/`
with its license. To bump it, replace the file and change the version in its name in
`web/index.html` and `server/src/api.rs` (the browser caches it by name).

Web tests (Node 22; Chromium from Playwright, never `playwright install` in the cloud):

1. Rate limiter unit tests: `node --test web/tests/latest.test.mjs`
2. Drag check against the fake backend: `cargo build --manifest-path server/Cargo.toml`,
   then `NODE_PATH=$(npm root -g) node web/tests/drag.mjs` (needs `playwright` installed
   globally; about 15 s).

## Builds and releases

GitHub Actions (`.github/workflows/release.yml`) checks the server and, in parallel,
cross-compiles `helix-bridge` and TonePush's `tonepush` for `aarch64-unknown-linux-gnu`
(glibc 2.31, `cargo-zigbuild`). Every push uploads a `pi-bundle` workflow artifact;
pushing a `v*` tag also creates a GitHub release with the tarball. The Pi's updater
installs the newest green build of the branch it follows
(`sudo helix-bridge-update --branch NAME`).

Build the bundle locally (Linux or WSL, needs `zig` and `cargo-zigbuild`):

1. `cd server && cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.31`
2. Same in a TonePush `v0.9.0` checkout: `cargo zigbuild --release -p tonepush-cli --target aarch64-unknown-linux-gnu.2.31`
3. `deploy/package.sh <version> server/target/aarch64-unknown-linux-gnu/release/helix-bridge <tonepush>/target/aarch64-unknown-linux-gnu/release/tonepush <tonepush> dist`

## Patched `hx-usb`

Live sync needed a fix to TonePush's `hx-usb`, which stalled the pedal's notifications
after 195 (A/B tested: upstream 195, patched 1095 in 3 min). The patched copy lives in
`third_party/hx-usb` ([PATCHES.md](../third_party/hx-usb/PATCHES.md)), wired in with
`[patch]` in `server/Cargo.toml`. Drop it once a TonePush release carries the fixes.

## Status and hardware verification

Phases 1 to 6 of AGENTS.md are done; phase 7 (move to the Pi 3A+) is deferred, the Pi 4
is the working target. Pi 4 baseline: about 220 MB RAM used for the whole system with
the service running.

Verified on the HX Effects, firmware 3.80, from iPad Safari over Wi-Fi:

| Date | What | Setup |
|---|---|---|
| 2026-10-12 | Read-only API and web UI | Server on Windows 11, `tonepush` v0.9.0, build `d972399` |
| 2026-10-12 | Install on the Pi 4 with `install.sh`, HX Edit model names | Bundle `0.1.0-bfaeb24`, 64-bit Raspberry Pi OS Lite |
| 2026-10-13 | Live sync over the WebSocket (`usb` backend) | Pi 4 |
| 2026-10-14 | Load, block on/off, parameters (slider drags, menu, switch, 10 s fast drag), save, rename, snapshot switch, tap and typed tempo; behind `HELIX_TOKEN` | Pi 4, build `a04993e`; first writes on a spare slot |
| 2026-10-15 | Add and remove blocks (checked in Signal Flow view), footswitch assign (checked by hand) | Pi 4, build `d211589` |
| 2026-10-22 | Block moves on Path A, onto and off Path B; split and merge moves; EXP 1/2 assign, take off, heel/toe range; path layout (`probe-slot`: split 12, Path B 13–21, join 22); unsaved edits survive a restart, a snapshot switch alone is no edit | Pi 4, preset 01A |
| 2026-10-25 | A pedal restart clears a stale unsaved flag; a footswitch round trip reads saved; adding onto Path B keeps the split and merge around the whole line; footswitch numbering agrees with the preset's own table | Pi 4, preset 01A |

Moves are the server's own preset writes (op21), not `tonepush move`, which TonePush
marks untested on hardware. Untested on hardware: other HX models; snapshot-controlled
parameter values in the unsaved comparison; expression pedal assignments after a move.
