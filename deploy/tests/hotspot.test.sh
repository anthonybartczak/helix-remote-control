#!/bin/sh
# Checks deploy/hotspot.sh against a fake nmcli: what it asks NetworkManager to
# do for each setting the app can save, and that a bad one touches nothing.
# Runs anywhere:
#
#   sh deploy/tests/hotspot.test.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
script="$here/../hotspot.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
log="$work/nmcli.log"
conf="$work/hotspot.conf"

# Fake nmcli: wlan0 is on $FAKE_ON (none if empty); $FAKE_ACTIVE is the
# active connection list. Every call is logged.
mkdir "$work/bin"
cat > "$work/bin/nmcli" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
case "$*" in
    "-t -f DEVICE,TYPE device") printf 'wlan0:wifi\neth0:ethernet\nlo:loopback\n' ;;
    "-t -f DEVICE,STATE,CONNECTION device")
        if [ -n "${FAKE_ON:-}" ]; then echo "wlan0:connected:$FAKE_ON"; else echo "wlan0:disconnected:"; fi
        echo "eth0:unavailable:" ;;
    "-t -f NAME connection show --active") printf '%s\n' "${FAKE_ACTIVE:-lo}" ;;
    "connection up"*) [ -z "${FAKE_UP_FAILS:-}" ] || exit 4 ;;
esac
EOF
printf '#!/bin/sh\n' > "$work/bin/sleep"
printf '#!/bin/sh\necho helix-bridge\n' > "$work/bin/hostname"
chmod +x "$work/bin/nmcli" "$work/bin/sleep" "$work/bin/hostname"

failures=0
fail() { printf 'FAIL: %s\n' "$*"; failures=$((failures + 1)); }

# run NAME EXPECTED_EXIT FILE [VAR=value ...]: run the script with FILE as the
# app's saved setting ("-" for none) and these fakes.
run() {
    name=$1 want=$2 settings=$3
    shift 3
    : > "$log"
    rm -f "$conf"
    [ "$settings" = - ] || printf '%s\n' "$settings" > "$conf"
    got=0
    env -i PATH="$work/bin:$PATH" FAKE_LOG="$log" HELIX_HOTSPOT_FILE="$conf" \
        HELIX_HOTSPOT_WAIT_SECS=3 "$@" sh "$script" > "$work/out" 2>&1 || got=$?
    if [ "$got" -ne "$want" ]; then
        fail "$name: exit $got, expected $want"
        sed 's/^/    /' "$work/out"
    fi
}
logged() { grep -q -- "$1" "$log" || fail "$name: nmcli never got: $1"; }
not_logged() { ! grep -q -- "$1" "$log" || fail "$name: nmcli got: $1"; }
said() { grep -q -- "$1" "$work/out" || fail "$name: output lacks: $1"; }

# As the server writes it (server/src/hotspot.rs).
saved() { # MODE PASSWORD [NAME]
    printf '# Written by Helix Bridge\nHOTSPOT=%s\nNAME=%s\nPASSWORD=%s' "$1" "${3:-Helix Bridge}" "$2"
}

run "nothing saved yet is off" 0 -
logged "connection delete helix-bridge-hotspot"
not_logged "connection add"
not_logged "connection down"

run "off" 0 "$(saved off gig-secret-1)"
not_logged "connection add"

run "off while ours runs" 0 "$(saved off gig-secret-1)" FAKE_ACTIVE=helix-bridge-hotspot
logged "connection down helix-bridge-hotspot"

run "on" 0 "$(saved on gig-secret-1)" FAKE_ON=HomeNet
logged "connection add type wifi ifname wlan0 con-name helix-bridge-hotspot autoconnect no ssid Helix Bridge 802-11-wireless.mode ap"
logged "wifi-sec.psk gig-secret-1"
logged "connection up helix-bridge-hotspot"
said "Join it on the iPad, then open http://helix-bridge.local"

# A literal `$(x)`: the password must reach nmcli as text, never run.
# shellcheck disable=SC2016
run "on, own name and odd characters" 0 "$(saved on ' pa=ss $(x) ' "Anna's rig")"
logged "ssid Anna's rig 802-11"
logged "wifi-sec.psk  pa=ss \$(x)  wifi-sec.proto"

run "on, short password" 1 "$(saved on short)" FAKE_ON=HomeNet
said "8 or more characters"
not_logged "connection add"
not_logged "connection delete"

run "on, no password" 1 "$(saved on '')"
not_logged "connection add"

run "on, name too long" 1 "$(saved on 12345678 123456789012345678901234567890123)"
not_logged "connection add"

run "on, NetworkManager refuses" 1 "$(saved on 12345678)" FAKE_UP_FAILS=1
said "could not start our own Wi-Fi"
[ "$(grep -c "connection delete helix-bridge-hotspot" "$log")" -eq 2 ] ||
    fail "$name: ours not deleted after the failed start"

run "auto at home" 0 "$(saved auto 12345678)" FAKE_ON=HomeNet
said 'On saved Wi-Fi "HomeNet"'
not_logged "connection add"

run "auto at a gig" 0 "$(saved auto 12345678)"
said "No saved Wi-Fi after 3 s"
logged "connection up helix-bridge-hotspot"

run "auto, ours left up" 0 "$(saved auto 12345678)" \
    FAKE_ON=helix-bridge-hotspot FAKE_ACTIVE=helix-bridge-hotspot
logged "connection down helix-bridge-hotspot"
logged "connection up helix-bridge-hotspot"

run "unknown mode" 1 "$(saved yes 12345678)"
said "must be off, on or auto"
[ ! -s "$log" ] || fail "$name: nmcli was called"

# Root reads a file the service user writes: never follow a link. (Skipped
# where `ln -s` copies instead, as Git Bash on Windows does.)
rm -f "$conf"
saved on 12345678 > "$work/target"
ln -s "$work/target" "$conf" 2>/dev/null || true
if [ -L "$conf" ]; then
    name="symbolic link"
    : > "$log"
    got=0
    env -i PATH="$work/bin:$PATH" FAKE_LOG="$log" HELIX_HOTSPOT_FILE="$conf" \
        sh "$script" > "$work/out" 2>&1 || got=$?
    [ "$got" -eq 1 ] || fail "$name: exit $got, expected 1"
    said "symbolic link"
    [ ! -s "$log" ] || fail "$name: nmcli was called"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "hotspot.sh: all checks passed"
