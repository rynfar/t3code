#!/bin/sh
# Build a release binary and wrap it as "T3 Code Native.app" in target/. Unsigned; for local use.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p t3-native
app="target/T3 Code Native.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/t3-native "$app/Contents/MacOS/t3-native"
mkdir -p "$app/Contents/Resources/bin"
[ -z "${T3_NATIVE_T3_BIN:-}" ] || install -m 755 "$T3_NATIVE_T3_BIN" "$app/Contents/Resources/bin/t3"
[ -z "${T3_NATIVE_LICENSE_MANIFEST:-}" ] || install -m 644 "$T3_NATIVE_LICENSE_MANIFEST" "$app/Contents/Resources/third-party-licenses.json"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>T3 Code Native</string>
  <key>CFBundleDisplayName</key><string>T3 Code Native</string>
  <key>CFBundleIdentifier</key><string>codes.t3.native.dev</string>
  <key>CFBundleExecutable</key><string>t3-native</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleURLTypes</key>
  <array><dict>
    <key>CFBundleURLName</key><string>codes.t3.native.prompt-link</string>
    <key>CFBundleURLSchemes</key><array><string>t3code-native</string></array>
  </dict></array>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
echo "$app"
