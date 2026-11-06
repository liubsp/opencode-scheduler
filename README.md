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

`--at` accepts `5:00`, `05:00` or `05:00:30` for the **next strictly future local occurrence** (today if still future, otherwise tomorrow), an RFC 3339 datetime with an explicit offset, or local `YYYY-MM-DD HH:MM[:SS]`. For example, `'2099-11-06 05:00'` uses the daemon host's IANA timezone; `2099-11-06T05:00:00+01:00` specifies an explicit offset. Local input rejects ambiguous or nonexistent DST times: choose another time or specify an offset. Creation prints the full resolved date, offset and local timezone. The appointment must still be future when committed; slow validation never silently moves it to another day. Clock replay returns the original frozen appointment, even on another day.

Create prints a request UUID to stderr before contacting the daemon. To recover a lost response, repeat the identical command with `--request-id <that UUID>`. This returns the original frozen batch, even if OpenCode is now offline; different input with the same UUID conflicts.

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule cancel <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler daemon stop
```

Commands print readable summaries by default, including resolved dates, states, attempts and delivery errors. `list` and `log` accept `--limit` (1–100) and the `--cursor` printed with the previous page. For automation, optional `--json` returns the stable API success/error envelope on stdout. Errors exit nonzero; human diagnostics go to stderr.

Every command has `--help`, including argument meanings, accepted time formats, delivery and recovery semantics. `steer` asks OpenCode to admit the prompt immediately; `queue` asks it to queue the prompt. Neither mode means the agent has completed the task.

## Recover from an error

Errors include a safe recovery hint. Malformed time syntax is rejected before directory preparation or service startup; the daemon remains authoritative for timezone resolution and replay. Private-directory security validation, its deadline, daemon startup, storage permissions, full disks and database lock contention have distinct error codes. Unknown storage failures stay explicitly unknown, rather than guessing a cause.

First run `opencode-scheduler status`. If the scheduler is absent, use `opencode-scheduler daemon start`; inspect `diagnostic.log` in the private data directory locally if startup fails. Check ownership/write permissions and free disk space without weakening ACLs or deleting the database. On Windows, PowerShell security validation has a 15-second deadline; daemon startup then has a separate 30-second budget, including its own security validation. The plugin allows 85 seconds for the invoked start command: these stages, its final status RPC (up to 35 seconds), and process launch/exit overhead. A deadline does not prove startup failed: check status before acting. Sanitize diagnostics before sharing them.

For a lost creation response, preserve the printed request UUID and repeat **identical input** with `--request-id <UUID>`; never change the timestamp or use a new UUID as a blind retry. Use `schedule list`, `schedule show <id>` and `schedule log <id>` to inspect existing work. Only create a new request after confirming you want a separate appointment. Admission uncertainty is not rejection, and cancellation cannot retract accepted work. There is no manual retry/reset command; automatic retries retain their original message ID and attempt budget.

## Delivery and limits

- Six attempts maximum, with retries 10/20/40/80/160 seconds after each failed attempt completes. Restarting does not replenish attempts. Overdue work resumes when the scheduler runs; there is no expiry or catch-up burst.
- The original target, prompt, mode and message ID are preserved across retries. Restart recovery reconciles unfinished work without submitting that unfinished attempt again.
- `admitted` means OpenCode accepted the prompt, not that the agent finished. `unconfirmed` means acceptance is uncertain, including after retry exhaustion. Cancellation cannot retract admitted work or guarantee cancellation during an uncertain/in-flight attempt.
- Local list/show/cancel/log remain available while OpenCode is disconnected or a delivery is waiting on the network. `status` is read-only and never starts either service.
- The daemon is singleton per data directory, bound to IPv4 loopback and protected by a private random token and instance ID. Windows launch uses no console and inherits no caller handles. `daemon stop` leaves schedules intact. There is no login/startup registration: start it manually after signing in.

Global registration is currently Windows-only; non-Windows installation is not runtime-certified. OpenCode bootstrap uses one supported `opencode api get /api/info` helper, bounded to 15 seconds; delivery still uses authenticated HTTP, not CLI prompt forwarding. Inconclusive process ownership fails visibly rather than replacing a service. Keep the machine awake for on-time delivery; the scheduler does not wake it.

The earlier Windows pilot was validated with OpenCode service 2.2.5: timed prompt admission, same-message-ID retransmission, cancellation, scheduler stop/start recovery, message lookup, and delivery logs. Automated fixtures additionally cover human output, isolated staged upgrades, PATH transformations and bounded bootstrap. The completed global installation and live bootstrap still require installed validation. Other service versions and live OpenCode restart recovery have not been runtime-certified. No provider or quota-management policy is implemented.

## Build an audited payload

Requires a Windows x64 Rust MSVC toolchain with `rust-docs`, Node 22 or newer, npm and tar. The builder uses locked dependencies, remaps native compiler paths, bundles the plugin as ESM with only Node builtins external, and generates license notices from incorporated code. The SDK and bundler are development-only dependencies; the plugin package has no runtime npm dependencies.

```powershell
powershell -NoProfile -File scripts/build-release.ps1 -PayloadOnly
node scripts/check-package.mjs dist/payload
```

`dist/payload` contains the native executable, bundled plugin, explicit-only CLI reference skill, README, license and generated third-party notices. The scanner enforces a strict file allowlist and checks text/native strings for private build prefixes, credentials and debug metadata. No runtime state or installation configuration is shipped.

This staging payload is not a complete user installer release. Normal builder mode requires `scripts/install.ps1`, adds it to the payload, creates `dist/opencode-scheduler-<version>-windows-x64.zip`, then extracts and scans the archive. It fails clearly if the installer is absent. CI checks Rust and Node on Windows/Linux with Node 22/24; the release workflow builds an audited archive without publishing it.

`skills/opencode-scheduler/SKILL.md` is a concise CLI reference for explicit user requests only. Its V2 `opencode/autoinvoke: false` metadata hides it from automatic advertisement while permitting explicit loading by ID. Skill installation and live registration require the product installer; merely building the payload does not change OpenCode configuration.

MIT licensed.
