#!/bin/sh
# Renders macos/AppIcon.svg into macos/AppIcon.icns (needs rsvg-convert:
# brew install librsvg). Run after editing the SVG and commit both files.
set -eu
cd "$(dirname "$0")/.."

SET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$SET"
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" macos/AppIcon.svg -o "$SET/icon_${size}x${size}.png"
    double=$((size * 2))
    rsvg-convert -w "$double" -h "$double" macos/AppIcon.svg -o "$SET/icon_${size}x${size}@2x.png"
done
iconutil --convert icns "$SET" --output macos/AppIcon.icns
rm -rf "$(dirname "$SET")"
echo "Wrote macos/AppIcon.icns"
