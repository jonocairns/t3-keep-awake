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
t3-keep-awake on       # keep Windows awake all the time, until `off`
t3-keep-awake on 2h    # ...or just for a while
t3-keep-awake off      # back to awake only while T3 works
t3-keep-awake stop     # stop keeping Windows awake
t3-keep-awake start    # start again
```

Agents can do the same through MCP: `t3-keep-awake mcp` serves `status`,
`always_on`, and `always_off` tools.

[Configuration, uninstalling, and development](docs/usage.md).

[MIT licence](LICENSE).
