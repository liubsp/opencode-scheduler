---
name: opencode-scheduler
description: Use when explicitly told by the user to load the opencode-scheduler CLI reference skill.
metadata:
  opencode/autoinvoke: false
---

# OpenCode scheduler CLI

Load this skill only on an explicit user request. Do not invoke it proactively or schedule anything the user did not request. Use the installed global `opencode-scheduler` command; consult `--help` for details. Never read secrets, authentication files or the scheduler database directly.

## Create once

Obtain the intended target, time, timezone and exact prompt text. Prefer RFC3339 with an explicit UTC offset when the date matters. Local `YYYY-MM-DD HH:MM[:SS]` uses the host timezone and rejects DST gaps/ambiguities. A clock such as `5:00`, `05:00` or `05:00:30` selects the next future local occurrence (today or tomorrow). A passed clock skips today; a selected future DST gap or ambiguity is rejected. Check the full resolved date/time printed by create rather than assuming which day was selected.

```powershell
opencode-scheduler status
opencode-scheduler schedule create --session ses_example --at '2099-11-06T14:00:00+01:00' --prompt 'Continue the task' --delivery steer
opencode-scheduler schedule create --session-title 'Exact title' --at '5:00' --prompt 'Resume the task' --delivery queue
```

Replace example values with the user's instructions. Choose either `--session` (one ID) or `--session-title 'Exact title'`: title selection freezes **ALL exact matches**, not just the current session. Confirm that fanout is intended. `steer` is the default; `queue` waits behind current work. Preserve prompt text literally, including whitespace, quotes, `$`, `@` and newlines; shell quoting must not interpolate it. The scheduler does no command/file expansion.

Create prints a request UUID to stderr before contacting the daemon. Retain it with the original inputs. If the response is lost or uncertain, retry only the identical command with `--request-id` set to that UUID; never generate a new ID for the same uncertain create. A replay returns the original frozen batch; changed inputs conflict. Aborting a CLI wait does not cancel accepted schedules.

## Inspect and cancel

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler schedule cancel <schedule-id>
```

Plain `list` and `show` omit prompt text. For internal exact-prompt inspection, optional `opencode-scheduler schedule show <schedule-id> --json` includes the literal prompt. Keep plain commands primary. `list` and `log` accept `--limit` (1–100) and the previous page's `--cursor`. Output is human-readable by default; `--json` is optional for internal automation.

There are six persisted delivery attempts total, with waits of 10/20/40/80/160 seconds after failed attempts finish. Restarts do not reset the budget; overdue work has no age expiry. Read logs rather than creating duplicates. `admitted` means OpenCode accepted the prompt, not that the agent completed it. `unconfirmed` means admission is uncertain, even after exhaustion. Cancellation cannot recall admitted work or promise that an uncertain/in-flight delivery was not accepted.

`status` is read-only. If explicitly needed, `opencode-scheduler daemon start` / `daemon stop` control only the scheduler; stopping preserves schedules. There is no automatic login startup or wake-from-sleep support. Do not restart OpenCode, edit PATH/configuration, or install anything without authorization.
