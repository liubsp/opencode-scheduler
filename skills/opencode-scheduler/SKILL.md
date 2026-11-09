---
name: opencode-scheduler
description: Use when explicitly told by the user to load the opencode-scheduler CLI reference skill.
metadata:
  opencode/autoinvoke: false
---

# OpenCode scheduler CLI

Use this reference only on explicit request, never proactively. Schedule only what the user requests, using the installed `opencode-scheduler` command. Use `--help` for arguments; do not read secrets, authentication files or the scheduler database directly.

## Create once

Obtain the target, time, timezone and exact prompt. RFC 3339 with an explicit offset specifies an instant. Local `YYYY-MM-DD HH:MM[:SS]` uses the daemon host timezone. A clock (`5:00`, `05:00`, `05:00:30`) selects the next future local occurrence, today or tomorrow. DST gaps/ambiguities are rejected: choose another time or specify an offset. Check the resolved full date and offset.

```powershell
opencode-scheduler status
opencode-scheduler schedule create --session ses_example --at '2099-11-06T14:00:00+01:00' --prompt 'Continue the task' --delivery steer
opencode-scheduler schedule create --session-title 'Exact title' --at '5:00' --prompt 'Resume the task' --delivery queue
```

Replace example values with the user's instructions. Choose `--session` for one ID or `--session-title` for **ALL exact, case-sensitive matches across the service**. Confirm intended fanout; recipients are frozen at creation. Default `steer` requests immediate admission; `queue` waits behind current work. Prompts are literal, subject to shell argument passing.

PowerShell 5.1 can strip quotes or alter adjacent backslashes. For exact quoted/multiline prompts, run this in PowerShell 7.3+ (`pwsh`), without forwarding through a legacy shell. Save scripts as UTF-8.

```powershell
if ($PSVersionTable.PSVersion -lt [version]'7.3') { throw 'PowerShell 7.3+ required' }
$PSNativeCommandArgumentPassing = 'Standard'
$prompt = @'
Say "hello" exactly.
Keep folder\subfolder\ and $variables literal.
'@
opencode-scheduler schedule create --session ses_example --at '2099-11-06T14:00:00+01:00' --prompt $prompt
```

The single-quoted here-string prevents interpolation; close it with `'@` on its own line. If the prompt contains that line, pass an existing exact string variable. Delimiter line breaks are excluded; interior line endings are preserved. Never reconstruct quotes lost by the shell. The prompt limit is 65536 UTF-8 bytes. Check create's resolved session IDs/titles and datetime.

Retain the request UUID printed to stderr and the original inputs. After a lost or uncertain response, repeat identical input with `--request-id <UUID>`. Replay returns the original batch and time; changed input conflicts. A new UUID risks duplicates. Aborting a wait does not cancel accepted schedules.

## Inspect and cancel

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler schedule cancel <schedule-id>
```

Readable `list` and `show` summaries omit prompt text. Optional `opencode-scheduler schedule show <schedule-id> --json` includes the literal prompt; use `--json` for automation. `list` and `log` accept `--limit` (1–100) and the previous page's `--cursor`.

There are six persisted attempts, with waits of 10/20/40/80/160 seconds after failures finish. Retries preserve the message ID; restarts do not reset the budget. Overdue work has no expiry. `admitted` means accepted, not agent completion; `unconfirmed` means acceptance is uncertain. Cancellation cannot retract admitted work or guarantee cancellation of an uncertain/in-flight attempt. Inspect logs rather than creating duplicates.

`status` is read-only. If needed, `daemon start` / `daemon stop` control only the scheduler; stopping preserves schedules. Keep the machine awake: there is no automatic login startup or wake support. Do not restart OpenCode, edit PATH/configuration or install without authorization.
