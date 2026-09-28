Capture new durable conventions, invariants, and recurring pitfalls here when they will help future agents make better decisions.

- After changes run `cargo fmt && cargo clippy --all-targets && cargo test`. Then `./target/release/herdr-keep-awake restart` (after `cargo build --release`) so the live daemon runs the new binary; the old one keeps running otherwise.
- Keep the daemon level-triggered: hooks only nudge; `decide::Tracker::observe` recomputes the hold from a full snapshot. Don't add logic that depends on seeing a particular event.
- Use herdr's semantic `agent_status` as the source of truth; inspect `herdr agent explain` when its classification seems wrong instead of inferring activity from process CPU or terminal output here.
- Keep one controller running while installed. Poll slowly only after a complete zero-session snapshot and once any grace hold has ended; an unreadable session list must keep being retried. Never renew a hold from a snapshot older than 60s.
- Keep `poll_secs` below the shared `MAX_SNAPSHOT_AGE` limit so normal polling cannot repeatedly expire and restart the keeper.
- Fail toward letting Windows sleep. Any new failure path should end in a released hold (grace period at most), never an indefinite one.
- Windows PowerShell 5.1 pitfalls in `src/keeper.ps1`:
  - Hex literals `>= 0x80000000` parse as negative Int32 and fail the uint parameter. Pass `SetThreadExecutionState` flags as decimal (filled in from Rust).
  - `[Console]::In.ReadLineAsync()` blocks, silently defeating the heartbeat timeout. Use `IO.StreamReader([Console]::OpenStandardInput())`.
  - The keeper must release before writing to stdout: with the daemon gone, the write can fail and end the script.
- `CallNtPowerInformation(SystemExecutionState)` (`probe`) is system-wide. Other Windows processes briefly set `ES_SYSTEM_REQUIRED` too, so don't treat it as proof of what our keeper did; use keeper replies and process state.
- `herdr` event hooks inherit the invoking session's `HERDR_*` env. `Herdr::call` strips it and uses `--session` explicitly.
- `HERDR_BIN_PATH` can contain a stale `... (deleted)` path after herdr updates during a long-running session. Check it before each CLI call and fall back to `herdr` on `PATH`.
- `revision` in `herdr agent list` barely changes while an agent works, so it can't show liveness. `state_change_seq` changes on every status transition, so `(pane, seq)` identifies one uninterrupted stretch.
