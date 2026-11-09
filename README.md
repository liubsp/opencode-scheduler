# opencode-scheduler

Schedule a prompt once in OpenCode V2. A background scheduler keeps appointments across restarts.

## Install on Windows

Requires Windows x64, PowerShell and OpenCode V2 service version 2.2.0 or newer within major version 2. The CLI version may differ. Release users do not need Rust or npm.

Expand the Windows x64 release ZIP into a fresh directory. Run this **in your PowerShell terminal**:

```powershell
.\install.ps1
opencode-scheduler status
```

The installer installs the command, plugin and explicit-only skill, then starts the scheduler. It refreshes this terminal's PATH; open a new terminal for other running shells. Running it through `powershell -File` refreshes only the child process's PATH.

Upgrades preserve schedules and unrelated configuration. Shared or modified managed files are refused: preserve your edit or review sharing locally, then restore the original file or current-user-only access before reinstalling. Unfamiliar files are left alone. If rollback fails, keep the reported recovery backup and inspect the affected path. A payload failure does not undo a completed native upgrade.

### Plugin and skill

`status` distinguishes **Plugin payload: installed** from **Plugin host: active**. Host status covers the queried OpenCode location, not every location. If inactive or unknown, check the plugin list and disable policy in your intended location, wait for watched discovery, then recheck. Installation does not restart OpenCode or override disabled plugins.

Load the CLI reference with `@opencode-scheduler` only when wanted. It does not load proactively or schedule automatically. The plugin provides `scheduler_create/list/show/cancel/log/status` using the same scheduler.

### Custom directories

Data defaults to `%LOCALAPPDATA%/opencode-scheduler`. Plugin and skill discovery uses `${XDG_CONFIG_HOME:-~/.config}/opencode/{plugins,skills}/opencode-scheduler/`.

```powershell
.\install.ps1 -DataHome $dataDirectory -ConfigHome $configDirectory -NoPath
```

Use absolute directories; `-ConfigHome` is the config root, not its `opencode` child. Set `OPENCODE_SCHEDULER_HOME` and `XDG_CONFIG_HOME` to these roots in future terminals and the OpenCode host environment. `-NoPath` skips persistent user PATH registration but refreshes this process's PATH. Otherwise only the native `app/bin` directory is added to user PATH. Linked paths and unsafe data permissions are refused.

## Schedule a prompt

```powershell
$at = (Get-Date).AddMinutes(5).ToString('o')
opencode-scheduler schedule create --session ses_example --at $at --prompt 'Continue the task' --delivery steer
opencode-scheduler schedule create --session-title 'Exact session title' --at $at --prompt 'Continue the task' --delivery queue
```

Choose one session ID or a title, not both. Title selection freezes **ALL exact, case-sensitive matches across the service**, with a separate schedule for each. Later title changes do not change recipients. `steer` requests immediate admission; `queue` requests queued admission. Neither means the agent has finished.

`--at` accepts:

- `5:00`, `05:00` or `05:00:30`: the next strictly future local occurrence, today or tomorrow.
- `'2099-11-06 05:00'`: local `YYYY-MM-DD HH:MM[:SS]` on the daemon host.
- `2099-11-06T05:00:00+01:00`: RFC 3339 with an explicit offset.

Local times use the host's IANA timezone. DST gaps and ambiguities are rejected; choose another time or specify an offset. Check the resolved full date, offset and timezone. The appointment must still be future when saved; validation does not roll it forward again.

Create prints a request UUID to stderr before contacting the daemon. If the response is lost, repeat **identical input** with `--request-id <that UUID>`. Replay returns the original batch and time, even on another day or while OpenCode is offline. Changed input conflicts. A new UUID creates a separate appointment; do not use one as a blind retry.

### Quotes and multiline text

Windows PowerShell 5.1 supports simple prompts but can strip embedded quotes or alter adjacent backslashes. For exact quoted/multiline text, open **PowerShell 7.3+ (`pwsh`)** and run the recipe there, without forwarding the prompt through a legacy shell:

```powershell
if ($PSVersionTable.PSVersion -lt [version]'7.3') { throw 'PowerShell 7.3+ required' }
$PSNativeCommandArgumentPassing = 'Standard'
$prompt = @'
Say "hello" exactly.
Keep folder\subfolder\ and $variables literal.
'@
opencode-scheduler schedule create --session ses_example --at '2099-11-06T14:00:00+01:00' --prompt $prompt
```

The single-quoted here-string prevents interpolation. Put its closing `'@` on its own line; if the prompt contains that line, pass an existing exact string variable instead. Delimiter line breaks are excluded; interior line endings are preserved. Save scripts as UTF-8. The scheduler passes text literally and cannot restore characters lost by the shell. Prompts must be nonblank and at most 65536 UTF-8 bytes.

## Inspect and cancel

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler schedule cancel <schedule-id>
```

Summaries show targets, dates, states, attempts and errors without prompt bodies. `list` and `log` accept `--limit` (1–100) and the previous page's `--cursor`. Optional `--json` returns the API envelope; `show --json` includes the literal prompt. Errors exit nonzero. Use `<command> --help` for arguments.

`admitted` means OpenCode accepted the prompt. `unconfirmed` means acceptance is uncertain, even after retries end. Cancellation cannot retract admitted work or guarantee cancellation of an uncertain/in-flight attempt. Aborting a command's wait does not cancel accepted schedules.

## Recover and operate

```powershell
opencode-scheduler status
opencode-scheduler daemon start
opencode-scheduler daemon stop
```

`status` is read-only and starts neither service. Start the scheduler if absent; stopping it preserves schedules and leaves OpenCode running. Local inspection and cancellation work while OpenCode is disconnected.

For startup failures, inspect `diagnostic.log` in the private data directory locally and check ownership, write permissions and free space. A timeout may still leave the daemon running: check status before retrying. Preserve scheduler data and private permissions; sanitize diagnostics before sharing. Inspect `list`, `show` and `log` before creating separate work; there is no manual delivery retry/reset command.

Delivery has six persisted attempts, with waits of 10/20/40/80/160 seconds after failures finish. Restarts do not reset the budget. Retries keep the same target, literal prompt, mode and message ID; interrupted work is reconciled before further submission. Overdue work resumes without expiry when the scheduler runs.

Keep the machine awake for on-time delivery. There is no automatic login startup or wake support. OpenCode may be started for creation/delivery only if its registration is absent or its process is verified dead; a live unhealthy service is not restarted. Global installation supports Windows only.

## Build a release

Requires Windows x64, Rust MSVC with `rust-docs`, Node 22+, npm and tar.

```powershell
powershell -NoProfile -File scripts/build-release.ps1
```

The builder uses locked dependencies, remaps compiler paths and generates third-party notices. It builds `dist/payload` and `dist/opencode-scheduler-<version>-windows-x64.zip`, then extracts and scans the archive for unexpected files, private paths, credentials and debug metadata. The standalone ESM plugin has no runtime npm dependencies. `-PayloadOnly` omits the installer and ZIP; it is not a complete release.

For a native-only development install, run `cargo build`, then `target/debug/opencode-scheduler.exe install --no-path`. This does not install the plugin or skill and only restarts an already-running scheduler.

MIT licensed.
