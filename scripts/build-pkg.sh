#!/usr/bin/env bash
# Builds build/MacSync-<version>-arm64.pkg, which installs MacSync.app into
# /Applications (and removes RepoSync.app, the app's name in 0.2.0). Signs with
# $INSTALLER_IDENTITY ("Developer ID Installer: …") if set; notarises if
# $NOTARY_PROFILE (xcrun notarytool keychain profile) is set.
#
# Usage: scripts/build-pkg.sh   (or: make pkg; builds the app first)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
BUNDLE_ID=com.alpineperspective.macsync
OUT=build
PKG="$OUT/MacSync-$VERSION-arm64.pkg"

[ -d "$OUT/MacSync.app" ] || scripts/build-app.sh

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/root" "$stage/scripts"
cp -R "$OUT/MacSync.app" "$stage/root/"

# Always install to /Applications, even if another copy exists elsewhere.
pkgbuild --analyze --root "$stage/root" "$stage/component.plist" >/dev/null
plutil -replace BundleIsRelocatable -bool NO "$stage/component.plist"

# Quit a running copy before replacing it (RepoSync = the app's name in 0.2.0,
# removed); start MacSync again afterwards.
cat >"$stage/scripts/preinstall" <<'EOF'
#!/bin/sh
pkill -x MacSync 2>/dev/null || true
pkill -x RepoSync 2>/dev/null || true
sleep 1
rm -rf /Applications/RepoSync.app
pkgutil --forget com.chrisurf.reposync >/dev/null 2>&1 || true
exit 0
EOF
cat >"$stage/scripts/postinstall" <<'EOF'
#!/bin/sh
user=$(stat -f %Su /dev/console)
if [ -n "$user" ] && [ "$user" != root ]; then
  sudo -u "$user" open -g /Applications/MacSync.app 2>/dev/null || true
fi
exit 0
EOF
chmod +x "$stage/scripts/preinstall" "$stage/scripts/postinstall"

pkgbuild --root "$stage/root" --component-plist "$stage/component.plist" \
  --scripts "$stage/scripts" --install-location /Applications \
  --identifier "$BUNDLE_ID" --version "$VERSION" "$stage/MacSync.pkg"

sign=()
[ -n "${INSTALLER_IDENTITY:-}" ] && sign=(--sign "$INSTALLER_IDENTITY")
productbuild --package "$stage/MacSync.pkg" ${sign[@]+"${sign[@]}"} "$PKG"

if [ -n "${NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$PKG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$PKG"
fi

shasum -a 256 "$PKG" | tee "$PKG.sha256"
echo "built $PKG"
