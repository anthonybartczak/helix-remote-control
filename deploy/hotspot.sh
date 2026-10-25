#!/bin/sh
# The Pi's own Wi-Fi, so at a gig the iPad talks to the Pi directly and nobody
# on the venue's network can reach the bridge. Set in the app (Settings, Own
# Wi-Fi for gigs), which saves /var/lib/helix-bridge/hotspot.conf; run at
# start-up by helix-bridge-hotspot.service. By hand (cuts an SSH session that
# runs over Wi-Fi): sudo systemctl restart helix-bridge-hotspot
#
#   HOTSPOT=off   leave Wi-Fi to the saved networks; stop ours if it runs (default)
#   HOTSPOT=auto  a saved Wi-Fi if one connects within HELIX_HOTSPOT_WAIT_SECS (45), else ours
#   HOTSPOT=on    always make our own Wi-Fi
#
# The file is written by the unprivileged server and this runs as root, so it
# is only ever read as data, never sourced. Needs NetworkManager (Raspberry
# Pi OS Bookworm or newer). Never changes the saved networks: ours is one more
# connection, with autoconnect off, so without this service the Pi is back on
# its saved Wi-Fi after a reboot.
set -eu

CON=helix-bridge-hotspot
file=${HELIX_HOTSPOT_FILE:-/var/lib/helix-bridge/hotspot.conf}
wait_secs=${HELIX_HOTSPOT_WAIT_SECS:-45}

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# A link could point this root script at any file; the app writes a plain one.
[ ! -L "$file" ] || die "$file is a symbolic link: not reading it"
setting() { # KEY [default]: the last KEY= line of the file, as text
    value=""
    if [ -r "$file" ]; then
        value=$(sed -n "s/^$1=//p" "$file" | tail -n 1)
    fi
    printf '%s' "${value:-${2:-}}"
}
mode=$(setting HOTSPOT off)
name=$(setting NAME "Helix Bridge")
password=$(setting PASSWORD)

case $mode in
    off|on|auto) ;;
    *) die "HOTSPOT must be off, on or auto, got \"$mode\" ($file)" ;;
esac
command -v nmcli >/dev/null 2>&1 ||
    die "needs NetworkManager (Raspberry Pi OS Bookworm or newer)"

# The first Wi-Fi device: wlan0 on a Pi.
dev=$(nmcli -t -f DEVICE,TYPE device | awk -F: '$2 == "wifi" { print $1; exit }')
[ -n "$dev" ] || die "no Wi-Fi device found"

active_on_dev() { # the connection $dev is on, empty if none
    nmcli -t -f DEVICE,STATE,CONNECTION device |
        awk -F: -v d="$dev" '$1 == d && $2 == "connected" { print $3 }'
}

take_down() {
    if nmcli -t -f NAME connection show --active | grep -qx "$CON"; then
        say "Stopping the Pi's own Wi-Fi"
        nmcli connection down "$CON" >/dev/null
    fi
    nmcli connection delete "$CON" >/dev/null 2>&1 || true
}

bring_up() {
    # Checked before touching the network, so a bad setting leaves the Pi on
    # its saved Wi-Fi (and reachable).
    [ "${#password}" -ge 8 ] ||
        die "the password needs 8 or more characters: not making our own Wi-Fi"
    [ "${#password}" -le 63 ] || die "the password is longer than 63 characters"
    if [ -z "$name" ] || [ "${#name}" -gt 32 ]; then
        die "the Wi-Fi name must be 1 to 32 characters"
    fi
    # Made afresh each time, so a new name or password applies. WPA2 with
    # CCMP only and no PMF: what iPads join reliably from a Pi's Wi-Fi chip.
    # (The password is on nmcli's command line for a moment; only local users
    # could see it, and the Pi has none but you.)
    nmcli connection delete "$CON" >/dev/null 2>&1 || true
    nmcli connection add type wifi ifname "$dev" con-name "$CON" autoconnect no \
        ssid "$name" 802-11-wireless.mode ap 802-11-wireless.band bg \
        ipv4.method shared \
        wifi-sec.key-mgmt wpa-psk wifi-sec.psk "$password" \
        wifi-sec.proto rsn wifi-sec.pairwise ccmp wifi-sec.group ccmp \
        wifi-sec.pmf disable >/dev/null
    if ! nmcli connection up "$CON" >/dev/null; then
        # Gone again, so NetworkManager goes back to the saved Wi-Fi.
        nmcli connection delete "$CON" >/dev/null 2>&1 || true
        die "could not start our own Wi-Fi: journalctl -u NetworkManager -n 50"
    fi
    say "Own Wi-Fi \"$name\" is on. Join it on the iPad, then open http://$(hostname).local"
}

wait_for_saved() { # true once $dev is on a saved network
    i=0
    while [ "$i" -lt "$wait_secs" ]; do
        on=$(active_on_dev)
        if [ -n "$on" ] && [ "$on" != "$CON" ]; then
            say "On saved Wi-Fi \"$on\": not making our own"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

case $mode in
    off) take_down ;;
    on) bring_up ;;
    auto)
        # Ours could be up from an earlier run: give the saved ones a chance.
        take_down
        if ! wait_for_saved; then
            say "No saved Wi-Fi after $wait_secs s: making our own"
            bring_up
        fi
        ;;
esac
