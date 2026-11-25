#!/bin/sh
# Renders docs/images/logo.svg into app/AppIcon.icns. Run it after editing
# the logo, then commit both files. Needs macOS (AppKit draws the SVG).
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

swiftc -O "$root/scripts/svg2png.swift" -o "$work/svg2png"
set_dir="$work/AppIcon.iconset"
mkdir "$set_dir"
for size in 16 32 128 256 512; do
  "$work/svg2png" "$root/docs/images/logo.svg" "$set_dir/icon_${size}x${size}.png" "$size"
  "$work/svg2png" "$root/docs/images/logo.svg" "$set_dir/icon_${size}x${size}@2x.png" "$((size * 2))"
done
iconutil -c icns "$set_dir" -o "$root/app/AppIcon.icns"
echo "wrote app/AppIcon.icns"
