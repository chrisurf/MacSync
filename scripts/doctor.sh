#!/usr/bin/env bash
# Checks everything needed to build and run RepoSync on this machine and
# explains how to fix what is missing. Read-only: changes nothing.
#
# Usage: scripts/doctor.sh  (or: make doctor)
set -uo pipefail

BINDIR="${BINDIR:-$HOME/.local/bin}"
CONFIG="${CONFIG:-$HOME/.config/reposync/config.toml}"
cd "$(dirname "$0")/.."

ok=0 warn=0 bad=0
pass() { printf '  \033[32m✔\033[0m %s\n' "$*"; ok=$((ok + 1)); }
note() { printf '  \033[33m!\033[0m %s\n' "$*"; warn=$((warn + 1)); }
fail() { printf '  \033[31m✘\033[0m %s\n' "$*"; bad=$((bad + 1)); }
hint() { printf '      → %s\n' "$*"; }
section() { printf '\n\033[1m%s\033[0m\n' "$*"; }

IS_MAC=0; [ "$(uname -s)" = Darwin ] && IS_MAC=1

section "System"
if [ $IS_MAC = 1 ]; then
  pass "macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion)), $(uname -m)"
  if xcode-select -p >/dev/null 2>&1; then
    pass "Xcode Command Line Tools: $(xcode-select -p)"
  else
    fail "Xcode Command Line Tools missing (needed as C compiler/linker)"
    hint "xcode-select --install"
  fi
else
  pass "$(uname -sr) $(uname -m) (development/CI platform)"
fi

section "Tools"
if command -v git >/dev/null; then pass "$(git --version)"; else fail "git missing"; hint "xcode-select --install"; fi
if command -v rustup >/dev/null; then
  pass "rustup $(rustup --version 2>/dev/null | head -1 | awk '{print $2}')"
  pinned=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
  if rustup toolchain list | grep -q "^$pinned"; then
    pass "pinned Rust toolchain $pinned installed"
  else
    note "pinned Rust toolchain $pinned not installed yet (cargo installs it automatically on first build)"
    hint "make toolchain"
  fi
else
  fail "rustup/cargo missing"
  hint "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # then open a new Terminal"
fi
if command -v python3 >/dev/null; then pass "python3 (only needed for make e2e)"; else note "python3 missing (only needed for make e2e)"; fi

section "Repository"
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null)
head=$(git rev-parse --short=10 HEAD 2>/dev/null)
pass "branch $branch at $head"
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then note "working tree has local changes"; fi
if git rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1; then
  if git fetch -q 2>/dev/null; then
    behind=$(git rev-list --count 'HEAD..@{u}')
    if [ "$behind" = 0 ]; then pass "up to date with $(git rev-parse --abbrev-ref '@{u}')"; else
      note "$behind commit(s) behind $(git rev-parse --abbrev-ref '@{u}')"; hint "make update"; fi
  else
    note "could not fetch from remote (offline?)"
  fi
fi

section "Installation"
if [ -x "$BINDIR/reposync" ]; then
  v=$("$BINDIR/reposync" --version 2>/dev/null)
  if printf '%s' "$v" | grep -q "$head"; then pass "$BINDIR/reposync is current: $v"; else
    note "$BINDIR/reposync is not built from HEAD ($v)"; hint "make install"; fi
else
  note "reposync not installed in $BINDIR"; hint "make install"
fi
case ":$PATH:" in
  *":$BINDIR:"*) pass "$BINDIR is in PATH" ;;
  *) note "$BINDIR is not in PATH"; hint "echo 'export PATH=\"$BINDIR:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
esac

section "Configuration"
if [ -f "$CONFIG" ]; then
  pass "config $CONFIG"
  root=$(sed -n 's/^root *= *"\(.*\)"/\1/p' "$CONFIG")
  root=${root/#\~/$HOME}
  if [ -d "$root" ]; then pass "root $root exists"; else note "root $root does not exist yet"; fi
  case "$root" in
    "$HOME/Desktop"*|"$HOME/Documents"*|"$HOME/Downloads"*|"$HOME/Library/Mobile Documents"*)
      note "root is in a privacy-protected or iCloud folder"
      hint "macOS will ask Terminal for access; for the first test prefer ~/RepoSyncTest" ;;
  esac
  if [ -x "$BINDIR/reposync" ]; then
    peers=$("$BINDIR/reposync" --config "$CONFIG" peers 2>/dev/null)
    if [ -n "$peers" ] && [ "$peers" != "not paired" ]; then pass "paired: $peers"; else note "not paired yet"; hint "make pair-listen (one Mac) / make pair-connect HOST=… CODE=… (other Mac)"; fi
  fi
else
  note "no configuration yet"; hint "make init"
fi

section "Network"
if [ $IS_MAC = 1 ]; then
  pass "Bonjour name: $(scutil --get LocalHostName 2>/dev/null).local"
  for i in en0 en1; do
    ip=$(ipconfig getifaddr "$i" 2>/dev/null) && [ -n "$ip" ] && pass "IPv4 on $i: $ip"
  done
  fw=$(/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate 2>/dev/null)
  case "$fw" in
    *enabled*|*"State = 1"*) note "macOS firewall is on: allow incoming connections for reposync when asked"
       hint "or: sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add $BINDIR/reposync --unblockapp $BINDIR/reposync" ;;
    *) pass "macOS firewall off (no prompt for incoming connections)" ;;
  esac
  if [ "${TERM_PROGRAM:-}" = "Apple_Terminal" ]; then
    pass "running in Terminal.app (exempt from the Local Network privacy check)"
  else
    note "not running in Terminal.app (${TERM_PROGRAM:-unknown})"
    hint "for the test, start the daemon in Terminal.app: other terminal apps need their own Local Network permission"
  fi
else
  ip=$(hostname -I 2>/dev/null | awk '{print $1}'); [ -n "$ip" ] && pass "IPv4: $ip"
fi

printf '\n%d ok, %d hints, %d problems\n' "$ok" "$warn" "$bad"
[ "$bad" = 0 ]
