#!/usr/bin/env bash
# Stop the daemon (releasing any hold), unlink the plugin, remove the CLI link.
# Config and logs under ~/.config and ~/.local/state are left in place.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/target/release/herdr-keep-awake"

if [ -x "$BIN" ]; then
  "$BIN" stop
fi
herdr plugin unlink herdr-keep-awake || true
if [ "$(readlink "$HOME/.local/bin/herdr-keep-awake" 2>/dev/null)" = "$BIN" ]; then
  rm "$HOME/.local/bin/herdr-keep-awake"
fi
echo "uninstalled"
