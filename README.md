# Helix Bridge

Edit a Line 6 HX Effects from an iPad (or any browser) over Wi-Fi. A Raspberry Pi sits
between the pedal and your network: it talks to the pedal over USB and serves a
touch-friendly web UI.

```
[Line 6 HX device] --USB--> [Raspberry Pi] --Wi-Fi/HTTP--> [iPad Safari]
```

iPadOS cannot talk to the pedal directly (it uses a vendor USB interface), so all USB
work happens on the Pi. Built on [TonePush](https://github.com/crmne/tonepush), an
open-source editor for HX devices.

Not affiliated with or endorsed by Line 6.

## What it does

- Shows the loaded preset's signal chain, Path A and Path B, with HX Edit's model
  names, categories and colours.
- Loads, renames and saves presets; switches snapshots; sets the tempo (tap or type).
- Turns blocks on and off and changes their parameters with sliders, menus and switches.
- Adds, removes and moves blocks, on either path, and moves the split and merge.
- Puts blocks on footswitches and parameters on the expression pedals.
- Follows the pedal live: footswitch presses, knob turns and preset changes show on the
  iPad within about a second.

## What you need

- **Line 6 HX Effects**, firmware 3.80. This is the only device it has been tested on;
  other HX-family devices may work but are untested.
- **Raspberry Pi 4** with 64-bit Raspberry Pi OS Lite, on the same network as the iPad.
- **HX Edit** installed on a computer, once: the Pi needs a copy of its data folder for
  model and parameter names. Those files are Line 6's; they are never part of this repo.
- An iPad with Safari, or any recent browser on the same network.

## Install

Full guide, about 45 minutes: [docs/deploy.md](docs/deploy.md). In short:

1. Flash 64-bit Raspberry Pi OS Lite and boot the Pi.
2. Copy the Pi bundle (from this repo's GitHub Actions or releases) to the Pi and unpack it.
3. Run `sudo ./install.sh --hostname helix-bridge`.
4. Copy HX Edit's data folder to the Pi (guide step 6) and plug in the pedal.
5. Open `http://helix-bridge.local` on the iPad. To make changes, set a write token
   (guide step 9).

Updates: `sudo helix-bridge-update` on the Pi (needs a read-only GitHub token, see
docs/deploy.md "Updating").

Close HX Edit while the bridge runs: only one program can hold the pedal's USB
connection. To use HX Edit or `tonepush` by hand, stop the service first
(`sudo systemctl stop helix-bridge`).

## Using the web UI

Changes go to the pedal's edit buffer, as on the pedal itself; **Save** stores them in
the preset. "Unsaved" in the preset bar means there are edits not saved yet. Changing
anything needs the write token: the UI asks for it on the first change and remembers it
in the browser. Without it, the UI is read-only and its write controls are dashed.

- **Load a preset:** ‹ › or Presets → pick → Load, or tap it in the preset list on the
  left. Asks first if there are unsaved edits. On wide screens (1280 px and up) the button
  at the top left shows or hides the list; on a tablet in landscape, turn it on in the
  settings.
- **Settings** (the gear; on tablets and phones, the connection dots): block names as
  Line 6 calls them or as the gear they are based on, and the preset list. Kept in this
  browser.
- **Own Wi-Fi for gigs** (Settings, on the Pi only): so nobody on a venue's Wi-Fi can
  see the bridge, the Pi makes its own password-protected Wi-Fi that only your iPad
  joins. **Off** (the default) never does; **Away** joins your saved Wi-Fi when it is in
  range and makes its own anywhere else; **Always** makes its own even at home. Saving
  needs the write token and applies the next time the Pi is switched on. See
  docs/deploy.md, "Own Wi-Fi for gigs".
- **Rename / Save:** buttons in the preset bar. Save asks first and overwrites the
  loaded preset.
- **Snapshot:** tap 1–4 in the preset bar. The active one is lit.
- **Tempo:** tap TAP on the beat; the BPM is sent once you pause for 1.5 s (amber until
  the pedal has it). Tap the BPM to type a value (40–240).
- **Block on/off:** select a block, tap Turn on / Turn off.
- **Parameter:** drag its slider (the value shows under your finger), pick a menu
  entry, or flip a switch. Tap the value to type an exact one in the shown units (`40`,
  `2.2 kHz`). Sliders snap to a grid (0.01 on 0..1 knobs, 1 dB on levels); type for
  finer values.
- **Add a block:** tap an empty slot on Path A or Path B. Pick a category, then a model;
  a type chip narrows the list, the filter searches names, types and the gear a model is
  based on. Mono or Stereo is picked in the footer; legacy models hide behind the Legacy
  toggle; Recent lists the last 8 models added. The block lands switched on, with HX
  Edit's default values. A taken slot is never replaced.
- **Footswitch for a new block:** the menu next to Add offers No footswitch (default),
  First free (as the pedal does) or FS1–FS6. Without a footswitch, a block does not show
  on the HX Effects home screen, only in Signal Flow view (MENU, then the Signal Flow
  switch).
- **Remove a block:** select it, tap Remove, confirm.
- **Move a block:** drag it onto an empty cell on either path (on a touch screen, hold it
  for a moment first), or select it, tap **Move**, then the cell.
- **Phones:** the chain fills the screen, one cell per row (Path B in a second column).
  Tap a block to open its settings full screen; Back returns to the chain.
- **Path B:** a preset with blocks on Path B shows two rows, with the split and merge
  where they attach. **+ Path B** (while it is empty) adds its first block, which then
  runs parallel to the whole line. Tap the split or merge dot, then ‹ Earlier / Later ›,
  to move it. Path B's blocks always stay between the split and merge.
- **Footswitch:** select a block; the Footswitch row under its name shows its switch
  (also `· FS3` above it in the chain) and puts it on None or FS1–FS6. A dot on a switch
  means another block is on it too.
- **Expression pedals:** a block tagged `EXP1`/`EXP2` has a setting on that pedal. Tap a
  setting's **EXP** tag, pick None / EXP 1 / EXP 2, then drag the **Heel** and **Toe**
  sliders or tap **Full range**. Footswitch, MIDI and snapshot controllers show as
  `CTL n`, read only.

## Settings

Set in `/etc/helix-bridge/helix-bridge.env` on the Pi, then
`sudo systemctl restart helix-bridge`.

| Variable | Default | Meaning |
|---|---|---|
| `HELIX_TOKEN` | unset | Shared token for changes. Unset or empty = read-only. Never commit it |
| `HELIX_BACKEND` | `fake` (Pi: `usb`) | `usb` = the pedal, one held USB connection, live updates (recommended); `cli` = the pedal via the `tonepush` program, one connection per call, no live updates; `fake` = recorded demo data |
| `HELIX_BIND` | `0.0.0.0:8080` (Pi: port 80) | Listen address. Keep it on your local network: there is no other protection than the token |
| `HELIX_ALLOWED_HOSTS` | unset | Rarely needed. Extra host names to answer, comma-separated. Work with no setup: IP addresses, `helix-bridge.local`, `helix-bridge`, the router's name for the Pi (e.g. `helix-bridge.fritz.box`) and `.lan`/`.home`/`.home.arpa`/`.internal` names. Any other name is refused (protects against DNS rebinding); a browser gets a page with a button to the right address |
| `HX_EDIT_RESOURCES` | unset | HX Edit's data folder, for model and parameter names |
| `HELIX_TONEPUSH` | `tonepush` | `cli` only: path to the `tonepush` program |
| `HELIX_TONEPUSH_TIMEOUT_SECS` | `30` | `cli` only: stop a `tonepush` call that runs longer |
| `RUST_LOG` | `info` | Log detail |

## Known limits

- **Live updates need the `usb` backend.** With `cli`, the UI reads the pedal when it
  opens, when the iPad returns to the page, and on "Refresh from device" (tap the
  connection dots). The active snapshot is not shown with `cli` either.
- **The bridge connects 20 s after it starts**, after the pedal is plugged in or powered
  up, and after a connection ends; the UI shows "waiting N s". Connecting sooner can
  leave the HX Effects without live updates for the whole session.
- **Names, colours and sliders need HX Edit's data** on the Pi. Without it, block
  colours are guessed from the model name, unknown models show grey `FX`, and parameter
  values are typed instead of dragged.
- **"Based on …" under a model** needs `vendor/based-on.json`, built from HX Edit's
  Pilot's Guide (docs/deploy.md step 6).
- **A lost live update** (rare, when knobs turn fast) is corrected by a check against
  the pedal about once a minute.
- **Not available:** changing an existing block's model, snapshot rename, impulse
  responses, device settings, restoring backups.

## Troubleshooting

- The UI says "no device": check the USB cable and that the pedal is on; the bridge
  reconnects by itself about 20 s after the pedal appears.
- Service log: `journalctl -u helix-bridge -f`. More in
  [docs/deploy.md](docs/deploy.md) "Troubleshooting".

Diagnostics, run on the Pi with the service stopped (`sudo systemctl stop helix-bridge`,
then wait 40 s); all but `probe-add` only read:

| Command | Prints |
|---|---|
| `helix-bridge watch [SECONDS]` | The pedal's notifications as you use it |
| `helix-bridge probe-unsaved` | Where the edit buffer differs from the stored preset |
| `helix-bridge probe-slot POSITION` | The preset's slot row and one raw slot |
| `helix-bridge probe-switches` | Each footswitch's raw reply and which blocks the bridge shows on it |
| `helix-bridge probe-switch-table` | Every stored preset's footswitch table in the loaded setlist |
| `helix-bridge probe-add POSITION MODEL` | Adds a block as the bridge does (changes the edit buffer) and every reply |

To check footswitch numbering on another HX model: put a block on a footswitch on the
pedal by hand, run `helix-bridge probe-switches`, and check the block is listed under
that switch.

## Documentation

- [docs/deploy.md](docs/deploy.md): install, update, troubleshoot and uninstall on the Pi.
- [docs/api.md](docs/api.md): the HTTP API behind the UI.
- [docs/development.md](docs/development.md): running the server locally, tests, builds,
  and what has been verified on hardware.
- [docs/hx-effects-notifications.md](docs/hx-effects-notifications.md) and
  [docs/hx-effects-models.md](docs/hx-effects-models.md): notes on the HX Effects.

## Credits

- [TonePush](https://github.com/crmne/tonepush) by Carmine Paolino (MIT), pinned to tag
  `v0.9.0`. A patched copy of its `hx-usb` crate is in `third_party/hx-usb`
  ([PATCHES.md](third_party/hx-usb/PATCHES.md)), with its license.
- [Motion](https://motion.dev) 13.4.6 (MIT), vendored in `web/lib/` with its license.
- HX Edit's data files and model names belong to Line 6 and are not included.
