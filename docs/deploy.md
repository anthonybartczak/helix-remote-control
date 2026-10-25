# Deploy to a Raspberry Pi

Runs Helix Bridge as a systemd service on 64-bit Raspberry Pi OS Lite, reachable from
the iPad at `http://helix-bridge.local`. The Pi never compiles: GitHub Actions builds
the bundle (`.github/workflows/release.yml`).

Total time: about 45 minutes, most of it flashing the SD card and the first boot.

## What gets installed

| Path | What |
|---|---|
| `/opt/helix-bridge/bin/` | `helix-bridge` (server + web UI) and `tonepush` (TonePush v0.9.0 CLI) |
| `/opt/helix-bridge/fixtures/` | Recorded output, for `HELIX_BACKEND=fake` |
| `/opt/helix-bridge/vendor/res/` | HX Edit data, copied by you (step 6). Never committed |
| `/etc/helix-bridge/helix-bridge.env` | Settings, incl. the write token. Mode `600`. Kept on update |
| `/etc/systemd/system/helix-bridge.service` | Service, runs as user `helix-bridge` on port 80 |
| `/etc/systemd/system/helix-bridge-hotspot.service` | Own Wi-Fi for gigs, as set in the app (`/var/lib/helix-bridge/hotspot.conf`): runs `/opt/helix-bridge/bin/helix-bridge-hotspot` at start-up |
| `/etc/udev/rules.d/70-helix-bridge.rules` | USB access to vendor `0e41` for that user |
| `/etc/avahi/services/helix-bridge.service` | mDNS advert for the web UI |

## Steps

1. **Flash the SD card (about 15 minutes).** Raspberry Pi Imager → Raspberry Pi OS Lite
   **(64-bit)**. In the Imager settings: hostname `helix-bridge`, enable SSH, your user
   and password, your Wi-Fi. Boot the Pi and wait about 2 minutes.
2. **Get the bundle (about 2 minutes).** On the dev machine, from the repo's GitHub
   **Releases** page (or a workflow run's `pi-bundle` artifact), download
   `helix-bridge-<version>-aarch64-linux.tar.gz`. The repo is private, so download in the
   browser while logged in.
3. **Copy it to the Pi.** PowerShell on the dev machine:
   `scp .\helix-bridge-*-aarch64-linux.tar.gz <user>@helix-bridge.local:`
4. **Install (about 1 minute).** On the Pi (`ssh <user>@helix-bridge.local`):
   ```sh
   tar xzf helix-bridge-*-aarch64-linux.tar.gz
   cd helix-bridge-*-aarch64-linux
   sudo ./install.sh --hostname helix-bridge
   ```
   It ends with `Done. Open http://helix-bridge.local on the iPad`.
5. **Check without the pedal.** Before plugging the device in, try the fake backend:
   ```sh
   sudo sed -i 's/^HELIX_BACKEND=.*/HELIX_BACKEND=fake/' /etc/helix-bridge/helix-bridge.env
   sudo systemctl restart helix-bridge
   ```
   Open `http://helix-bridge.local` on the iPad: the recorded presets appear. Switch back
   with `HELIX_BACKEND=cli` and restart.
6. **Copy HX Edit data (about 5 minutes).** Needed for model and parameter names. Never
   commit these files (AGENTS.md 5.4). PowerShell on the dev machine:
   ```powershell
   scp -r 'C:\Program Files (x86)\Line6\HX Edit\res' <user>@helix-bridge.local:hx-res
   ```
   Then on the Pi:
   ```sh
   sudo rm -rf /opt/helix-bridge/vendor/res
   sudo mv ~/hx-res /opt/helix-bridge/vendor/res
   sudo chown -R root:root /opt/helix-bridge/vendor/res
   sudo chmod -R a+rX /opt/helix-bridge/vendor/res
   ```
   The `chmod` matters: `scp` from Windows creates the folder as `drwx------`, so the
   service user cannot read it and names stay `model 269`.

   Optional, for "Based on MXR Dyna Comp" under each model (about 3 minutes): build the
   table from HX Edit's Pilot's Guide on the dev machine, from the repository root. It is
   Line 6's table, so it stays in the gitignored `vendor/`:
   ```powershell
   cargo run --release --manifest-path tools/based-on/Cargo.toml
   scp vendor/based-on.json <user>@helix-bridge.local:
   ```
   Then on the Pi, and restart the service:
   ```sh
   sudo install -m 644 ~/based-on.json /opt/helix-bridge/vendor/based-on.json
   sudo systemctl restart helix-bridge
   ```
   The log says `based-on table loaded … models=261`.
7. **Plug in the pedal.** Quit HX Edit on every machine first (AGENTS.md 5.5). Connect
   the HX Effects to the Pi over USB, then:
   ```sh
   lsusb | grep -i 0e41               # expect 0e41:4245 Line 6
   ls -l /dev/bus/usb/*/* | grep helix-bridge   # the udev rule applied
   curl -s http://localhost/api/device
   ```
8. **Open `http://helix-bridge.local` on the iPad.** Add it to the Home Screen
   (Share → Add to Home Screen) for a full-screen app.
9. **Turn on writes (about 2 minutes, optional).** Without a token the UI is read-only.
   Only after a full preset backup from HX Edit (AGENTS.md 6). On the Pi:
   ```sh
   echo "HELIX_TOKEN=$(openssl rand -hex 16)" | sudo tee -a /etc/helix-bridge/helix-bridge.env
   sudo chmod 600 /etc/helix-bridge/helix-bridge.env
   sudo systemctl restart helix-bridge
   sudo grep HELIX_TOKEN /etc/helix-bridge/helix-bridge.env   # type this on the iPad
   ```
   The iPad asks for the token on the first write and keeps it in Safari. The log line
   `helix-bridge listening … writes="on"` confirms it (`journalctl -u helix-bridge -n 5`).
10. **Live sync (about 2 minutes, installs before 2026-10-13 only).** New installs use
    the `usb` backend already: one held USB session, pedal changes appear on the iPad
    within about 1 s. An older settings file still says `cli`; switch it:
    ```sh
    sudo sed -i 's/^HELIX_BACKEND=.*/HELIX_BACKEND=usb/' /etc/helix-bridge/helix-bridge.env
    sudo systemctl restart helix-bridge
    sudo journalctl -u helix-bridge -n 5 --no-pager   # expect "USB session open"
    ```
    Back to the old behaviour: same with `HELIX_BACKEND=cli`.

## Own Wi-Fi for gigs (about 5 minutes, optional)

On a venue's Wi-Fi, anyone else on it can open the bridge and watch your setup. With
its own Wi-Fi the Pi makes a password-protected network that only your iPad joins.
It is set in the app and is off until you turn it on. **Away** is the easy choice: at
home the Pi joins your Wi-Fi as before (for updates); anywhere it finds no saved Wi-Fi
it makes its own, about 1 minute after power-on. Needs the write token (step 9).

1. **Open Settings** in the app: the gear, or the connection dots on a tablet or phone.
2. **Under Own Wi-Fi for gigs, tap Away.** Keep the name or change it, and type a
   password of 8 or more characters.
3. **Tap Save.** Nothing changes now: switching would cut off this iPad.
4. **Try it once at home (about 2 minutes):** switch your Wi-Fi router off (or take
   the Pi out of its range), power the Pi off and on, and wait 1 minute.
5. **Join it on the iPad:** Settings → Wi-Fi → **Helix Bridge**, type the password once.
   iOS says "No Internet Connection": that is expected, stay on it. Then open
   `http://helix-bridge.local` (or `http://10.42.0.1`).

- **Always** makes its own Wi-Fi even at home; **Off** (the default) never does.
- Do not save the venue's Wi-Fi on the Pi: with Away it would join that instead.
- The app saves it to `/var/lib/helix-bridge/hotspot.conf` (`HOTSPOT=off|auto|on`,
  `NAME=`, `PASSWORD=`); `helix-bridge-hotspot.service` reads it at start-up, as root,
  as data only. What it did: `journalctl -u helix-bridge-hotspot -b`.

## Updating

One command on the Pi, once set up: `sudo helix-bridge-update`. It takes the newest
**green** build of your branch from GitHub Actions, checks its SHA-256, runs
`install.sh` (settings and `vendor/` are kept) and restarts the service.

| Command | Does |
|---|---|
| `sudo helix-bridge-update --check` | Show installed vs newest build, change nothing |
| `sudo helix-bridge-update` | Install the newest build if it is newer |
| `sudo helix-bridge-update --rollback` | Reinstall the previous bundle (last 3 kept in `/var/cache/helix-bridge-update`) |
| `sudo helix-bridge-update --branch NAME` | Follow another branch from now on |
| `sudo helix-bridge-update --force` | Reinstall even if current |

Builds are kept by GitHub for 90 days; older ones cannot be downloaded.

### One-time setup (about 10 minutes)

1. **Install a bundle that has the updater** (one that contains `deploy/update.sh`), the manual
   way: steps 2–4 above. It installs `/usr/local/sbin/helix-bridge-update`.
2. **Make a read-only GitHub token.** github.com → Settings → Developer settings →
   Personal access tokens → **Fine-grained tokens** → Generate new token:
   - Repository access: **Only select repositories** → `helix-remote-control`
   - Permissions → Repository → **Actions: Read-only** (Metadata read-only is added
     automatically). Nothing else.
   - Expiration: your choice; the updater says when it has expired.
3. **Store it on the Pi** (never in git, AGENTS.md 6):
   ```sh
   sudo sh -c 'umask 077; cat > /etc/helix-bridge/github-token'
   ```
   Paste the token, press Enter, then Ctrl+D.
4. **Check:** `sudo helix-bridge-update --check`. It names the installed build and the
   newest one. The branch comes from the installed build; change it with `--branch`.

Manual way still works: repeat steps 2–4 with the new tarball.

## Running `tonepush` by hand on the Pi

The device belongs to group `helix-bridge`. Add yourself once, then log out and in:
`sudo usermod -aG helix-bridge $USER`. Stop the service first so the two don't fight
over the USB session: `sudo systemctl stop helix-bridge`.

```sh
HX_EDIT_RESOURCES=/opt/helix-bridge/vendor/res /opt/helix-bridge/bin/tonepush info
```

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `helix-bridge.local` does not resolve | Hostname not set or Avahi down. Check `hostname` and `systemctl status avahi-daemon`. Fallback: `http://<pi-ip>` (`hostname -I`) |
| "Helix Bridge has another address" | The page was opened by a name the bridge does not answer to, to keep websites away from the pedal. Tap the button on that page and bookmark where it leads. (Experts: `HELIX_ALLOWED_HOSTS` in the settings file adds names) |
| UI loads, device calls fail with a permission error | udev rule not applied. Run `sudo udevadm trigger --action=change --subsystem-match=usb`, then check step 7 |
| Device calls time out | HX Edit is open somewhere, or the pedal is wedged. Close HX Edit. If still stuck, pull the pedal's **power adapter** (a USB replug does not help, PROTOCOL.md) |
| No model names, `FX` everywhere | `vendor/res` missing, wrong folder, or unreadable. `ls -ld /opt/helix-bridge/vendor/res` must show `drwxr-xr-x`; fix with `sudo chmod -R a+rX /opt/helix-bridge/vendor/res`, or re-run `install.sh` |
| Service does not start | `journalctl -u helix-bridge -n 50` |
| `usb` backend: "cannot open the pedal" in the log | HX Edit or a hand-run `tonepush` holds the interface. Close it; the server retries by itself (5 s, backing off to 60 s) |
| `helix-bridge-update`: "GitHub API request failed" | Token expired, mistyped, or lacks Actions: read. Make a new one (Updating, step 2) |
| Write buttons dashed, "Writes are off" | No `HELIX_TOKEN` in the settings file. See step 9 |
| iPad keeps asking for the token | Token typed wrong, or changed on the Pi. Re-type it; the UI asks again after each refusal |
| No "Helix Bridge" Wi-Fi at the gig | A saved Wi-Fi was in range (Away joined it), or the setting is Off. `journalctl -u helix-bridge-hotspot -b` says which |
| Pi on its own Wi-Fi at home, needed on your Wi-Fi | Always keeps it there. Join "Helix Bridge" on the iPad, open the app, set Away or Off in Settings, then switch the Pi off and on |

Memory: `systemctl status helix-bridge` shows the service's RAM, `free -h` the whole Pi.
The unit caps the service at 180 MB (`MemoryMax`).

## Uninstall

`sudo ./install.sh --uninstall` from the bundle folder. Keeps `/etc/helix-bridge`,
`/opt/helix-bridge/vendor` and `/var/lib/helix-bridge`.
