# t3-keep-awake

Keep Windows awake while T3 Code runs AI turns in WSL. When the work finishes,
Windows returns to its normal sleep settings. Your screen can still turn off
and lock.

It starts automatically with WSL and runs quietly in the background.

## Install

Run inside **x86_64 WSL2 with systemd enabled**:

```sh
curl -fsSL https://raw.githubusercontent.com/jonocairns/t3-keep-awake/main/install.sh | sh
```

The installer starts the service automatically. Re-run the same command to
update.

## Controls

```sh
t3-keep-awake status   # see what is keeping Windows awake
t3-keep-awake stop     # stop keeping Windows awake
t3-keep-awake start    # start again
```

[Configuration, uninstalling, and development](docs/usage.md).

[MIT licence](LICENSE).
