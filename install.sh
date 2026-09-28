#!/usr/bin/env bash
# Build herdr-keep-awake, link it into herdr, and (re)start the daemon.
# Safe to re-run after pulling: restart picks up the rebuilt binary.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/target/release/herdr-keep-awake"

cargo build --release --manifest-path "$ROOT/Cargo.toml"

if herdr plugin list 2>/dev/null | grep -q '^- herdr-keep-awake '; then
  echo "herdr plugin already linked"
else
  herdr plugin link "$ROOT"
fi

mkdir -p "$HOME/.local/bin"
ln -sf "$BIN" "$HOME/.local/bin/herdr-keep-awake"

# Startup hooks run only when a herdr server starts, so start the daemon now.
"$BIN" restart
"$BIN" status
