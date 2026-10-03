# Usage and development

Keeps Windows awake while any T3 Code thread is actively running an AI turn in
WSL. Once all turns finish, Windows returns to its normal sleep policy after a
30-second grace period. T3 being open, an idle agent process, or a saved thread
by itself does not keep the machine awake.

The display can turn off and lock normally. Closing the laptop lid or explicitly
putting Windows to sleep still follows Windows' own settings.

## Install

Requires x86_64 WSL2 with systemd, a working user service manager, and Windows
interop (`powershell.exe`). T3 Code must run its agents in the same WSL distro.
The release binary is static and includes SQLite; Rust and a C compiler are
not required.

```sh
curl -fsSL https://raw.githubusercontent.com/jonocairns/t3-keep-awake/main/install.sh | sh
```

The installer downloads the latest release, verifies its SHA-256 checksum,
installs it in `~/.local/bin`, and enables and starts `t3-keep-awake.service` in
your systemd user manager. It enables user linger so the service starts when
this WSL distro starts and survives closing terminals. It does not start WSL
when Windows boots or change Windows power
settings. Re-run the same command to update; your configuration and logs are
preserved. Add `~/.local/bin` to your shell's `PATH` if the installer prompts you.

To install a specific release, pass `T3_KEEP_AWAKE_VERSION=v0.1.0` to `sh`.

### Build from source

For development or other WSL architectures, install the Rust toolchain pinned
in `rust-toolchain.toml` and a C compiler, then run from a checkout:

```sh
cargo build --release --locked
T3_KEEP_AWAKE_BINARY="$PWD/target/release/t3-keep-awake" sh ./install.sh
```

## Use

```sh
t3-keep-awake status        # desired hold, confirmed keeper, active turns, errors
t3-keep-awake status --json # the same state as JSON
t3-keep-awake log 30        # recent transitions and errors
t3-keep-awake probe         # Windows' system-wide view, no administrator needed
t3-keep-awake stop          # stop the service and release its sleep request
t3-keep-awake start         # start the service again
t3-keep-awake restart       # apply configuration or binary changes
```

Installed lifecycle commands use systemd so the daemon remains supervised. To
run without installation, use `cargo run -- daemon` in the foreground.
`T3_KEEP_AWAKE_STATE_DIR` selects isolated/manual state and disables service
routing for lifecycle commands.

The probe reports the union of all Windows processes' power requests. Its output
alone does not prove which process is holding Windows awake; `status` reports
the keeper's confirmation and its Windows PID.

Logs are written to `~/.local/state/t3-keep-awake/daemon.log` and rotated at about
1 MiB, retaining one previous archive. Startup failures are also visible with:

```sh
journalctl --user -u t3-keep-awake.service
```

## Configure

Create `~/.config/t3-keep-awake/config.toml` to override defaults. All keys are
optional; unknown keys are rejected. Run `t3-keep-awake restart` after edits.

| Key | Default | Meaning |
| --- | --- | --- |
| `t3_data_dir` | `~/.t3/userdata` | Absolute path to T3's WSL userdata directory containing `state.sqlite` and `server-runtime.json`. |
| `poll_secs` | `5` | Poll interval while T3 is running, a hold is wanted, or a read failed. Must be below 60 seconds. |
| `idle_poll_secs` | `5` | How often to look for a returning T3 server. |
| `grace_secs` | `30` | Hold this long after the last active turn, or a database read error. Set to `0` for immediate release. |
| `hold_blocked` | `false` | Count a running turn waiting on a permission approval. Async user questions alone do not classify a turn as blocked. |
| `max_working_secs` | `28800` | Treat one turn running longer than eight hours as stuck. A new turn resets this timer. |
| `keep_display_on` | `false` | Also keep the display on. |
| `heartbeat_secs` | `15` | How often the daemon pings the Windows keeper. |
| `keeper_timeout_secs` | `60` | Release if no heartbeat arrives; must be at least twice `heartbeat_secs`. |

Example:

```toml
grace_secs = 30
hold_blocked = true
# Use an absolute path; TOML paths do not expand ~ or environment variables.
# t3_data_dir = "/home/you/.t3/userdata"
```

`T3_KEEP_AWAKE_DATA_DIR` overrides the T3 path for a manual process;
`T3_KEEP_AWAKE_CONFIG` overrides the config file. Installed services should use
the configuration file rather than shell environment overrides.

## Activity and recovery

Every poll reads a full snapshot; it does not depend on receiving a particular
start or finish event. A singleton lock prevents duplicate controllers.

Before reading activity, the daemon checks that the PID in `server-runtime.json`
owns T3's listening socket and responds to a bounded HTTP request. It then opens
`state.sqlite` **read-only**, preserving WAL visibility, and joins current thread,
session, provider-runtime, and turn state. A turn must be running in both the
session and current-turn projections, with a matching provider active-turn ID
and a provider-runtime observation from this server's lifetime. Deleted threads,
old turns, and idle provider processes do not count.

This has been checked against T3 Code 0.0.45 and T3 database migration 54. The
database projections are an internal T3 interface, so a future schema change may
require updating the reader. Missing tables or columns, unsupported metadata,
and read failures are errors: they are reported in `status` and the log, and
they cannot keep extending a hold indefinitely. A newer T3 migration is only a
warning, because most migrations do not touch these projections, but working
turns may go undetected until the reader is checked against it.

A stopped or unresponsive server releases immediately on the next poll, even
if its database still says a turn is running. Other read failures can preserve
an existing hold only through the grace period. A snapshot older than 60 seconds
cannot authorize a hold. Each turn has a stable identity so repeated polls do
not reset its stuck timer.

The Windows keeper uses `SetThreadExecutionState` from one long-lived
PowerShell process. It releases when stdin closes, the process exits, or a
heartbeat is missing for 60 seconds. Keeper failures retry with bounded backoff.
The daemon resolves WSL's init interop socket for each Windows invocation, so it
does not need a terminal's `WSL_INTEROP` environment.

Only this configured WSL server is watched. Work hosted on another machine does
not need to keep this Windows host awake.

## Remove

```sh
curl -fsSL https://raw.githubusercontent.com/jonocairns/t3-keep-awake/main/uninstall.sh | sh
```

You can also run `sh ./uninstall.sh` from a checkout. This stops and disables
the service, releases the keeper, and removes the unit and binary.
Configuration, logs, and user linger remain. Linger is shared by
all your user services; if you no longer need it, disable it separately with
`loginctl disable-linger "$(id -un)"`.

## Develop

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
python3 tests/installer.py
```

Tests use isolated T3 SQLite fixtures, real loopback HTTP listeners, and fake
keepers. They cover active and idle turns, concurrent threads, permission
prompts, old provider observations, incompatible schemas, server disappearance,
HTTP liveness, controller crashes, and keeper recovery without changing Windows
power state.

Installer tests use temporary homes, fake release assets, and mocked Windows
and systemd commands. They cover downloads, checksum failures, upgrades from
the old symlink install, private repositories, source installs, and removal.

## Releases

After updating the Cargo package version and lockfile, push a matching `vX.Y.Z`
tag on `main`. The release workflow runs checks and tests, builds a static
x86_64 Linux binary, and publishes it with a SHA-256 checksum. Installers resolve
the latest tag once, then download both assets from that same release.

## Licence

[MIT](../LICENSE).
