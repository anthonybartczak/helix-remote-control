# Helix Bridge

Control a Line 6 HX-family device from an iPad over Wi-Fi, via a Raspberry Pi bridge.

```
[Line 6 HX device] --USB--> [Raspberry Pi] --Wi-Fi/HTTP--> [iPad Safari]
```

Built on [TonePush](https://github.com/crmne/tonepush) (MIT), pinned to tag `v0.9.0`.
See [AGENTS.md](AGENTS.md) for architecture, constraints and phases.

Device: HX Effects, firmware 3.80.

Status: Phase 4 (read-only web UI) done. Verified from iPad Safari over Wi-Fi against
HX Effects firmware 3.80 (build `d972399`), server on Windows 11 with `tonepush` v0.9.0,
2026-10-12. Phase 3 endpoints verified on the same setup.

Phase 5 (deploy to Pi 4) verified 2026-10-12: bundle `0.1.0-bfaeb24` installed with
`install.sh` on a Raspberry Pi 4 (64-bit Raspberry Pi OS Lite), HX Effects firmware 3.80
over USB, UI opened from iPad Safari at `http://helix-bridge.local` with HX Edit model
names. Whole Pi: 222 MB RAM used with the service running. Next: Phase 6 (write
operations) after a full preset backup. Install guide: [docs/deploy.md](docs/deploy.md).

Phase 6 (write operations) done and verified 2026-10-14 on the HX Effects (firmware
3.80) via a Raspberry Pi 4 and iPad Safari, behind a shared token (`HELIX_TOKEN`): load
preset, block on/off, parameter value (shows on the pedal almost at once), save
(survives reloading the preset) and rename, each first run on a spare slot with the
user's confirmation. Reordering blocks is left out on purpose: TonePush marks `move`
untested on hardware (AGENTS.md 6). Phase 7 (move to the Pi 3A+) is deferred: the Pi 4
is the working target for now.

Live sync verified 2026-10-13 on the Pi 4 with the HX Effects 3.80: the `usb` backend
holds one USB session and pushes pedal changes (footswitches, knobs, preset switches) to
the iPad over a WebSocket. It needed a fix to TonePush's `hx-usb`, which stalled the
pedal's notifications after 195 (A/B tested: upstream 195, patched 1095 in 3 min). The
patched copy lives in `third_party/hx-usb` ([PATCHES.md](third_party/hx-usb/PATCHES.md)).
Writes and the updater (`sudo helix-bridge-update`) are verified on the Pi 4 too.

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

HTTP API over the device. Endpoints: [docs/api.md](docs/api.md). Writes need
`HELIX_TOKEN`; without it the server is read-only.

1. Against recorded fixtures (no device needed):
   `cd server && cargo run` then open http://localhost:8080/api/chain.
   With writes: `HELIX_TOKEN=dev cargo run`. The fake keeps writes in memory until
   restart; it has one recorded chain, so every preset shows it.
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

| Variable | Default | Meaning |
|---|---|---|
| `HELIX_BACKEND` | `fake` | `usb` = real device, one held USB session, live updates (recommended, the Pi default); `cli` = real device via the `tonepush` binary, one session per call; `fake` = replay `server/fixtures/` |
| `HELIX_BIND` | `0.0.0.0:8080` | Listen address. LAN only (AGENTS.md 5.7) |
| `HELIX_TONEPUSH` | `tonepush` | `cli` only: path to the `tonepush` binary |
| `HELIX_TONEPUSH_TIMEOUT_SECS` | `30` | Kill a `tonepush` call that runs longer |
| `HELIX_FIXTURES` | bundled `server/fixtures` | Fixture directory for `fake` |
| `HELIX_SESSION_COOLDOWN_MS` | `3000` | `cli` only: gap between two `tonepush` sessions. 3 s was needed on Windows; try `500` on the Pi |
| `HELIX_FAKE_DELAY_MS` | `0` | `fake` only: wait before each reply, e.g. `1500` to see loading states |
| `HELIX_TOKEN` | unset | Shared token for writes. Unset or empty = read-only. Keep it in `.env` or the Pi's settings file, never in git |
| `RUST_LOG` | `info` | Log filter |

`HX_EDIT_RESOURCES` points at HX Edit's data for model names. `usb` reads it in the
server process; `cli` passes it on to `tonepush`.

The `usb` backend holds the pedal's USB interface while the server runs, as HX Edit
does: stop the service before using HX Edit or `tonepush` by hand.

## Web UI (`web/`)

Plain HTML, CSS and JS, no build step. The server compiles the files into its binary, so
rebuild (`cargo run`) after editing them. Design: `docs/design/stage-and-studio-layouts`
(direction 2A "Rig").

1. Start the server (see above), e.g. `cd server && cargo run`.
2. Open http://localhost:8080 on the same machine, or `http://<machine-ip>:8080` on the iPad.
3. To see loading skeletons on the fake backend: `HELIX_FAKE_DELAY_MS=1500 cargo run`.

Writes (server needs `HELIX_TOKEN`; the UI asks for it on the first write and keeps it
in the browser):

- **Load a preset:** ‹ ›, a bank tile, or Presets → pick → Load. Asks first if there are
  unsaved edits.
- **Block on/off:** select a block, tap Turn on / Turn off.
- **Parameter:** drag its slider left or right (the value shows under your finger), pick
  a menu entry, or flip a switch. Writes go out at most every 100 ms, one at a time.
  Tap the value to type an exact one in the shown units (`40`, `2.2 kHz`). Sliders need
  HX Edit data on the server (`usb` with `/vendor`); without it every value is typed.
  The `fake` backend uses synthetic ranges. Verified 2026-10-14 on the HX Effects 3.80
  (Pi 4, iPad Safari, build `a04993e`): slider drags, menu, switch, 10 s fast drag with
  footswitch changes still reaching the iPad live.
- **Rename / Save:** buttons in the preset bar. Save asks first and overwrites the
  loaded preset. "Unsaved" in the bar means edits are only in the edit buffer.

Without a token, write controls are dashed and inert. Tap tempo stays dashed (not
built). Known limits:

- **Live updates need `HELIX_BACKEND=usb`.** Then the server pushes each change over
  `/api/live` within about 1 s. With `cli`, the UI reads the device on open, when the iPad
  returns to the page, and on "Refresh from device" (tap the connection dots).
- **The `usb` backend connects 20 s after start**, after the pedal is plugged in or
  powered up, and after a session ends. Sooner, the HX Effects may leave live updates
  off for the whole session (`third_party/hx-usb/PATCHES.md` 2b). The UI shows
  "waiting N s" meanwhile.
- **Block categories are guessed** from the model name (`CATEGORY_RULES` in `app.js`);
  unknown models show grey `FX`. Real categories need the HX Edit catalog on the server.
- **Active snapshot is not shown.** `tonepush chain` does not report it.
- **Sliders snap to a grid** (0.01 on 0..1 knobs, 1 dB on levels). Type for finer values.
  Without HX Edit data (`cli`, or no `/vendor`), parameters are typed and only `%`
  values get a bar.
- **Unsaved flag:** with `usb`, pedal edits count too; with `cli`, only bridge edits.

Web tests (Node 22; Chromium from Playwright, never `playwright install` in the cloud):

1. Rate limiter unit tests: `node --test web/tests/latest.test.mjs`
2. Drag check against the fake backend: `cargo build --manifest-path server/Cargo.toml`,
   then `NODE_PATH=$(npm root -g) node web/tests/drag.mjs` (needs `playwright` installed
   globally; about 15 s).

## Deploying to the Pi (`deploy/`)

GitHub Actions (`.github/workflows/release.yml`) checks the server, then cross-compiles
`helix-bridge` and TonePush's `tonepush` for `aarch64-unknown-linux-gnu` (glibc 2.31,
`cargo-zigbuild`). Every push uploads a `pi-bundle` workflow artifact; pushing a `v*` tag
also creates a GitHub release with the tarball.

Install steps, paths and troubleshooting: [docs/deploy.md](docs/deploy.md). In short:
copy the tarball to the Pi, unpack, `sudo ./install.sh --hostname helix-bridge`, open
`http://helix-bridge.local`. Later updates: `sudo helix-bridge-update` on the Pi
(`deploy/update.sh`; needs a read-only GitHub token, see docs/deploy.md "Updating").

Build the bundle locally (Linux or WSL, needs `zig` and `cargo-zigbuild`):

1. `cd server && cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.31`
2. Same in a TonePush `v0.9.0` checkout: `cargo zigbuild --release -p tonepush-cli --target aarch64-unknown-linux-gnu.2.31`
3. `deploy/package.sh <version> server/target/aarch64-unknown-linux-gnu/release/helix-bridge <tonepush>/target/aarch64-unknown-linux-gnu/release/tonepush <tonepush> dist`
