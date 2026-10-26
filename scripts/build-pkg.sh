#!/usr/bin/env bash
# Builds build/RepoSync-<version>-arm64.pkg, which installs RepoSync.app into
# /Applications. Signs with $INSTALLER_IDENTITY ("Developer ID Installer: …")
# if set; notarises if $NOTARY_PROFILE (xcrun notarytool keychain profile) is set.
#
# Usage: scripts/build-pkg.sh   (or: make pkg; builds the app first)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
BUNDLE_ID=com.chrisurf.reposync
OUT=build
PKG="$OUT/RepoSync-$VERSION-arm64.pkg"

[ -d "$OUT/RepoSync.app" ] || scripts/build-app.sh

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/root" "$stage/scripts"
cp -R "$OUT/RepoSync.app" "$stage/root/"

# Always install to /Applications, even if another copy exists elsewhere.
pkgbuild --analyze --root "$stage/root" "$stage/component.plist" >/dev/null
plutil -replace BundleIsRelocatable -bool NO "$stage/component.plist"

# Quit a running RepoSync before replacing it; start it again afterwards.
cat >"$stage/scripts/preinstall" <<'EOF'
#!/bin/sh
pkill -x RepoSync 2>/dev/null || true
exit 0
EOF
cat >"$stage/scripts/postinstall" <<'EOF'
#!/bin/sh
user=$(stat -f %Su /dev/console)
if [ -n "$user" ] && [ "$user" != root ]; then
  sudo -u "$user" open -g /Applications/RepoSync.app 2>/dev/null || true
fi
exit 0
EOF
chmod +x "$stage/scripts/preinstall" "$stage/scripts/postinstall"

pkgbuild --root "$stage/root" --component-plist "$stage/component.plist" \
  --scripts "$stage/scripts" --install-location /Applications \
  --identifier "$BUNDLE_ID" --version "$VERSION" "$stage/RepoSync.pkg"

sign=()
[ -n "${INSTALLER_IDENTITY:-}" ] && sign=(--sign "$INSTALLER_IDENTITY")
productbuild --package "$stage/RepoSync.pkg" ${sign[@]+"${sign[@]}"} "$PKG"

if [ -n "${NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$PKG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$PKG"
fi

shasum -a 256 "$PKG" | tee "$PKG.sha256"
echo "built $PKG"
