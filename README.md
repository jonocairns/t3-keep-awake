# herdr-keep-awake

Keeps Windows from sleeping while any [herdr](https://herdr.dev) agent is
working inside WSL, and lets it sleep again once they're all done.

```
herdr events ──nudge──┐
                      ▼
poll every 10s ──► daemon (one per WSL distro, flock)
                    hold = any agent working, or one was within the grace period
                      │ stdin heartbeat every 15s
                      ▼
               powershell.exe keeper: SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
               lets go when stdin closes, or after 60s without a heartbeat
```

The screen still turns off and locks as usual; only sleep is blocked (set
`keep_display_on` to keep the display on too). Closing a laptop lid still
follows your Windows lid setting.

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

## Use

It runs by itself. To see what it's doing:

```sh
herdr-keep-awake status   # held or not, why, which agents, keeper health
herdr-keep-awake log 30   # state transitions, keeper restarts, herdr errors
herdr-keep-awake probe    # Windows' own view: is anything blocking sleep?
```

In herdr, the **Keep awake: status** action opens the same status as a popup.

`probe` reads the system-wide execution state with `CallNtPowerInformation`,
so it needs no admin rights (unlike `powercfg /requests`). It reports every
Windows process's requests, not only this plugin's.

## Configure

`~/.config/herdr-keep-awake/config.toml` (all optional; unknown keys are
rejected). Run `herdr-keep-awake restart` after editing.

| Key | Default | Meaning |
|---|---|---|
| `grace_secs` | `300` | Keep holding this long after the last working agent. Bridges gaps between turns, and covers a briefly unreadable herdr. |
| `hold_blocked` | `false` | Whether an agent waiting on a permission prompt keeps Windows awake. |
| `max_working_secs` | `28800` | One uninterrupted working stretch longer than this is treated as a stuck status and stops counting. |
| `keep_display_on` | `false` | Also keep the display on. |
| `poll_secs` | `10` | How often herdr is read when no event arrives. |
| `heartbeat_secs` | `15` | How often the daemon pings the keeper. |
| `keeper_timeout_secs` | `60` | The keeper lets go after this long without a ping. Must be at least 2x `heartbeat_secs`. |
| `idle_exit_secs` | `600` | The daemon exits after this long with no running herdr server; the next herdr hook starts it again. |

## How it stays reliable

- **Hooks never decide anything.** Each herdr event only nudges the daemon,
  starting it if it isn't running. The daemon then reads every agent in every
  running herdr session and works out the answer from scratch, so concurrent,
  duplicated, or missed events can't leave it in the wrong state. It also
  polls every 10s regardless.
- **One daemon.** A lock file decides which process wins when hooks race to
  start it; the rest exit quietly.
- **Stuck on is harder than stuck off.** If the daemon dies, the keeper's
  stdin closes and it lets go. If the daemon or WSL hangs, the heartbeats stop
  and the keeper lets go after 60s. If herdr can't be read, the hold lasts
  only through the grace period. An agent stuck showing `working` stops
  counting after `max_working_secs`.
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
