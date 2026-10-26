#!/usr/bin/env bash
# Connects this Mac with the other one in a single step: configuration,
# pairing and (optionally) starting the daemon. Run the same command on both
# Macs, in the directory that should be synchronised – no host names needed.
#
# Usage: reposync-connect [options] [DIR]   (or: make connect ROOT=DIR)
#        reposync-connect --stop | --help
set -uo pipefail

PROG=$(basename "$0")
ROOT=""
PEER=""
PORT=47123
NAME=""
MODE="two-way"
CONFIG="${REPOSYNC_CONFIG:-$HOME/.config/reposync/config.toml}"
ROLE=auto
WAIT_S=4
TIMEOUT_S=300
RUN=ask
YES=0
FORCE=0
STOP=0
SERVICE=_reposync-pair._tcp

usage() {
  cat <<EOF
$PROG – connect two Macs with RepoSync (configure, pair, start)

Usage: $PROG [options] [DIR]

Run the same command on both Macs. The order does not matter:
  1. Mac 1: finds nobody, waits and shows a 6-digit code
  2. Mac 2: finds Mac 1 via Bonjour and asks for that code
Then both start synchronising (in the foreground; stop with Ctrl-C).

  DIR                 directory to synchronise; without DIR the script asks
                      (suggestion: current directory; in ~ or a Git repository
                      the configured directory or ~/RepoSyncTest)

Options:
  -p, --peer HOST[:PORT]
                      the other Mac (name.local or IP, its port if not
                      the same as ours) – skips the Bonjour search
  -P, --port N        protocol port (default 47123, pairing uses N+1)
  -n, --name NAME     device name (default: computer name)
  -m, --mode MODE     two-way | mirror-send | mirror-receive (default two-way)
      --send          short for --mode mirror-send   (this Mac only sends)
      --receive       short for --mode mirror-receive (this Mac only receives)
      --listen        always wait for the other Mac (show the code)
      --connect       always connect to the other Mac (enter the code)
  -w, --wait S        seconds to search for the other Mac (default 4)
  -c, --config FILE   configuration file (default ~/.config/reposync/config.toml)
  -f, --force         overwrite an existing configuration / pair again
      --run           start the daemon afterwards without asking
      --no-run        do not start the daemon
  -y, --yes           no questions: accept all defaults
      --stop          stop a running daemon / pairing wait on this Mac
  -h, --help          this help

Examples:
  $PROG ~/RepoSyncTest                    # on both Macs
  $PROG                                    # asks for the directory
  $PROG --stop                             # stop synchronising
  $PROG --peer macbook-b.local             # Bonjour unavailable (guest Wi-Fi)
  $PROG --send   (Mac A)   /   $PROG --receive   (Mac B)
EOF
}

die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*"; }
need_arg() { [ $# -ge 2 ] && [ -n "$2" ] || die "$1 needs a value (see --help)"; }

# ask "question" default(y|n) → 0 for yes
ask() {
  local q=$1 def=$2 hint="[Y/n]" a
  [ "$def" = n ] && hint="[y/N]"
  if [ $YES = 1 ] || [ ! -t 0 ]; then [ "$def" = y ]; return; fi
  read -r -p "$q $hint " a
  a=${a:-$def}
  case "$a" in [yYjJ]*) return 0 ;; *) return 1 ;; esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    -p|--peer) need_arg "$@"; PEER=$2; shift ;;
    -P|--port) need_arg "$@"; PORT=$2; shift ;;
    -n|--name) need_arg "$@"; NAME=$2; shift ;;
    -m|--mode) need_arg "$@"; MODE=$2; shift ;;
    --send) MODE=mirror-send ;;
    --receive) MODE=mirror-receive ;;
    --listen) ROLE=listen ;;
    --connect) ROLE=connect ;;
    -w|--wait) need_arg "$@"; WAIT_S=$2; shift ;;
    -c|--config) need_arg "$@"; CONFIG=$2; shift ;;
    -f|--force) FORCE=1 ;;
    --run) RUN=yes ;;
    --no-run) RUN=no ;;
    -y|--yes) YES=1 ;;
    --stop) STOP=1 ;;
    -*) die "unknown option $1 (see --help)" ;;
    *) [ -z "$ROOT" ] || die "only one directory allowed"; ROOT=$1 ;;
  esac
  shift
done

case "$PORT" in ''|*[!0-9]*) die "--port must be a number" ;; esac
[ "$PORT" -ge 1 ] && [ "$PORT" -le 65534 ] || die "--port must be between 1 and 65534"
PAIR_PORT=$((PORT + 1))
PEER_HOST=${PEER%%:*}
PEER_PORT=$PORT
case "$PEER" in *:*) PEER_PORT=${PEER##*:} ;; esac
case "$PEER_PORT" in ''|*[!0-9]*) die "--peer port must be a number" ;; esac
case "$MODE" in two-way|mirror-send|mirror-receive) ;; *) die "--mode must be two-way, mirror-send or mirror-receive" ;; esac
case "$WAIT_S" in ''|*[!0-9]*) die "--wait must be a number" ;; esac

IS_MAC=0; [ "$(uname -s)" = Darwin ] && IS_MAC=1
[ -n "$NAME" ] || NAME=$(scutil --get ComputerName 2>/dev/null || hostname -s)
SELF_HOST=$( [ $IS_MAC = 1 ] && scutil --get LocalHostName 2>/dev/null || hostname -s)

# --- reposync binary -------------------------------------------------------
RS_BIN=$(command -v reposync || true)
[ -n "$RS_BIN" ] || { [ -x "$HOME/.local/bin/reposync" ] && RS_BIN="$HOME/.local/bin/reposync"; }
[ -n "$RS_BIN" ] || die "reposync is not installed – run 'make install' in the RepoSync repository"
rs() { "$RS_BIN" --config "$CONFIG" "$@"; }

# --- stop ------------------------------------------------------------------
if [ $STOP = 1 ]; then
  stopped=0
  pkill -f "$SERVICE" 2>/dev/null && stopped=1
  if pkill -INT -f "reposync .*daemon" 2>/dev/null; then
    stopped=1; sleep 1
    pkill -f "reposync .*daemon" 2>/dev/null
  fi
  pkill -f "reposync .*pair listen" 2>/dev/null && stopped=1
  if [ $stopped = 1 ]; then info "stopped"; else info "nothing was running"; fi
  exit 0
fi

# --- directory -------------------------------------------------------------
if [ -z "$ROOT" ]; then
  suggest=$PWD
  if [ "$PWD" = "$HOME" ] || [ -e "$PWD/.git" ]; then
    suggest="$HOME/RepoSyncTest"
    prev=$(sed -n 's/^root *= *"\(.*\)"/\1/p' "$CONFIG" 2>/dev/null)
    [ -n "$prev" ] && [ -d "$prev" ] && [ ! -e "$prev/.git" ] && [ "$prev" != "$HOME" ] && suggest=$prev
  fi
  if [ $YES = 0 ] && [ -t 0 ]; then
    read -r -p "Directory to synchronise [$suggest]: " ROOT || exit 1
  fi
  ROOT=${ROOT:-$suggest}
fi
ROOT=${ROOT/#\~/$HOME}
mkdir -p "$ROOT" || die "cannot create $ROOT"
ROOT=$(cd "$ROOT" && pwd -P)
case "$ROOT" in
  /|"$HOME"|/Users|/Volumes|/System*|/Library*|/Applications*)
    die "refusing to synchronise $ROOT – choose a dedicated directory, e.g. ~/RepoSyncTest" ;;
  "$HOME/Library"*) die "refusing to synchronise inside ~/Library" ;;
esac
case "$ROOT" in
  "$HOME/Desktop"*|"$HOME/Documents"*|"$HOME/Downloads"*|"$HOME/Library/Mobile Documents"*)
    warn "$ROOT is privacy-protected or in iCloud; macOS will ask Terminal for access" ;;
esac
files=$(find "$ROOT" -mindepth 1 -maxdepth 1 ! -name .DS_Store ! -name .reposync 2>/dev/null | wc -l | tr -d ' ')

info "this Mac : $NAME ($SELF_HOST.local)"
info "directory: $ROOT ($files entries)"
info "mode     : $MODE, port $PORT (pairing $PAIR_PORT)"

# --- 1. configuration ------------------------------------------------------
if [ -f "$CONFIG" ]; then
  cur_root=$(sed -n 's/^root *= *"\(.*\)"/\1/p' "$CONFIG")
  cur_root=${cur_root/#\~/$HOME}
  cur_port=$(sed -n 's/^listen *= *".*:\([0-9]*\)"/\1/p' "$CONFIG")
  cur_mode=$(sed -n 's/^mode *= *"\(.*\)"/\1/p' "$CONFIG")
  if [ $FORCE = 0 ] && [ "$cur_root" = "$ROOT" ] && [ "${cur_port:-47123}" = "$PORT" ] && [ "${cur_mode:-two-way}" = "$MODE" ]; then
    info "configuration $CONFIG is already set up for this directory"
    write_config=0
  else
    warn "existing configuration $CONFIG: root $cur_root, mode ${cur_mode:-two-way}, port ${cur_port:-47123}"
    if [ $FORCE = 1 ] || ask "Replace it with $ROOT / $MODE / $PORT?" n; then
      write_config=1
    else
      die "kept the existing configuration (use --force to replace it)"
    fi
  fi
else
  write_config=1
fi
if [ "$write_config" = 1 ]; then
  args=(init --root "$ROOT" --name "$NAME" --mode "$MODE" --listen "0.0.0.0:$PORT" --force)
  [ -n "$PEER" ] && args+=(--peer "$PEER_HOST:$PEER_PORT")
  rs "${args[@]}" >/dev/null || die "reposync init failed"
  info "configuration written to $CONFIG"
fi

# --- 2. pairing ------------------------------------------------------------
# Bonjour search for a Mac that is waiting with a pairing code.
# Prints "host port name" lines.
browse() {
  local tmp pid inst
  tmp=$(mktemp -d)
  dns-sd -B "$SERVICE" local. >"$tmp/b" 2>/dev/null & pid=$!
  sleep "$WAIT_S"; kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  awk '$2 == "Add" { $1=$2=$3=$4=$5=$6=""; sub(/^ +/, ""); print }' "$tmp/b" | sort -u >"$tmp/names"
  while IFS= read -r inst; do
    [ -n "$inst" ] || continue
    dns-sd -L "$inst" "$SERVICE" local. >"$tmp/l" 2>/dev/null & pid=$!
    sleep 2; kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    sed -n 's/.* can be reached at \([^ ]*\)\.:\([0-9]*\) .*/\1 \2/p' "$tmp/l" | head -1 | while read -r h p; do
      [ "$h" = "$SELF_HOST.local" ] || printf '%s %s %s\n' "$h" "$p" "$inst"
    done
  done <"$tmp/names"
  rm -rf "$tmp"
}

do_listen() {
  local adv=""
  if [ $IS_MAC = 1 ]; then
    dns-sd -R "$NAME" "$SERVICE" local. "$PAIR_PORT" >/dev/null 2>&1 & adv=$!
    trap '[ -n "$adv" ] && kill $adv 2>/dev/null' EXIT INT TERM
  fi
  echo
  info "this Mac waits. Now run '$PROG' on the OTHER Mac and enter this code there:"
  rs pair listen --timeout-s "$TIMEOUT_S" | sed '/on the other Mac run/d'
  local rc=$?
  [ -n "$adv" ] && kill "$adv" 2>/dev/null
  trap - EXIT INT TERM
  case $rc in 130|143) info "stopped"; exit 0 ;; esac
  return $rc
}

do_connect() {
  local host=$1 port=$2 code tries=0
  [ -t 0 ] || die "a pairing code is needed – run this interactively"
  if ! ping -c 1 -t 2 "$host" >/dev/null 2>&1; then
    warn "$host does not answer ping (asleep? other Wi-Fi? stealth-mode firewall?) – trying anyway"
  fi
  while [ $tries -lt 3 ]; do
    read -r -p "6-digit code shown on the other Mac: " code || return 1
    code=$(printf '%s' "$code" | tr -cd '0-9')
    if [ ${#code} != 6 ]; then echo "the code has 6 digits"; continue; fi
    rs pair connect "$host:$port" "$code" && return 0
    tries=$((tries + 1))
  done
  return 1
}

peers=$(rs peers 2>/dev/null)
if [ -n "$peers" ] && [ "$peers" != "not paired" ] && [ $FORCE = 0 ] && [ "$ROLE" = auto ] && [ -z "$PEER" ]; then
  info "already paired: $peers"
  ask "Pair again?" n && paired=0 || paired=1
else
  paired=0
fi

if [ "$paired" = 0 ]; then
  if [ "$ROLE" = auto ] && [ -n "$PEER" ]; then ROLE=connect; fi
  target_host=$PEER_HOST target_port=$((PEER_PORT + 1))
  if [ "$ROLE" != listen ] && [ -z "$PEER" ]; then
    if [ $IS_MAC = 1 ]; then
      info "searching for the other Mac via Bonjour ($WAIT_S s) …"
      found=$(browse)
    else
      found=""
    fi
    n=$(printf '%s' "$found" | grep -c . || true)
    if [ "$n" = 1 ]; then
      target_host=$(echo "$found" | awk '{print $1}'); target_port=$(echo "$found" | awk '{print $2}')
      info "found $(echo "$found" | cut -d' ' -f3-) at $target_host"
      ROLE=connect
    elif [ "$n" -gt 1 ]; then
      echo "several Macs are waiting:"
      echo "$found" | awk '{ printf "  %d) %s (", NR, $1; for (i = 3; i <= NF; i++) printf "%s%s", $i, (i < NF ? " " : ""); print ")" }'
      [ $YES = 0 ] && [ -t 0 ] || die "choose one with --peer HOST"
      read -r -p "number: " k
      line=$(echo "$found" | sed -n "${k}p"); [ -n "$line" ] || die "invalid choice"
      target_host=$(echo "$line" | awk '{print $1}'); target_port=$(echo "$line" | awk '{print $2}')
      ROLE=connect
    elif [ "$ROLE" = connect ]; then
      if [ -t 0 ] && [ $YES = 0 ]; then
        read -r -p "no Mac found – name or IP of the other Mac: " target_host
        [ -n "$target_host" ] || die "no peer given"
      else
        die "no Mac found – pass --peer HOST"
      fi
    else
      info "no Mac is waiting yet – this Mac waits for the other one"
      ROLE=listen
    fi
  fi
  if [ "$ROLE" = listen ]; then
    do_listen || die "pairing failed (code expired or wrong 3 times) – run $PROG again"
  else
    do_connect "$target_host" "$target_port" || die "pairing failed – check the code and run $PROG again"
  fi
fi

# --- 3. start --------------------------------------------------------------
echo
info "done: $ROOT is connected"
if [ "$RUN" = ask ]; then
  if [ $YES = 0 ] && [ -t 0 ] && ask "Start synchronisation now (Ctrl-C stops it)?" y; then RUN=yes; else RUN=no; fi
fi
if [ "$RUN" = yes ]; then
  info "synchronising – stop with Ctrl-C (or '$PROG --stop' in another window)"
  if [ $IS_MAC = 1 ] && [ "${TERM_PROGRAM:-}" != Apple_Terminal ]; then
    warn "not running in Terminal.app (${TERM_PROGRAM:-unknown}): macOS may block the local network ('No route to host')"
  fi
  REPOSYNC_LOG=${REPOSYNC_LOG:-info} exec "$RS_BIN" --config "$CONFIG" daemon
fi
echo "start later with: reposync --config $CONFIG daemon   (in Terminal.app, stop: Ctrl-C)"
echo "check with      : reposync status"
