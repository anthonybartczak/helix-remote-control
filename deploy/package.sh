#!/bin/sh
# Build the Pi release tarball from already cross-compiled binaries.
# Used by .github/workflows/release.yml; runs on any Linux machine.
#
#   deploy/package.sh VERSION HELIX_BRIDGE_BIN TONEPUSH_BIN TONEPUSH_SRC OUT_DIR
#
# Writes OUT_DIR/helix-bridge-VERSION-aarch64-linux.tar.gz and its .sha256.
set -eu

[ $# -eq 5 ] || { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
version=$1 bridge_bin=$2 tonepush_bin=$3 tonepush_src=$4 out=$5

repo=$(cd "$(dirname "$0")/.." && pwd)
name="helix-bridge-$version-aarch64-linux"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir="$stage/$name"

mkdir -p "$dir/bin" "$dir/deploy" "$dir/licenses"
install -m 755 "$bridge_bin" "$dir/bin/helix-bridge"
install -m 755 "$tonepush_bin" "$dir/bin/tonepush"
install -m 755 "$repo/deploy/install.sh" "$dir/install.sh"
install -m 755 "$repo/deploy/update.sh" "$dir/deploy/update.sh"
# What this bundle is, for helix-bridge-update. GITHUB_* are set in Actions.
{
    echo "version=$version"
    echo "commit=${GITHUB_SHA:-$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)}"
    if [ "${GITHUB_REF_TYPE:-branch}" = branch ]; then
        echo "ref=${GITHUB_REF_NAME:-$(git -C "$repo" rev-parse --abbrev-ref HEAD 2>/dev/null)}"
    fi
} > "$dir/BUILD_INFO"
for f in helix-bridge.service helix-bridge.env 70-helix-bridge.rules avahi-helix-bridge.service; do
    install -m 644 "$repo/deploy/$f" "$dir/deploy/$f"
done
# Lets HELIX_BACKEND=fake run on the Pi without the pedal.
cp -R "$repo/server/fixtures" "$dir/fixtures"
# TonePush is MIT: its notice ships with its binary (AGENTS.md 3).
install -m 644 "$tonepush_src/LICENSE" "$dir/licenses/tonepush-LICENSE"
install -m 644 "$repo/docs/deploy.md" "$dir/README.md"

mkdir -p "$out"
out=$(cd "$out" && pwd)
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
