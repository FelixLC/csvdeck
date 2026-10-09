#!/bin/zsh
# Builds dist/csvdeck.app. Install with: cp -R dist/csvdeck.app /Applications/
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release

APP=dist/csvdeck.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/csvdeck "$APP/Contents/MacOS/csvdeck"

ICONSET=$(mktemp -d)/csvdeck.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/csvdeck.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>csvdeck</string>
  <key>CFBundleDisplayName</key><string>csvdeck</string>
  <key>CFBundleIdentifier</key><string>dev.csvdeck.app</string>
  <key>CFBundleExecutable</key><string>csvdeck</string>
  <key>CFBundleIconFile</key><string>csvdeck</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Delimited text</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>CFBundleTypeExtensions</key><array><string>csv</string><string>tsv</string><string>psv</string><string>txt</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Apache Parquet</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>CFBundleTypeExtensions</key><array><string>parquet</string><string>pq</string></array>
    </dict>
  </array>
</dict>
</plist>
EOF

codesign --force --sign - "$APP"
# Make Finder's "Open With" list pick up the document types right away.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP"
echo "Built $APP"
