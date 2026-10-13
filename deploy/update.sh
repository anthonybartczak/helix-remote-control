#!/bin/sh
# Update Helix Bridge on the Pi to the newest green build from GitHub Actions.
# Installed as /usr/local/sbin/helix-bridge-update by install.sh.
#
#   sudo helix-bridge-update                 install the newest build, if newer
#   sudo helix-bridge-update --check         only show installed vs available
#   sudo helix-bridge-update --force         reinstall even if already current
#   sudo helix-bridge-update --rollback      reinstall the previous bundle
#   sudo helix-bridge-update --branch NAME   follow another branch (remembered)
#   sudo helix-bridge-update --download-only fetch and verify, do not install
#
# Needs a read-only GitHub token in /etc/helix-bridge/github-token (the repo is
# private). Setup: docs/deploy.md, "Updating". Never compiles (AGENTS.md 5.1).
set -eu

CONF_DIR=/etc/helix-bridge
TOKEN_FILE=$CONF_DIR/github-token
CONF=$CONF_DIR/update.conf
PREFIX=/opt/helix-bridge
CACHE=/var/cache/helix-bridge-update
KEEP=3
API=${HELIX_UPDATE_API:-https://api.github.com}
REPO=anthonybartczak/helix-remote-control
BRANCH=""

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

# update.conf holds REPO= and BRANCH= lines; written by --branch.
if [ -r "$CONF" ]; then
    REPO=$(sed -n 's/^REPO=//p' "$CONF" | tail -n 1)
    BRANCH=$(sed -n 's/^BRANCH=//p' "$CONF" | tail -n 1)
    REPO=${REPO:-anthonybartczak/helix-remote-control}
fi

mode=update force=0 new_branch=""
while [ $# -gt 0 ]; do
    case "$1" in
        --check) mode=check; shift ;;
        --force) force=1; shift ;;
        --rollback) mode=rollback; shift ;;
        --download-only) mode=download; shift ;;
        --branch)
            [ $# -ge 2 ] || die "--branch needs a name"
            new_branch=$2
            shift 2
            ;;
        -h|--help) usage 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; usage 2 ;;
    esac
done

[ "$(id -u)" -eq 0 ] || [ -n "${HELIX_UPDATE_API:-}" ] || die "run with sudo"
for tool in curl python3 sha256sum tar; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is missing: sudo apt install $tool"
done

info_field() { # BUILD_INFO key
    [ -r "$PREFIX/BUILD_INFO" ] && sed -n "s/^$1=//p" "$PREFIX/BUILD_INFO" | tail -n 1 || true
}

installed_version=$(info_field version)
installed_commit=$(info_field commit)

install_bundle() { # path/to/helix-bridge-*.tar.gz
    work=$(mktemp -d)
    tar -C "$work" -xzf "$1"
    dir=$(find "$work" -mindepth 1 -maxdepth 1 -type d | head -n 1)
    [ -x "$dir/install.sh" ] || die "$1 has no install.sh"
    "$dir/install.sh"
    rm -rf "$work"
}

if [ "$mode" = rollback ]; then
    # Newest first; the first one is what is installed now.
    previous=$(ls -1t "$CACHE"/helix-bridge-*.tar.gz 2>/dev/null | sed -n 2p)
    [ -n "$previous" ] || die "no previous bundle in $CACHE (it keeps the last $KEEP updates)"
    say "Rolling back to $(basename "$previous" .tar.gz)"
    touch "$previous"
    install_bundle "$previous"
    exit 0
fi

if [ -n "$new_branch" ]; then
    BRANCH=$new_branch
    install -d -m 755 "$CONF_DIR"
    printf 'REPO=%s\nBRANCH=%s\n' "$REPO" "$BRANCH" > "$CONF"
    chmod 644 "$CONF"
fi
# Default: the branch the installed build came from.
[ -n "$BRANCH" ] || BRANCH=$(info_field ref)
[ -n "$BRANCH" ] || die "no branch known yet: run once with --branch <name>"

[ -r "$TOKEN_FILE" ] || die "no GitHub token: put one in $TOKEN_FILE (docs/deploy.md, Updating)"
token=$(tr -d ' \r\n' < "$TOKEN_FILE")
[ -n "$token" ] || die "$TOKEN_FILE is empty"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Headers from a file, so the token never shows in `ps`.
umask 077
printf 'Authorization: Bearer %s\nAccept: application/vnd.github+json\nX-GitHub-Api-Version: 2022-11-28\n' \
    "$token" > "$tmp/headers"
umask 022

api() { # path under /repos/REPO/
    curl -fsS -H @"$tmp/headers" "$API/repos/$REPO/$1" ||
        die "GitHub API request failed ($1). Token expired or missing Actions: read?"
}

json() { # python expression over `d`, reads JSON from stdin
    python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"
}

branch_q=$(python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$BRANCH")
api "actions/workflows/release.yml/runs?branch=$branch_q&status=success&event=push&per_page=1" > "$tmp/runs.json"
[ "$(json 'd["total_count"]' < "$tmp/runs.json")" != 0 ] || die "no green build on branch $BRANCH"
run_id=$(json 'd["workflow_runs"][0]["id"]' < "$tmp/runs.json")
commit=$(json 'd["workflow_runs"][0]["head_sha"]' < "$tmp/runs.json")
title=$(json 'd["workflow_runs"][0]["display_title"]' < "$tmp/runs.json")
built=$(json 'd["workflow_runs"][0]["updated_at"]' < "$tmp/runs.json")

say "Installed: ${installed_version:-unknown} (${installed_commit:-?})"
say "Available: $(printf %.7s "$commit") on $BRANCH, built $built"
say "           $title"

if [ "$mode" = check ]; then
    [ "$commit" = "$installed_commit" ] && say "Up to date." || say "Update available: sudo helix-bridge-update"
    exit 0
fi
if [ "$commit" = "$installed_commit" ] && [ "$force" -eq 0 ] && [ "$mode" = update ]; then
    say "Up to date. (--force reinstalls)"
    exit 0
fi

api "actions/runs/$run_id/artifacts" > "$tmp/artifacts.json"
artifact=$(json '([a["id"] for a in d["artifacts"] if a["name"] == "pi-bundle" and not a["expired"]] or [""])[0]' < "$tmp/artifacts.json")
[ -n "$artifact" ] || die "build $run_id has no pi-bundle (expired after 90 days?)"

# The zip lives on a signed storage URL: ask GitHub for it, then fetch it
# without the token, which must not travel to another host.
url=$(curl -fsS -o /dev/null -w '%{redirect_url}' -H @"$tmp/headers" \
    "$API/repos/$REPO/actions/artifacts/$artifact/zip") || die "could not get the download link"
[ -n "$url" ] || die "GitHub gave no download link for artifact $artifact"
say "Downloading"
curl -fsSL -o "$tmp/bundle.zip" "$url" || die "download failed"

mkdir "$tmp/zip"
python3 -m zipfile -e "$tmp/bundle.zip" "$tmp/zip"
tarball=$(find "$tmp/zip" -name 'helix-bridge-*-aarch64-linux.tar.gz' | head -n 1)
[ -n "$tarball" ] || die "the artifact holds no helix-bridge tarball"
(cd "$(dirname "$tarball")" && sha256sum -c --quiet "$(basename "$tarball").sha256") ||
    die "checksum mismatch: not installing"
say "Checksum OK: $(basename "$tarball")"

install -d -m 700 "$CACHE"
cp "$tarball" "$CACHE/"
saved="$CACHE/$(basename "$tarball")"
touch "$saved"
# Keep the newest $KEEP for --rollback.
ls -1t "$CACHE"/helix-bridge-*.tar.gz | sed -n "$((KEEP + 1)),\$p" | while IFS= read -r old; do
    rm -f "$old"
done

if [ "$mode" = download ]; then
    say "Saved $saved (not installed)"
    exit 0
fi

install_bundle "$saved"
say "Updated to $(info_field version). Previous one: sudo helix-bridge-update --rollback"
