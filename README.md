# opencode-scheduler

Durable, manual one-shot prompt scheduling for OpenCode V2, using a detached Rust daemon and CLI.

## Windows development installation

Requires Rust to build and OpenCode on PATH, version 2.2.0 or newer within major version 2. The scheduler reads OpenCode's service registration and authenticates directly. Creating or delivering a schedule may start OpenCode only when its registration is absent or its registered process is verified dead. It never restarts a live, unhealthy, incompatible or authentication-failing OpenCode service.

Build and install the global command into private application data:

```powershell
cargo build
.\target\debug\opencode-scheduler.exe install
```

Open a new terminal, then use the installed command:

```powershell
opencode-scheduler status
opencode-scheduler daemon start
```

`install` adds the installed native executable directory (`app/bin`) to your user PATH, without replacing other entries. Commands run the executable directly, preserving literal prompt arguments without an intermediate command interpreter. Installation does not start an absent scheduler daemon. Upgrades stage the executable, stop only the authenticated scheduler daemon if running, preserve schedules and installation metadata, then restore its running state. A loaded old executable may remain as a private backup until a later installation can remove it.

For an alternative data directory, set `OPENCODE_SCHEDULER_HOME` to an absolute private directory before installation and in terminals using it. An existing directory with unsafe ownership or permissions is rejected rather than silently weakened. `install --no-path` supports isolated installation or manual PATH management without changing your user PATH.

## Schedule a prompt

```powershell
$at = (Get-Date).AddMinutes(5).ToString('o')
opencode-scheduler schedule create --session ses_example --at $at --prompt 'Continue the task' --delivery steer
opencode-scheduler schedule create --session-title 'Exact session title' --at $at --prompt 'Continue the task' --delivery queue
```

Use either `--session` or `--session-title`, not both. Title matching is exact and service-wide: **every match gets a separate schedule**. The complete recipient set is validated and committed together. Later title changes do not change that set.

`--at` accepts an RFC 3339 datetime with an explicit offset, or local `YYYY-MM-DD HH:MM[:SS]`. Local input uses the host's IANA timezone and rejects ambiguous or nonexistent DST times. Creation prints the full resolved datetime. It must still be in the future when committed.

Create prints a request UUID to stderr before contacting the daemon. To recover a lost response, repeat the identical command with `--request-id <that UUID>`. This returns the original frozen batch, even if OpenCode is now offline; different input with the same UUID conflicts.

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule cancel <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler daemon stop
```

Commands print readable summaries by default, including resolved dates, states, attempts and delivery errors. `list` and `log` accept `--limit` (1–100) and the `--cursor` printed with the previous page. For automation, optional `--json` returns the stable API success/error envelope on stdout. Errors exit nonzero; human diagnostics go to stderr.

## Delivery and limits

- Six attempts maximum, with retries 10/20/40/80/160 seconds after each failed attempt completes. Restarting does not replenish attempts. Overdue work resumes when the scheduler runs; there is no expiry or catch-up burst.
- The original target, prompt, mode and message ID are preserved across retries. Restart recovery reconciles unfinished work without submitting that unfinished attempt again.
- `admitted` means OpenCode accepted the prompt, not that the agent finished. `unconfirmed` means acceptance is uncertain, including after retry exhaustion. Cancellation cannot retract admitted work or guarantee cancellation during an uncertain/in-flight attempt.
- Local list/show/cancel/log remain available while OpenCode is disconnected or a delivery is waiting on the network. `status` is read-only and never starts either service.
- The daemon is singleton per data directory, bound to IPv4 loopback and protected by a private random token and instance ID. Windows launch uses no console and inherits no caller handles. `daemon stop` leaves schedules intact. There is no login/startup registration: start it manually after signing in.

This is a development installation, not yet a distributable release. The TypeScript plugin and release packaging/CI are separate completion work. Global registration is currently Windows-only; non-Windows installation is not runtime-certified. OpenCode bootstrap uses one supported `opencode api get /api/info` helper, bounded to 15 seconds; delivery still uses authenticated HTTP, not CLI prompt forwarding. Inconclusive process ownership fails visibly rather than replacing a service. Keep the machine awake for on-time delivery; the scheduler does not wake it.

The earlier Windows pilot was validated with OpenCode service 2.2.5: timed prompt admission, same-message-ID retransmission, cancellation, scheduler stop/start recovery, message lookup, and delivery logs. Automated fixtures additionally cover human output, isolated staged upgrades, PATH transformations and bounded bootstrap. The completed global installation and live bootstrap still require installed validation. Other service versions and live OpenCode restart recovery have not been runtime-certified. No provider or quota-management policy is implemented.

MIT licensed.
