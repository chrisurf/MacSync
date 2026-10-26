#!/usr/bin/env bash
# Renders the README screenshots (docs/images/) from neutral demo data:
# two paired daemons on this Mac, demo files, the menu bar app in demo mode.
# Never touches the real configuration (explicit --config / MACSYNC_CONFIG).
#
# Usage: scripts/screenshots.sh   (or: make screenshots; needs make app first)
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=docs/images
MS="$PWD/target/aarch64-apple-darwin/release/macsync"
APP="$PWD/build/MacSync.app/Contents/MacOS/MacSync"
[ -x "$MS" ] && [ -x "$APP" ] || { echo "build first: make app"; exit 1; }
mkdir -p "$OUT"

# Short path: Unix sockets allow ~100 characters.
T=$(mktemp -d /tmp/msdemo.XXXX)
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  pkill -f "$T" 2>/dev/null || true
  rm -rf "$T"
}
trap cleanup EXIT

a() { "$MS" --config "$T/a.toml" "$@"; }
b() { "$MS" --config "$T/b.toml" "$@"; }
a init --root "$T/a/MacSync" --state-dir "$T/sa" --listen 0.0.0.0:48700 --name "MacBook Air" >/dev/null
b init --root "$T/b/MacSync" --state-dir "$T/sb" --listen 0.0.0.0:48710 --name "MacBook Pro" >/dev/null

a pair listen --json >"$T/pair" 2>&1 & pids+=($!)
sleep 1
code=$(sed -n 's/.*"code":"\([0-9]*\)".*/\1/p' "$T/pair")
b pair connect 127.0.0.1:48701 "$code" >/dev/null

# Demo content on the "MacBook Air".
R="$T/a/MacSync"
mkdir -p "$R/Projekte/website/src" "$R/Projekte/app/Sources" "$R/Notizen" "$R/Dokumente" "$R/Fotos"
echo "<h1>Hallo</h1>" >"$R/Projekte/website/index.html"
echo "body { font-family: system-ui }" >"$R/Projekte/website/src/style.css"
echo 'print("MacSync")' >"$R/Projekte/app/Sources/main.swift"
for p in website app; do
  (cd "$R/Projekte/$p" && git init -q && git add . && git -c user.name=Demo -c user.email=demo@example.com commit -qm init)
done
printf '# Ideen\n- Urlaub planen\n' >"$R/Notizen/Ideen.md"
printf '# Einkauf\n- Kaffee\n' >"$R/Notizen/Einkauf.md"
printf '# Protokoll\n' >"$R/Notizen/Protokoll.md"
head -c 2400000 /dev/urandom >"$R/Fotos/Strand.jpg"
head -c 1800000 /dev/urandom >"$R/Fotos/Berge.jpg"
head -c 180000 /dev/urandom >"$R/Dokumente/Mietvertrag.pdf"

MACSYNC_LOG=warn a daemon >"$T/a.log" 2>&1 & pids+=($!)
MACSYNC_LOG=warn b daemon >"$T/b.log" 2>&1 & pids+=($!)
for _ in $(seq 1 30); do [ -f "$T/b/MacSync/Dokumente/Mietvertrag.pdf" ] && break; sleep 1; done
sleep 3

# Pending changes: the MacBook Pro pauses, the MacBook Air keeps working.
b pause >/dev/null
printf '# Ideen\n- Urlaub planen\n- MacSync testen\n' >"$R/Notizen/Ideen.md"
printf '# Meeting\n- Release 0.3.0\n' >"$R/Notizen/Meeting.md"
rm "$R/Notizen/Protokoll.md"
head -c 3100000 /dev/urandom >"$R/Fotos/Wald.jpg"
sleep 4

shot() { # shot <file> [folder]
  rm -f "$OUT/$1"
  MACSYNC_SNAPSHOT="$PWD/$OUT/$1" MACSYNC_CONFIG="$T/b.toml" "$APP" --manager ${2:+"$2"} >/dev/null 2>&1 &
  local pid=$!
  for _ in $(seq 1 20); do [ -f "$OUT/$1" ] && break; sleep 0.5; done
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  echo "wrote $OUT/$1"
}
shot manager.png
shot manager-folder.png Notizen

# Menu bar states and the app icon.
{
  echo 'import AppKit'
  # SyncState without its title texts (only the cases are needed).
  sed -n '/^enum SyncState/,/^}/p' app/Sources/MacSync/AppModel.swift | sed -n '1,2p'
  echo '}'
  sed -n '/^enum MenuIcon/,$p' app/Sources/MacSync/MenuIcon.swift
  cat app/Icon/render-menubar.swift
} >"$T/menubar.swift"
swift "$T/menubar.swift" "$OUT/menubar.png"
sips -Z 256 app/Resources/AppIcon.png --out "$OUT/app-icon.png" >/dev/null
echo "wrote $OUT/menubar.png, $OUT/app-icon.png"
