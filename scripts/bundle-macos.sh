#!/usr/bin/env bash
# Build the native app with its Finder and Dock icon.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS is required to build the Riff.app bundle." >&2
  exit 1
fi

cargo build --release
bundle="target/Riff.app/Contents"
mkdir -p "$bundle/MacOS" "$bundle/Resources"
cp target/release/riff-rs "$bundle/MacOS/riff-rs"
cp assets/icons/Riff.icns "$bundle/Resources/Riff.icns"
cat > "$bundle/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>riff-rs</string>
  <key>CFBundleIdentifier</key><string>dev.riff.Riff</string>
  <key>CFBundleName</key><string>Riff</string>
  <key>CFBundleDisplayName</key><string>Riff</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleIconFile</key><string>Riff.icns</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSEnvironment</key>
  <dict>
    <key>MallocLargeCache</key><string>0</string>
    <key>MallocAggressiveMadvise</key><string>1</string>
  </dict>
</dict>
</plist>
PLIST
touch target/Riff.app
echo "Built target/Riff.app"
