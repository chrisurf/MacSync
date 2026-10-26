#!/usr/bin/env bash
# Reports available updates for everything MacSync depends on:
#   1. the pinned Rust toolchain (rust-toolchain.toml) vs. the latest stable
#   2. crates: compatible updates (Cargo.lock) and new major versions
#   3. known security advisories (RustSec, via cargo-deny if installed)
# Read-only. Exit code 0 = up to date, 10 = updates available.
#
# Usage: scripts/check-updates.sh  (or: make outdated)
set -uo pipefail
cd "$(dirname "$0")/.."

status=0
section() { printf '\n\033[1m%s\033[0m\n' "$*"; }

section "Rust toolchain"
pinned=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
latest=$(curl -fsSL --max-time 15 https://static.rust-lang.org/dist/channel-rust-stable.toml 2>/dev/null \
  | awk '/^\[pkg.rust\]/{f=1} f && /^version *=/{print; exit}' | sed -E 's/^version *= *"([0-9.]+).*/\1/')
if [ -z "$latest" ]; then
  echo "  could not determine the latest stable Rust (offline?) – pinned: $pinned"
elif [ "$latest" = "$pinned" ]; then
  echo "  pinned $pinned is the latest stable"
else
  echo "  pinned $pinned, latest stable $latest → update rust-toolchain.toml (make update-toolchain)"
  status=10
fi

section "Crates (compatible updates within Cargo.toml requirements)"
out=$(cargo update --dry-run --verbose 2>&1)
compatible=$(printf '%s\n' "$out" | grep -E '^\s+Updating ' | grep -v 'crates.io index' | sort -u)
if [ -n "$compatible" ]; then
  printf '%s\n' "$compatible"
  echo "  → make update-deps"
  status=10
else
  echo "  Cargo.lock is up to date"
fi

section "Crates (new major versions, need a Cargo.toml change)"
major=$(printf '%s\n' "$out" | grep -E 'Unchanged .*\(available' | sort -u)
if [ -n "$major" ]; then printf '%s\n' "$major"; else echo "  none"; fi

section "Security advisories (RustSec)"
if command -v cargo-deny >/dev/null; then
  if cargo deny --log-level error check advisories; then echo "  no known vulnerabilities"; else status=1; fi
else
  echo "  cargo-deny not installed → make tools"
fi

exit $status
