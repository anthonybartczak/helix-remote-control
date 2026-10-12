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

## Updating

Repeat steps 2–4 with the new tarball. Settings and `vendor/` are kept.

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
| UI loads, device calls fail with a permission error | udev rule not applied. Run `sudo udevadm trigger --action=change --subsystem-match=usb`, then check step 7 |
| Device calls time out | HX Edit is open somewhere, or the pedal is wedged. Close HX Edit. If still stuck, pull the pedal's **power adapter** (a USB replug does not help, PROTOCOL.md) |
| No model names, `FX` everywhere | `vendor/res` missing, wrong folder, or unreadable. `ls -ld /opt/helix-bridge/vendor/res` must show `drwxr-xr-x`; fix with `sudo chmod -R a+rX /opt/helix-bridge/vendor/res`, or re-run `install.sh` |
| Service does not start | `journalctl -u helix-bridge -n 50` |
| Write buttons dashed, "Writes are off" | No `HELIX_TOKEN` in the settings file. See step 9 |
| iPad keeps asking for the token | Token typed wrong, or changed on the Pi. Re-type it; the UI asks again after each refusal |

Memory: `systemctl status helix-bridge` shows the service's RAM, `free -h` the whole Pi.
The unit caps the service at 180 MB (`MemoryMax`).

## Uninstall

`sudo ./install.sh --uninstall` from the bundle folder. Keeps `/etc/helix-bridge`,
`/opt/helix-bridge/vendor` and `/var/lib/helix-bridge`.
