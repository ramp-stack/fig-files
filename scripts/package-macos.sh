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

cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundleName</key><string>Fig Files</string>
    <key>CFBundleDisplayName</key><string>Fig Files</string>
    <key>CFBundleIdentifier</key><string>org.ramp.fig-files</string>
    <key>CFBundleExecutable</key><string>fig-files</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>FigFiles.icns</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict></plist>
PLIST

codesign --force --sign - "$bundle"
printf 'Built %s\n' "$bundle"
