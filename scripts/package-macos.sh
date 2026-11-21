#!/bin/bash
# Build a local development app bundle; distribution/signing uses Cargo Piston.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --locked --bin fig-files

bundle="$PWD/target/macos/Fig Files.app"
iconset="$PWD/target/macos/FigFiles.iconset"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$iconset"
cp target/debug/fig-files "$bundle/Contents/MacOS/fig-files"

for size in 16 32 128 256 512; do
    sips -z "$size" "$size" icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$bundle/Contents/Resources/FigFiles.icns"

cp macos/Info.plist "$bundle/Contents/Info.plist"

codesign --force --sign - --entitlements macos/FigFiles.entitlements "$bundle"
printf 'Built %s\n' "$bundle"
