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
when Windows boots or change Windows power settings. Re-run the same command to
update; your configuration and logs are preserved. Add `~/.local/bin` to your
shell's `PATH` if the installer prompts you.

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
t3-keep-awake on [DURATION] # keep Windows awake regardless of T3
t3-keep-awake off           # back to automatic
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

### Always on

`t3-keep-awake on` keeps Windows awake whatever T3 is doing, until
`t3-keep-awake off`. It is saved in `~/.local/state/t3-keep-awake/always-on.json`,
so it survives `restart`, WSL restarts, and reboots (once WSL is running again).
`t3-keep-awake on 2h` (or `90m`, `1h30m`, `1d`, up to 366 days) ends by itself
after that long, including if the time runs out while the daemon is stopped. The
end time is a wall-clock time, so if WSL's clock falls behind after Windows
sleeps, a timed hold can run long by the same amount. Running `on` again
replaces the previous setting. `off` returns to holding only while T3 threads
work. `stop` still releases everything until the next `start`.

Always-on uses the same keeper as automatic holds, so the display can still turn
off and lock, and the keeper still lets go if the daemon dies. `status` shows the
mode, and how long a timed hold has left.

### MCP

`t3-keep-awake mcp` is a stdio MCP server with three tools: `status`,
`always_on` (optional `duration`, such as `"2h"`), and `always_off`. It talks to
the running daemon: `always_on` and `always_off` start it if needed, and
`status` reports an error if it is not running. For example, in a Claude Code
`.mcp.json`:

```json
{
  "mcpServers": {
    "keep-awake": { "command": "/home/you/.local/bin/t3-keep-awake", "args": ["mcp"] }
  }
}
```

or in Codex's `config.toml`:

```toml
[mcp_servers.keep-awake]
command = "/home/you/.local/bin/t3-keep-awake"
args = ["mcp"]
```

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
| `t3_data_dir` | `~/.t3/userdata` | Absolute path to T3's WSL userdata directory containing `statev2.sqlite` (or `state.sqlite`) and `server-runtime.json`. |
| `poll_secs` | `5` | Poll interval while T3 is running, a hold is wanted, or a read failed. Must be below 60 seconds. |
| `idle_poll_secs` | `5` | How often to look for a returning T3 server. |
| `grace_secs` | `30` | Hold this long after the last active turn, or a database read error. Read errors never hold more than 60 seconds past the last good read. Set to `0` for immediate release. |
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
owns T3's listening socket and responds to a bounded HTTP request on the host it
advertises, or on loopback when T3 listens on every address.

It then reads whichever T3 database that server process has open:
`statev2.sqlite` (T3's v2 orchestrator) or, for older T3 releases,
`state.sqlite`. T3 leaves the old file behind when it upgrades, still readable
but frozen, so the server's open files rather than which files exist decide. If
the server has neither open, T3 has moved its database: that is an error in
`status` and the log, not a quiet "no threads working". Likewise, if
`server-runtime.json` is missing but a recently written T3 database is still
open in some process, `status` warns that T3 may have moved its runtime file.

The database is opened **read-only**, preserving WAL visibility. With
`statev2.sqlite`, a thread counts while its run is running, the run's active
attempt is running, and that attempt has a running provider turn started during
this server's lifetime. A pending command, file, or permission approval marks
the run as blocked. Queued runs, runs finalizing after their turn completed,
deleted threads, and turns left over from before a server restart do not count.
With `state.sqlite`, a turn must be running in both the session and current-turn
projections, with a matching provider active-turn ID and a provider-runtime
observation from this server's lifetime.

This has been checked against T3 Code server 0.1.7 (`statev2.sqlite` migration
79) and T3 Code 0.0.45 (`state.sqlite` migration 54). The database projections
are an internal T3 interface, so a future schema change may require updating the
reader. Missing tables or columns, unsupported metadata, and read failures are
errors: they are reported in `status` and the log, and they cannot keep
extending a hold indefinitely. A newer T3 migration is only a warning, because
most migrations do not touch these projections, but working turns may go
undetected until the reader is checked against it.
