#!/bin/sh
# Install or update Helix Bridge on a Raspberry Pi (64-bit Raspberry Pi OS).
# Run from the unpacked release folder:
#
#   sudo ./install.sh                         install or update
#   sudo ./install.sh --hostname helix-bridge also rename the Pi, for http://helix-bridge.local
#   sudo ./install.sh --uninstall             remove (keeps settings and vendor/)
#
# Never compiles anything (AGENTS.md 5.1). Safe to re-run.
set -eu

PREFIX=/opt/helix-bridge
CONF_DIR=/etc/helix-bridge
SVC_USER=helix-bridge
UNIT=/etc/systemd/system/helix-bridge.service
HOTSPOT_UNIT=/etc/systemd/system/helix-bridge-hotspot.service
RULES=/etc/udev/rules.d/70-helix-bridge.rules
AVAHI=/etc/avahi/services/helix-bridge.service

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

new_hostname=""
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --hostname)
            [ $# -ge 2 ] || die "--hostname needs a name"
            new_hostname=$2
            shift 2
            ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) usage 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; usage 2 ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "run with sudo"

if [ "$uninstall" -eq 1 ]; then
    say "Stopping and removing helix-bridge"
    systemctl disable --now helix-bridge 2>/dev/null || true
    # Not stopped here: that could cut this very session. Ours has autoconnect
    # off, so the Pi is back on its saved Wi-Fi after the next reboot.
    systemctl disable helix-bridge-hotspot 2>/dev/null || true
    rm -f "$UNIT" "$HOTSPOT_UNIT" "$RULES" "$AVAHI"
    systemctl daemon-reload
    udevadm control --reload-rules || true
    systemctl reload avahi-daemon 2>/dev/null || true
    rm -rf "${PREFIX:?}/bin" "${PREFIX:?}/fixtures" "${PREFIX:?}/licenses" "${PREFIX:?}/BUILD_INFO"
    rm -f /usr/local/sbin/helix-bridge-update
    say "Removed. Kept: $CONF_DIR, $PREFIX/vendor, /var/lib/helix-bridge, user $SVC_USER"
    say "If the Pi's own Wi-Fi is on, it stops at the next reboot"
    exit 0
fi

arch=$(uname -m)
[ "$arch" = aarch64 ] || die "needs 64-bit Raspberry Pi OS (uname -m says $arch, expected aarch64)"

here=$(cd "$(dirname "$0")" && pwd)
for f in bin/helix-bridge bin/tonepush deploy/helix-bridge.service deploy/helix-bridge.env \
    deploy/70-helix-bridge.rules deploy/avahi-helix-bridge.service \
    deploy/hotspot.sh deploy/helix-bridge-hotspot.service; do
    [ -e "$here/$f" ] || die "missing $f: run this from the unpacked release folder"
done

if [ -n "$new_hostname" ]; then
    case "$new_hostname" in
        *[!a-z0-9-]*|-*|*-) die "hostname must be lowercase letters, digits and inner dashes" ;;
    esac
fi

# 1. Service user. Owns nothing but its state dir; USB access comes from the udev rule.
if ! id "$SVC_USER" >/dev/null 2>&1; then
    say "Creating system user $SVC_USER"
    useradd --system --user-group --home-dir /var/lib/helix-bridge --no-create-home \
        --shell /usr/sbin/nologin "$SVC_USER"
fi

# 2. Binaries and fixtures.
say "Installing to $PREFIX"
install -d -m 755 "$PREFIX" "$PREFIX/bin" "$PREFIX/vendor"
install -m 755 "$here/bin/helix-bridge" "$here/bin/tonepush" "$PREFIX/bin/"
rm -rf "${PREFIX:?}/fixtures" "${PREFIX:?}/licenses"
if [ -d "$here/fixtures" ]; then
    cp -R "$here/fixtures" "$PREFIX/fixtures"
fi
if [ -d "$here/licenses" ]; then
    cp -R "$here/licenses" "$PREFIX/licenses"
fi
if [ -e "$here/BUILD_INFO" ]; then
    install -m 644 "$here/BUILD_INFO" "$PREFIX/BUILD_INFO"
fi
if [ -e "$here/deploy/update.sh" ]; then
    install -m 755 "$here/deploy/update.sh" /usr/local/sbin/helix-bridge-update
fi
install -m 755 "$here/deploy/hotspot.sh" "$PREFIX/bin/helix-bridge-hotspot"

# 3. Settings: installed once, never overwritten.
install -d -m 755 "$CONF_DIR"
if [ -e "$CONF_DIR/helix-bridge.env" ]; then
    say "Keeping existing $CONF_DIR/helix-bridge.env"
else
    install -m 600 "$here/deploy/helix-bridge.env" "$CONF_DIR/helix-bridge.env"
fi
# May hold HELIX_TOKEN. systemd reads it as root, so nobody else needs to.
chmod 600 "$CONF_DIR/helix-bridge.env"

# 4. USB permissions. "change" re-applies the rule to a pedal that is already
# plugged in; it does not reset or re-enumerate the device.
say "Installing udev rule"
install -m 644 "$here/deploy/70-helix-bridge.rules" "$RULES"
udevadm control --reload-rules
udevadm trigger --action=change --subsystem-match=usb --attr-match=idVendor=0e41 || true

# 5. mDNS. Raspberry Pi OS Lite ships avahi-daemon; install it if it is missing.
if ! command -v avahi-daemon >/dev/null 2>&1; then
    say "Installing avahi-daemon"
    apt-get update -q && apt-get install -y -q avahi-daemon
fi
install -d -m 755 /etc/avahi/services
install -m 644 "$here/deploy/avahi-helix-bridge.service" "$AVAHI"

if [ -n "$new_hostname" ] && [ "$(hostname)" != "$new_hostname" ]; then
    say "Renaming this Pi to $new_hostname"
    hostnamectl set-hostname "$new_hostname"
    if grep -q '^127\.0\.1\.1' /etc/hosts; then
        sed -i "s/^127\.0\.1\.1.*/127.0.1.1\t$new_hostname/" /etc/hosts
    else
        printf '127.0.1.1\t%s\n' "$new_hostname" >> /etc/hosts
    fi
fi
systemctl enable avahi-daemon >/dev/null 2>&1 || true
systemctl restart avahi-daemon

# 6. Service.
say "Starting helix-bridge"
install -m 644 "$here/deploy/helix-bridge.service" "$UNIT"
systemctl daemon-reload
systemctl enable helix-bridge >/dev/null 2>&1
systemctl restart helix-bridge
# Own Wi-Fi: takes effect at the next reboot, never now, as switching Wi-Fi
# would cut the SSH session (or update) this runs in.
install -m 644 "$here/deploy/helix-bridge-hotspot.service" "$HOTSPOT_UNIT"
systemctl daemon-reload
systemctl enable helix-bridge-hotspot >/dev/null 2>&1

# 7. Check. /api/health does not touch the device.
port=$(sed -n 's/^HELIX_BIND=.*:\([0-9]*\)$/\1/p' "$CONF_DIR/helix-bridge.env" | tail -n 1)
port=${port:-80}
ok=0
for _ in 1 2 3 4 5; do
    sleep 1
    if command -v curl >/dev/null 2>&1; then
        curl -fsS "http://127.0.0.1:$port/api/health" >/dev/null 2>&1 && ok=1 && break
    else
        systemctl is-active --quiet helix-bridge && ok=1 && break
    fi
done
if [ "$ok" -ne 1 ]; then
    systemctl --no-pager status helix-bridge || true
    die "helix-bridge did not come up. Logs: journalctl -u helix-bridge -n 50"
fi

[ "$port" = 80 ] && suffix="" || suffix=":$port"
say "Done. Open http://$(hostname).local$suffix on the iPad"
if [ -d "$PREFIX/vendor/res" ]; then
    # scp from Windows creates it as drwx------; the service user must read it.
    chmod -R a+rX "$PREFIX/vendor/res"
else
    say "Note: $PREFIX/vendor/res is missing, so no model names. See docs/deploy.md step 6."
fi
