# t3-keep-awake

Keep Windows awake while T3 Code is working.

Run it in WSL and your computer stays awake while any T3 thread is running an
AI turn. Once the work finishes, Windows returns to its normal sleep settings.
Your screen can still turn off and lock.

No Herdr, plugins, or changes to T3 required. It starts automatically with WSL
and runs quietly in the background.

## Install

Run inside **x86_64 WSL2 with systemd enabled**:

```sh
curl -fsSL https://raw.githubusercontent.com/jonocairns/t3-keep-awake/main/install.sh | sh
```

Re-run the same command to update. No Rust toolchain required.
For a private repository, use the [authenticated installer](docs/usage.md#install).

## Controls

```sh
t3-keep-awake status   # see what is keeping Windows awake
t3-keep-awake stop     # stop keeping Windows awake
t3-keep-awake start    # start again
```

[Configuration, uninstalling, and development](docs/usage.md).

MIT licensed. Derived from [herdr-keep-awake](https://github.com/jonocairns/herdr-keep-awake).
