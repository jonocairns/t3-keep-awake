#!/usr/bin/env bash
# Build and install an independent WSL user service. Safe to re-run after pulling.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/target/release/t3-keep-awake"
CONFIG_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}"
UNIT_DIR="$CONFIG_ROOT/systemd/user"

cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"
# Release any manual instance or stop the previous managed service first.
"$BIN" stop
mkdir -p "$HOME/.local/bin" "$UNIT_DIR"
ln -sf "$BIN" "$HOME/.local/bin/t3-keep-awake"

# %h is systemd's home-directory specifier, not shell expansion.
cat > "$UNIT_DIR/t3-keep-awake.service" <<'UNIT'
[Unit]
Description=Keep Windows awake while T3 Code threads are working

[Service]
Type=simple
ExecStart=%h/.local/bin/t3-keep-awake daemon
Restart=on-failure
RestartSec=5
TimeoutStopSec=15

[Install]
WantedBy=default.target
UNIT

systemctl --user daemon-reload
loginctl --no-ask-password enable-linger "$(id -un)"
systemctl --user enable t3-keep-awake.service
systemctl --user restart t3-keep-awake.service

for _attempt in {1..50}; do
  if "$BIN" status; then
    exit 0
  fi
  sleep 0.1
done
echo "The service did not start. Check: journalctl --user -u t3-keep-awake.service" >&2
exit 1
