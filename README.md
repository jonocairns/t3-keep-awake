# herdr-keep-awake

Keeps Windows from sleeping while any [herdr](https://herdr.dev) agent is
working inside WSL, and lets it sleep again once they're all done.

```
herdr events ──nudge──┐
                      ▼
periodic poll ─────► WSL daemon (one per distro, flock)
                     full herdr snapshot → desired hold
                     10s polling while active; 60s with no server and no hold
                     a snapshot can authorize a hold for at most 60s
                       │ stdin heartbeat every 15s while holding
                       ▼
                     powershell.exe keeper: SetThreadExecutionState
                     releases on stdin close or after 60s without a heartbeat
```

The screen still turns off and locks as usual; only sleep is blocked (set
`keep_display_on` to keep the display on too). Closing a laptop lid still
follows your Windows lid setting.

The plugin uses herdr's reported agent status as its source of truth. If an
agent appears to be in the wrong state, `herdr agent explain <pane>` shows how
herdr classified it.

## Install

Requires WSL with Windows interop (`powershell.exe` reachable), herdr 0.9.0+,
and the Rust toolchain pinned in `rust-toolchain.toml`.

```sh
git clone <this repo> ~/herdr-keep-awake
cd ~/herdr-keep-awake
./install.sh
```

`install.sh` builds, links the plugin into herdr, symlinks the CLI into
`~/.local/bin`, and (re)starts the daemon. Re-run it after pulling.
`./uninstall.sh` stops the daemon (releasing any hold) and unlinks the plugin.
The daemon stays running while idle so it can discover a Herdr server that
returns without another hook event.

## Use

It runs by itself. To see what it's doing:

```sh
herdr-keep-awake status   # held or not, why, which agents, keeper health
herdr-keep-awake log 30   # state transitions, keeper restarts, herdr errors
herdr-keep-awake probe    # Windows' own view: is anything blocking sleep?
herdr-keep-awake restart  # apply config or binary changes
herdr-keep-awake stop     # release the hold and stop until a hook or restart
```

In herdr, the **Keep awake: status** action opens the same status as a popup.
Just after startup, status may briefly say `reading herdr` before the first
snapshot finishes.

`probe` reads the system-wide execution state with `CallNtPowerInformation`,
so it needs no admin rights (unlike `powercfg /requests`). It reports every
Windows process's requests, not only this plugin's.

The daemon logs state changes and errors to `~/.local/state/herdr-keep-awake/daemon.log`.
It rotates on the next daemon log entry after about 1 MiB, retaining one
`daemon.log.1` archive. `log` reads the current file.

## Configure

`~/.config/herdr-keep-awake/config.toml` (all optional; unknown keys are
rejected). Run `herdr-keep-awake restart` after editing.

| Key | Default | Meaning |
|---|---|---|
| `grace_secs` | `300` | Keep holding this long after the last working agent. Bridges gaps between turns, and covers a briefly unreadable herdr. |
| `hold_blocked` | `false` | Whether an agent waiting on a permission prompt keeps Windows awake. |
| `max_working_secs` | `28800` | One uninterrupted working stretch longer than this is treated as a stuck status and stops counting. Its timer restarts with the daemon. |
| `keep_display_on` | `false` | Also keep the display on. |
| `poll_secs` | `10` | How often herdr is read while a session runs, a hold is wanted, or a read failed. Must be less than 60s so a working snapshot stays fresh. |
| `idle_poll_secs` | `60` | How often to look for a herdr server when none is running and no hold is wanted. |
| `heartbeat_secs` | `15` | How often the daemon pings the keeper. |
| `keeper_timeout_secs` | `60` | The keeper lets go after this long without a ping. Must be at least 2x `heartbeat_secs` and fit PowerShell's millisecond timeout. |

The old `idle_exit_secs` key is accepted for existing configs but has no effect.

## How it stays reliable

- **Hooks never decide anything.** Each herdr event only nudges the daemon,
  starting it if it isn't running. The daemon then reads every agent in every
  running herdr session and works out the answer from scratch, so concurrent,
  duplicated, or missed events can't leave it in the wrong state. It also
  polls every 10s by default while a server runs, a hold is wanted, or a read
  failed. After a readable zero-session snapshot and with no hold, it checks
  every 60s by default so it can discover a server without another hook.
- **One daemon.** A lock file decides which process wins when hooks race to
  start it; the rest exit quietly.
- **Stuck on is harder than stuck off.** If the daemon dies, the keeper's
  stdin closes and it lets go. If the daemon or WSL hangs, the heartbeats stop
  and the keeper lets go after 60s. A readable working agent still counts if
  another session fails to read; with none confirmed, a previous hold lasts
  only through the grace period. An agent stuck showing `working` stops
  counting after `max_working_secs`. A snapshot older than 60s cannot keep
  renewing the hold. Each herdr CLI call has a 5s timeout, and a full snapshot
  has a 30s budget.
- **Herdr updates are tolerated.** If a hook supplies a path to a Herdr binary
  that has since been removed, the daemon uses `herdr` from `PATH` on its next
  read. A closed or incomplete daemon socket reply is treated as an error, so
  a hook can restart the daemon.
- **Keeper failures are retried** with backoff from 1s up to 60s, and a keeper
  that stops answering heartbeats is killed and replaced. Killing the WSL-side
  `powershell.exe` process also ends the Windows process.

## Develop

```sh
cargo test                                   # unit + end-to-end tests
cargo clippy --all-targets && cargo fmt --check
```

The end-to-end tests in `tests/daemon.rs` drive the real binary against a
fake `herdr` (`HERDR_BIN_PATH`) and a fake keeper (`HERDR_KEEP_AWAKE_KEEPER`)
in an isolated state dir (`HERDR_KEEP_AWAKE_STATE_DIR`), so they never touch
Windows or your live daemon.
