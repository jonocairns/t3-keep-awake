#!/bin/sh
# Remove the user service and binary; keep configuration, logs, and user linger.
set -eu

NAME="t3-keep-awake"
BIN="$HOME/.local/bin/$NAME"
UNIT="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$NAME.service"

main() {
    if [ -f "$UNIT" ]; then
        systemctl --user disable --now "$NAME.service"
    fi
    if [ -x "$BIN" ]; then
        "$BIN" stop
    fi
    rm -f "$UNIT" "$BIN"
    systemctl --user daemon-reload
    echo "uninstalled (configuration, logs, and user linger retained)"
}

main "$@"
