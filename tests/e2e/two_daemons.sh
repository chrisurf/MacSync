#!/usr/bin/env bash
# End-to-end test: two MacSync daemons on loopback, paired with a code,
# continuous sync of Git repositories and ordinary folders, Git torture
# sequence, latency measurement, conflict detection.
#
# Usage: tests/e2e/two_daemons.sh [path/to/macsync]   (default: target/release/macsync)
# Writes a machine-readable summary to $OUT (default: target/e2e-summary.json).
set -euo pipefail

BIN="${1:-target/release/macsync}"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
OUT="${OUT:-target/e2e-summary.json}"
WORK="$(mktemp -d)"
PORT_A=$((47200 + RANDOM % 300 * 2))
PORT_B=$((PORT_A + 400))
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  [ "${KEEP:-0}" = 1 ] || rm -rf "$WORK"
}
trap cleanup EXIT

export GIT_AUTHOR_NAME=E2E GIT_AUTHOR_EMAIL=e2e@example.com GIT_COMMITTER_NAME=E2E GIT_COMMITTER_EMAIL=e2e@example.com
g() { git -c init.defaultBranch=main -c gc.auto=0 -c maintenance.auto=false "$@"; }
now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { log "FAIL: $*"; echo "--- daemon A log"; tail -50 "$WORK/a.log" || true; echo "--- daemon B log"; tail -50 "$WORK/b.log" || true; exit 1; }

A="$WORK/A"; B="$WORK/B"
ra() { "$BIN" --config "$WORK/a.toml" "$@"; }
rb() { "$BIN" --config "$WORK/b.toml" "$@"; }

# wait_until <timeout-s> <command…>
wait_until() {
  local t=$1; shift
  local deadline=$(( $(date +%s) + t ))
  until "$@" >/dev/null 2>&1; do
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}

roots_equal() {
  local a b
  a=$(ra status --json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sorted((u["name"],u["root"]) for u in d["units"]))')
  b=$(rb status --json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sorted((u["name"],u["root"]) for u in d["units"]))')
  [ "$a" = "$b" ]
}

# Portable tree comparison (content, mode, symlink targets; .git/index is
# compared semantically by MacSync, so it is skipped here).
trees_equal() {
  python3 - "$A" "$B" <<'PY'
import os, stat, sys
def snap(root):
    out = {}
    for d, dirs, files in os.walk(root):
        rel_d = os.path.relpath(d, root)
        if rel_d.split(os.sep)[0] == ".macsync":
            dirs[:] = []
            continue
        for n in dirs + files:
            p = os.path.join(d, n)
            rel = os.path.normpath(os.path.join(rel_d, n))
            if rel.startswith(".macsync") or n.endswith(".lock") or rel.endswith(os.path.join(".git", "index")):
                continue
            st = os.lstat(p)
            if stat.S_ISLNK(st.st_mode):
                out[rel] = ("l", os.readlink(p))
            elif stat.S_ISDIR(st.st_mode):
                out[rel] = ("d", st.st_mode & 0o7777)
            else:
                with open(p, "rb") as f:
                    out[rel] = ("f", st.st_mode & 0o7777, f.read())
    return out
sys.exit(0 if snap(sys.argv[1]) == snap(sys.argv[2]) else 1)
PY
}

converged() { roots_equal && trees_equal; }

log "work dir $WORK, ports $PORT_A/$PORT_B"
# E2E_DISCOVERY=1: no fixed peer address; the daemons find each other by
# their blinded Bonjour announcement (needs multicast, so off by default).
if [ "${E2E_DISCOVERY:-0}" = 1 ]; then
  ra init --root "$A" --name "Mac A" --listen "0.0.0.0:$PORT_A" --state-dir "$WORK/a-state" >/dev/null
  rb init --root "$B" --name "Mac B" --listen "0.0.0.0:$PORT_B" --state-dir "$WORK/b-state" >/dev/null
else
  ra init --root "$A" --name "Mac A" --listen "127.0.0.1:$PORT_A" --peer "127.0.0.1:$PORT_B" --state-dir "$WORK/a-state" >/dev/null
  rb init --root "$B" --name "Mac B" --listen "127.0.0.1:$PORT_B" --peer "127.0.0.1:$PORT_A" --state-dir "$WORK/b-state" >/dev/null
  for c in a b; do
    sed -i.bak 's/^discovery = true/discovery = false/' "$WORK/$c.toml"
  done
fi

log "pairing"
rb pair listen --timeout-s 30 > "$WORK/pair.out" 2>&1 &
PAIR_PID=$!
wait_until 10 grep -q "pairing code" "$WORK/pair.out" || fail "no pairing code"
CODE=$(sed -n 's/^pairing code: \([0-9]*\) \([0-9]*\)$/\1\2/p' "$WORK/pair.out")
# A wrong code is rejected first.
if echo 000000 | ra pair connect "127.0.0.1:$((PORT_B + 1))" >/dev/null 2>&1 && [ "$CODE" != 000000 ]; then fail "wrong code accepted"; fi
MACSYNC_PAIR_CODE="$CODE" ra pair connect "127.0.0.1:$((PORT_B + 1))" > "$WORK/pair-a.out"
wait $PAIR_PID || fail "pair listen failed"
grep -q "paired with Mac B" "$WORK/pair-a.out" || fail "A not paired"
grep -q "paired with Mac A" "$WORK/pair.out" || fail "B not paired"
FP_A=$(sed -n 's/^fingerprint: \(.*\) (the other.*/\1/p' "$WORK/pair-a.out")
FP_B=$(sed -n 's/^fingerprint: \(.*\) (the other.*/\1/p' "$WORK/pair.out")
[ -n "$FP_A" ] && [ "$FP_A" = "$FP_B" ] || fail "fingerprints differ: '$FP_A' vs '$FP_B'"
log "fingerprint on both Macs: $FP_A"

# Initial content on A before the daemons start.
mkdir -p "$A/notes" "$A/web"
echo "# Notizen" > "$A/notes/README.md"
( cd "$A/web" && g init -q && echo "console.log(1)" > index.js && g add . && g commit -qm initial )
for i in $(seq 1 3000); do d="$A/web/node_modules/pkg$((i % 60))"; mkdir -p "$d"; echo "module.exports=$i" > "$d/f$i.js"; done
echo "node_modules/" > "$A/web/.gitignore"

if [ "${E2E_DISCOVERY:-0}" = 1 ]; then
  # Forget the address seen at pairing: only Bonjour can find the peer now.
  for c in a b; do sqlite3 "$WORK/$c-state/macsync.db" "UPDATE peers SET addr = ''"; done
fi

log "starting daemons"
# Start the binary directly (not via a shell function) so that $! is the
# daemon itself and cleanup terminates it.
MACSYNC_LOG=info "$BIN" --config "$WORK/a.toml" daemon > "$WORK/a.log" 2>&1 & PIDS+=($!)
MACSYNC_LOG=info "$BIN" --config "$WORK/b.toml" daemon > "$WORK/b.log" 2>&1 & PIDS+=($!)

T0=$(now_ms)
wait_until 120 converged || fail "initial sync did not converge"
INITIAL_MS=$(( $(now_ms) - T0 ))
log "initial sync converged in ${INITIAL_MS} ms (3000+ files)"
if [ "${E2E_DISCOVERY:-0}" = 1 ]; then
  log "found via blinded Bonjour: $(ra peers)"
fi
( cd "$B/web" && git fsck --full --no-dangling ) >/dev/null || fail "fsck after initial sync"

log "latency of a single small change"
LAT=()
for i in 1 2 3 4 5; do
  T=$(now_ms)
  echo "change $i" > "$A/notes/latency-$i.txt"
  wait_until 20 test -f "$B/notes/latency-$i.txt" || fail "latency change $i not applied"
  wait_until 20 grep -q "change $i" "$B/notes/latency-$i.txt" || fail "content $i wrong"
  LAT+=($(( $(now_ms) - T )))
done
log "latencies (ms): ${LAT[*]}"

log "git torture on A"
cd "$A/web"
for i in $(seq 1 5); do echo "line $i" >> index.js; g commit -qam "commit $i"; done
g checkout -qb feature
echo "feature" > feature.js && g add . && g commit -qm "feature"
g checkout -q main
echo "wip" >> index.js
g stash -q
g rebase -q feature
g stash pop -q
g gc -q --prune=now
echo "staged" > staged.js && g add staged.js
cd - >/dev/null
wait_until 60 converged || fail "torture did not converge"
( cd "$B/web" && git fsck --full --no-dangling ) >/dev/null || fail "fsck after torture"
[ "$(cd "$A/web" && git rev-parse HEAD)" = "$(cd "$B/web" && git rev-parse HEAD)" ] || fail "HEAD differs"
[ "$(cd "$A/web" && git status --porcelain)" = "$(cd "$B/web" && git status --porcelain)" ] || fail "status differs"
# MacSync must never create stashes/refs of its own: B mirrors A exactly.
SA=$(cd "$A/web" && git stash list; git for-each-ref); SB=$(cd "$B/web" && git stash list; git for-each-ref)
[ "$SA" = "$SB" ] || { echo "A: $SA"; echo "B: $SB"; fail "refs/stashes differ"; }
log "torture converged; fsck ok; HEAD $(cd "$B/web" && git rev-parse --short HEAD)"

log "work continues on B"
( cd "$B/web" && g commit -qm "from B" && echo b > b.js && g add b.js && g commit -qm "b.js" )
wait_until 30 converged || fail "B → A did not converge"
[ "$(cd "$A/web" && git log -1 --format=%s)" = "b.js" ] || fail "commit from B missing on A"
( cd "$A/web" && git fsck --full --no-dangling ) >/dev/null || fail "fsck on A"

log "deletion of a whole folder"
rm -rf "$A/notes"
wait_until 30 test ! -e "$B/notes" || fail "folder deletion not propagated"

log "concurrent edit → conflict (daemons paused while editing)"
mkdir -p "$A/shared"; echo base > "$A/shared/doc.txt"
wait_until 30 test -f "$B/shared/doc.txt" || fail "shared doc not synced"
wait_until 30 converged || fail "not converged before conflict"
ra pause >/dev/null; rb pause >/dev/null
echo "edit A" > "$A/shared/doc.txt"; echo "edit B" > "$B/shared/doc.txt"
ra resume >/dev/null; rb resume >/dev/null
wait_until 30 sh -c "\"$BIN\" --config \"$WORK/a.toml\" conflicts | grep -q concurrent-modify" || fail "no conflict on A"
[ "$(cat "$A/shared/doc.txt")" = "edit A" ] || fail "A lost its version"
[ "$(cat "$B/shared/doc.txt")" = "edit B" ] || fail "B lost its version"
ID=$(ra conflicts --json | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')
ra resolve "$ID" --keep remote >/dev/null
wait_until 30 converged || fail "not converged after resolution"
[ "$(cat "$A/shared/doc.txt")" = "edit B" ] || fail "resolution not applied"

log "put first: a queued folder is pulled before the others"
rb pause >/dev/null
for u in q1 q2 q3 q4; do mkdir -p "$A/$u"; head -c 200000 /dev/urandom > "$A/$u/data.bin"; done
wait_until 30 sh -c "\"$BIN\" --config \"$WORK/a.toml\" status --json | grep -q '\"q4\"'" || fail "q-units not captured on A"
rb first q4 | grep -q "next: q4" || fail "first q4 not accepted"
rb status --json | python3 -c 'import json,sys; q=json.load(sys.stdin)["priority"]; assert [p["unit"] for p in q]==["q4"], q' \
  || fail "q4 not in the priority list"
rb resume >/dev/null
wait_until 30 test -f "$B/q1/data.bin" -a -f "$B/q4/data.bin" || fail "q-units not synced"
# Without colour codes; `applied unit=…` lines in the order of the pulls.
ORDER=$(sed $'s/\x1b\\[[0-9;]*m//g' "$WORK/b.log" | grep -o 'applied unit=q[1-4]' | sed 's/applied unit=//' | tr '\n' ' ' || true)
[ "${ORDER%% *}" = q4 ] || fail "q4 was not first: $ORDER"
log "pull order on B: $ORDER"
wait_until 30 converged || fail "not converged after put first"

log "status of the other Mac during a large transfer"
mkdir -p "$A/big"
head -c 600000000 /dev/urandom > "$A/big/large.bin"
transferring() { rb status --json | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("transfer") else 1)'; }
wait_until 60 transferring || log "transfer not seen (finished already?)"
SLOW=0; N=0
while [ "$N" -lt 5 ] && ! cmp -s "$A/big/large.bin" "$B/big/large.bin"; do
  T=$(now_ms)
  rb browse --json > "$WORK/browse.json" || fail "browse during transfer failed"
  D=$(( $(now_ms) - T )); N=$((N + 1))
  log "browse during transfer: ${D} ms"
  [ "$D" -lt 3000 ] || SLOW=$((SLOW + 1))
done
[ "$SLOW" = 0 ] || fail "browse waited behind the transfer"
wait_until 120 converged || fail "large file not converged"
rm -rf "$A/big"
wait_until 60 test ! -e "$B/big" || fail "large file not removed"

log "diagnosis of both Macs"
rb diag --minutes 10 --json > "$WORK/diag.json" || fail "diag failed"
python3 - "$WORK/diag.json" <<'PY' || fail "diag misses events of one side"
import json, sys
d = json.load(open(sys.argv[1]))["devices"]
assert len(d) == 2, d
for dev in d:
    assert "error" not in dev, dev.get("error")
    kinds = {e["kind"] for e in dev["events"]}
    assert "daemon-start" in kinds, (dev["name"], kinds)
    assert kinds & {"pull", "scan"}, (dev["name"], kinds)
assert abs(d[1]["offset_ms"]) < 1000, d[1]["offset_ms"]
PY
rb diag --minutes 10 --out "$WORK/diag.txt" >/dev/null || fail "diag --out failed"
grep -q "Summary" "$WORK/diag.txt" || fail "diag text without summary"
log "diag: $(grep -c . "$WORK/diag.txt") lines, both Macs"

log "verify"
ra verify --json > "$WORK/verify.json"
SILENT=$(python3 -c 'import json; print(sum(r["silent_changes"] for r in json.load(open("'"$WORK"'/verify.json"))))')
[ "$SILENT" = 0 ] || fail "verify found silent drift"

# Idle CPU: CPU time consumed by both daemons during 10 s without changes.
cpu_ms() {  # cumulative user+sys CPU time of a pid in ms
  if [ -r "/proc/$1/stat" ]; then  # Linux: exact clock ticks
    python3 -c 'import os,sys; f=open("/proc/%s/stat" % sys.argv[1]).read().rsplit(")",1)[1].split(); print(int((int(f[11])+int(f[12]))*1000/os.sysconf("SC_CLK_TCK")))' "$1"
  else  # macOS: ps TIME has centiseconds ([[dd-]hh:]mm:ss.cc)
    ps -o time= -p "$1" | python3 -c '
import sys
t = sys.stdin.read().strip().replace("-", ":")
s = 0.0
for p in t.split(":"): s = s * 60 + float(p)
print(int(s * 1000))'
  fi
}
sleep 2
C0=$(( $(cpu_ms "${PIDS[0]}") + $(cpu_ms "${PIDS[1]}") ))
sleep 10
C1=$(( $(cpu_ms "${PIDS[0]}") + $(cpu_ms "${PIDS[1]}") ))
CPU=$(python3 -c "print(round(($C1 - $C0) / 10000 * 100, 2))")
log "idle CPU of both daemons over 10 s: ${CPU}% of one core"

mkdir -p "$(dirname "$OUT")"
python3 - "$OUT" "$INITIAL_MS" "${LAT[*]}" "$CPU" <<'PY'
import json, sys
lat = [int(x) for x in sys.argv[3].split()]
json.dump({"initial_sync_ms": int(sys.argv[2]), "files_initial": 3003,
           "latency_ms": lat, "latency_max_ms": max(lat), "latency_median_ms": sorted(lat)[len(lat)//2],
           "idle_cpu_percent_10s": float(sys.argv[4]), "result": "pass"}, open(sys.argv[1], "w"), indent=2)
PY
log "PASS — summary in $OUT"
