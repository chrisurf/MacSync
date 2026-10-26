#!/usr/bin/env bash
# Builds build/RepoSync.app (Apple Silicon): SwiftUI menu bar app plus the
# Rust binary reposync. Signs with $CODESIGN_IDENTITY (Developer ID) if set,
# otherwise ad-hoc.
#
# Usage: scripts/build-app.sh   (or: make app)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
BUNDLE_ID=com.chrisurf.reposync
OUT=build
APP="$OUT/RepoSync.app"
TARGET=aarch64-apple-darwin

echo "==> reposync $VERSION (Rust, $TARGET)"
cargo build --release --locked -p reposync-cli --target "$TARGET"

echo "==> RepoSync (Swift)"
swift build -c release --arch arm64 --package-path app

echo "==> $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp app/.build/arm64-apple-macosx/release/RepoSync "$APP/Contents/MacOS/RepoSync"
# Helpers, not MacOS: "reposync" and "RepoSync" collide on case-insensitive APFS.
cp "target/$TARGET/release/reposync" "$APP/Contents/Helpers/reposync"
cp scripts/connect.sh "$APP/Contents/Resources/reposync-connect"

cat >"$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleName</key><string>RepoSync</string>
  <key>CFBundleDisplayName</key><string>RepoSync</string>
  <key>CFBundleExecutable</key><string>RepoSync</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>LSUIElement</key><true/>
  <key>NSHumanReadableCopyright</key><string>RepoSync</string>
  <key>NSLocalNetworkUsageDescription</key>
  <string>RepoSync synchronisiert deine Dateien direkt mit deinem anderen Mac im lokalen Netzwerk.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_reposync._tcp</string>
    <string>_reposync-pair._tcp</string>
  </array>
</dict>
</plist>
EOF
plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [ -n "${CODESIGN_IDENTITY:-}" ]; then
  echo "==> signing with $CODESIGN_IDENTITY"
  sign=(codesign --force --timestamp --options runtime --sign "$CODESIGN_IDENTITY")
else
  echo "==> ad-hoc signing (no CODESIGN_IDENTITY)"
  sign=(codesign --force --sign -)
fi
# Inner executables first, then the bundle.
"${sign[@]}" --identifier "$BUNDLE_ID.cli" "$APP/Contents/Helpers/reposync"
"${sign[@]}" "$APP"
codesign --verify --strict "$APP"

echo "built $APP ($VERSION)"
