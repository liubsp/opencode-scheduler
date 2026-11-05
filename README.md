# opencode-scheduler

Durable, manual one-shot prompt scheduling for OpenCode V2, using a detached Rust daemon and CLI.

## Private Windows pilot

Requires Rust to build and an already-running OpenCode service, version 2.2.0 or newer within major version 2. The scheduler reads OpenCode's service registration and authenticates directly; it does not change OpenCode configuration or restart its service.

Build and copy the executable into private application data:

```powershell
cargo build
.\target\debug\opencode-scheduler.exe install --json
$scheduler = Join-Path $env:LOCALAPPDATA 'opencode-scheduler/app/bin/opencode-scheduler.exe'
& $scheduler daemon start
& $scheduler status
```

`install` does not start anything or change PATH. For an alternative data directory, set `OPENCODE_SCHEDULER_HOME` to an absolute private directory before installation and use its `app/bin/opencode-scheduler.exe`. An existing directory with unsafe ownership or permissions is rejected rather than silently weakened.

## Schedule a prompt

```powershell
$at = (Get-Date).AddMinutes(5).ToString('o')
& $scheduler schedule create --session ses_example --at $at --prompt 'Continue the task' --delivery steer
& $scheduler schedule create --session-title 'Exact session title' --at $at --prompt 'Continue the task' --delivery queue
```

Use either `--session` or `--session-title`, not both. Title matching is exact and service-wide: **every match gets a separate schedule**. The complete recipient set is validated and committed together. Later title changes do not change that set.

`--at` accepts an RFC 3339 datetime with an explicit offset, or local `YYYY-MM-DD HH:MM[:SS]`. Local input uses the host's IANA timezone and rejects ambiguous or nonexistent DST times. Creation prints the full resolved datetime. It must still be in the future when committed.

Create prints a request UUID to stderr before contacting the daemon. To recover a lost response, repeat the identical command with `--request-id <that UUID>`. This returns the original frozen batch, even if OpenCode is now offline; different input with the same UUID conflicts.

```powershell
& $scheduler schedule list --json
& $scheduler schedule show <schedule-id> --json
& $scheduler schedule cancel <schedule-id> --json
& $scheduler schedule log <schedule-id> --limit 50 --json
& $scheduler daemon stop
```

`list` and `log` accept `--limit` (1–100) and `--cursor` from the previous page's `next_cursor`. `--json` returns the authenticated API's success/error envelope on stdout; errors exit nonzero. Human diagnostics go to stderr.

## Delivery and limits

- Six attempts maximum, with retries 10/20/40/80/160 seconds after each failed attempt completes. Restarting does not replenish attempts. Overdue work resumes when the scheduler runs; there is no expiry or catch-up burst.
- The original target, prompt, mode and message ID are preserved across retries. Restart recovery reconciles unfinished work without submitting that unfinished attempt again.
- `admitted` means OpenCode accepted the prompt, not that the agent finished. `unconfirmed` means acceptance is uncertain, including after retry exhaustion. Cancellation cannot retract admitted work or guarantee cancellation during an uncertain/in-flight attempt.
- Local list/show/cancel/log remain available while OpenCode is disconnected or a delivery is waiting on the network. `status` is read-only and never starts either service.
- The daemon is singleton per data directory, bound to IPv4 loopback and protected by a private random token and instance ID. Windows launch uses no console and inherits no caller handles. `daemon stop` leaves schedules intact. There is no login/startup registration: start it manually after signing in.

This is a private debug-build pilot, not a distributable release. The TypeScript plugin, absent-service bootstrap, release packaging/CI, extensive failure injection and supported non-Windows installation are deferred. If OpenCode is absent, start it normally; the scheduler reconnects on subsequent attempts. Keep the machine awake for on-time delivery; the scheduler does not wake it.

Validated on Windows with OpenCode service 2.2.5: timed prompt admission, same-message-ID retransmission, cancellation, scheduler stop/start recovery, message lookup, and delivery logs. Automated checks cover the durable core and detached CLI path (17 tests). Other service versions and live OpenCode restart recovery have not been runtime-certified. No provider or quota-management policy is implemented.

MIT licensed.
