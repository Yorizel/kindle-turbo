#!/bin/sh
set -eu

fail() { printf 'install-koplugin: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 1 ] || fail 'usage: scripts/install-koplugin.sh /mount/Kindle'
root=$(CDPATH= cd -- "$1" 2>/dev/null && pwd -P) || fail 'device root is not accessible'
[ -d "$root/documents" ] || fail 'missing documents directory; refusing non-Kindle root'
[ -d "$root/koreader" ] || fail 'missing koreader directory'
[ -d "$root/koreader/plugins" ] || fail 'missing koreader/plugins directory'
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo=$(dirname "$script_dir")
source="$repo/koplugin/kindle-turbo.koplugin"
[ -d "$source" ] || fail 'plugin source directory missing'
[ -f "$repo/LICENSE" ] || fail 'repository license missing'
[ -f "$repo/third-party/TachiKindle-LICENSE" ] || fail 'upstream license missing'
target="$root/koreader/plugins/kindle-turbo.koplugin"
[ ! -e "$target" ] && [ ! -L "$target" ] || fail 'destination already exists; refusing overwrite'
stage=$(mktemp -d "$root/koreader/plugins/.kindle-turbo.koplugin.partial.XXXXXX") || fail 'cannot create staging directory'
cleanup() { [ -z "${stage:-}" ] || rm -rf -- "$stage"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
cp -R "$source"/. "$stage/"
cp "$repo/LICENSE" "$stage/LICENSE-GPL-3.0.txt"
cp "$repo/third-party/TachiKindle-LICENSE" "$stage/LICENSE-TachiKindle-Apache-2.0.txt"
cat > "$stage/PROVENANCE.md" <<'NOTE'
This repository is GPL-3.0. Adapted portions and fixtures derive from Darkgoatie/TachiKindle, commit a268fbdc0ded6dc55877e3be19cd84f8958c4798, under Apache-2.0. Upstream: https://github.com/Darkgoatie/TachiKindle
NOTE
mv -T -n -- "$stage" "$target"
[ ! -d "$stage" ] || fail 'destination appeared during installation; refusing overwrite'
stage=
printf 'Installed KOReader plugin at %s\n' "$target"
