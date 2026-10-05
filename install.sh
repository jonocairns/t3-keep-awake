#!/bin/sh
# Self-contained for curl | sh; installs a verified release and WSL user service.
set -eu

REPO="jonocairns/t3-keep-awake"
NAME="t3-keep-awake"

err() { printf '%s\n' "$NAME installer: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "requires $1"; }

download() {
    curl -fsSL --retry 3 --connect-timeout 10 --max-time 120 "$1" -o "$2"
}

main() {
    [ "$(uname -s)" = Linux ] || err "run this installer inside WSL2"
    case "$(uname -r)" in
        *[Mm]icrosoft*|*WSL*) ;;
        *) err "run this installer inside WSL2" ;;
    esac
    for dependency in systemctl loginctl powershell.exe mktemp sha256sum; do
        need "$dependency"
    done
    systemctl --user show-environment >/dev/null 2>&1 \
        || err "a running systemd user manager is required; enable systemd in WSL first"

    bin_dir="$HOME/.local/bin"
    unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    tmp_dir="$(mktemp -d)"
    staged_bin=""
    trap 'rm -rf "$tmp_dir"; if [ -n "$staged_bin" ]; then rm -f "$staged_bin"; fi' 0
    trap 'exit 1' HUP INT TERM
    binary="$tmp_dir/$NAME"

    if [ -n "${T3_KEEP_AWAKE_BINARY:-}" ]; then
        # Development installs share the same service setup as release installs.
        cp "$T3_KEEP_AWAKE_BINARY" "$binary"
    else
        case "$(uname -m)" in
            x86_64|amd64) asset="$NAME-linux-x86_64" ;;
            *) err "prebuilt releases currently support x86_64 WSL; see the README for building from source" ;;
        esac
        version="${T3_KEEP_AWAKE_VERSION:-}"
        if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
            if [ -z "$version" ]; then
                version="$(gh release view --repo "$REPO" --json tagName --jq .tagName)"
            fi
            printf 'Downloading %s %s...\n' "$NAME" "$version"
            gh release download "$version" --repo "$REPO" --dir "$tmp_dir" \
                --pattern "$asset" --pattern "$asset.sha256"
        else
            need curl
            if [ -z "$version" ]; then
                release_url="$(curl -fsSL --retry 3 --connect-timeout 10 --max-time 30 \
                    -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
                    || err "cannot find a release; for a private repo, sign in with gh auth login"
                version="${release_url##*/}"
            fi
            case "$version" in
                v[0-9]*) ;;
                *) err "expected a release tag such as v0.1.0" ;;
            esac
            printf 'Downloading %s %s...\n' "$NAME" "$version"
            release_url="https://github.com/$REPO/releases/download/$version"
            download "$release_url/$asset" "$tmp_dir/$asset"
            download "$release_url/$asset.sha256" "$tmp_dir/$asset.sha256"
        fi
        # Check only the requested asset, never paths supplied by a manifest.
        expected="$(awk -v asset="$asset" '$2 == asset { print $1 }' "$tmp_dir/$asset.sha256")"
        [ "${#expected}" -eq 64 ] || err "release checksum is missing or invalid"
        case "$expected" in *[!0-9a-fA-F]*) err "invalid release checksum" ;; esac
        actual="$(sha256sum "$tmp_dir/$asset" | awk '{ print $1 }')"
        [ "$actual" = "$expected" ] || err "release checksum did not match; existing installation was left untouched"
        mv "$tmp_dir/$asset" "$binary"
    fi
    chmod 755 "$binary"
    "$binary" help >/dev/null

    # Check linger permission and stage the binary before stopping a working install.
    loginctl --no-ask-password enable-linger "$(id -un)"
    mkdir -p "$bin_dir" "$unit_dir"
    staged_bin="$(mktemp "$bin_dir/.$NAME.XXXXXX")"
    cp "$binary" "$staged_bin"
    chmod 755 "$staged_bin"
    "$binary" stop
    # Rename replaces the old checkout symlink without overwriting its target.
    mv -f "$staged_bin" "$bin_dir/$NAME"
    staged_bin=""

    cat > "$unit_dir/$NAME.service" <<'UNIT'
[Unit]
Description=Keep Windows awake while T3 Code threads are working

[Service]
Type=simple
ExecStart=%h/.local/bin/t3-keep-awake daemon
Restart=on-failure
RestartSec=5
# Signal only the daemon, so it releases its keeper itself before exiting.
KillMode=mixed
TimeoutStopSec=15

[Install]
WantedBy=default.target
UNIT

    systemctl --user daemon-reload
    systemctl --user enable "$NAME.service"
    systemctl --user restart "$NAME.service"
    attempt=0
    while [ "$attempt" -lt 50 ]; do
        if "$bin_dir/$NAME" status 2>/dev/null; then
            printf '\nInstalled and started %s. Re-run this installer to update.\n' "$NAME"
            # Print variables literally for the user's shell to expand later.
            # shellcheck disable=SC2016
            case ":$PATH:" in
                *":$bin_dir:"*) ;;
                *) printf 'Add ~/.local/bin to your PATH: export PATH="$HOME/.local/bin:$PATH"\n' ;;
            esac
            return
        fi
        attempt=$((attempt + 1))
        sleep 0.1
    done
    err "service did not start; check journalctl --user -u $NAME.service"
}

main "$@"
