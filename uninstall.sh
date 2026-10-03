#!/usr/bin/env bash
# Remove the user service and CLI link; keep configuration and logs.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/target/release/t3-keep-awake"
CONFIG_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}"

systemctl --user disable --now t3-keep-awake.service
if [ -x "$BIN" ]; then
  "$BIN" stop
fi
rm -f "$CONFIG_ROOT/systemd/user/t3-keep-awake.service"
systemctl --user daemon-reload
if [ "$(readlink "$HOME/.local/bin/t3-keep-awake" 2>/dev/null)" = "$BIN" ]; then
  rm "$HOME/.local/bin/t3-keep-awake"
fi
echo "uninstalled"
