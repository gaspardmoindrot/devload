#!/bin/sh
# Checks a built Devload.app before it ships: plist, signature, icon,
# architecture, minimum macOS, and that the bundled binary runs.
#
#   scripts/check-app.sh [path/to/Devload.app]   (default: build/Devload.app)
set -eu

app=${1:-build/Devload.app}
plist="$app/Contents/Info.plist"
bin="$app/Contents/MacOS/Devload"
fail() { echo "check-app: $*" >&2; exit 1; }

[ -d "$app" ] || fail "no app at $app"
plutil -lint -s "$plist" || fail "Info.plist is invalid"

id=$(plutil -extract CFBundleIdentifier raw "$plist")
[ "$id" = dev.devload.app ] || fail "bundle id is $id"
[ "$(plutil -extract LSUIElement raw "$plist")" = true ] || fail "LSUIElement is not set"

icon=$(plutil -extract CFBundleIconFile raw "$plist")
[ -s "$app/Contents/Resources/$icon.icns" ] || fail "icon $icon.icns is missing"

codesign --verify --strict "$app" || fail "signature does not verify"
signed_id=$(codesign -dv "$app" 2>&1 | sed -n 's/^Identifier=//p')
[ "$signed_id" = "$id" ] || fail "signed as $signed_id, bundle id is $id"

archs=$(lipo -archs "$bin")
[ "$archs" = arm64 ] || fail "binary architectures: $archs"

want=$(plutil -extract LSMinimumSystemVersion raw "$plist")
minos=$(otool -l "$bin" | awk '/LC_BUILD_VERSION/ { found = 1 } found && $1 == "minos" { print $2; exit }')
[ "$minos" = "$want" ] || fail "binary targets macOS $minos, Info.plist says $want"

# The bundled binary draws its snapshots from sample data and exits: it
# touches no real data folder.
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
"$bin" --snapshot "$out" >/dev/null
count=$(find "$out" -name '*.png' | wc -l | tr -d ' ')
[ "$count" -gt 0 ] || fail "snapshot run wrote no PNG"

echo "check-app: $app OK ($id $(plutil -extract CFBundleShortVersionString raw "$plist"), $archs, macOS $minos+, $count snapshots)"
