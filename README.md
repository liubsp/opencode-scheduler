# opencode-scheduler

Durable, manual one-shot prompt scheduling for OpenCode V2, using a detached Rust daemon and CLI.

## Windows installation

Requires Windows x64, PowerShell and OpenCode V2, service version 2.2.0 or newer within major version 2 (the CLI version may differ). No Rust or npm installation is needed to use an expanded release. The scheduler reads OpenCode's service registration and authenticates directly. Creating or delivering a schedule may start OpenCode only when its registration is absent or its registered process is verified dead. It never restarts a live, unhealthy, incompatible or authentication-failing OpenCode service.

Expand the audited Windows x64 release ZIP into a fresh directory, then run its installer **in your PowerShell terminal**:

```powershell
.\install.ps1
opencode-scheduler status
```

The installer refreshes that PowerShell process's PATH. Other already-running terminals need a new terminal. Running the installer with `powershell -File` refreshes only that child process, not its parent. To start the scheduler manually:

```powershell
opencode-scheduler daemon start
```

The product installer validates and stages the native executable, standalone plugin and explicit-only skill before activation, then starts and authenticates the installed scheduler daemon, including on a clean installation. It delegates native upgrades and user PATH registration to `opencode-scheduler install`: only the installed native executable directory (`app/bin`) is registered, without replacing other entries. Commands run the executable directly, with no intermediate command interpreter; the caller's shell still controls argument quoting (see below). Upgrades stop only the authenticated scheduler daemon if running, preserve schedules and installation metadata, then restore its running state. A loaded old executable may remain as a private backup until a later installation can remove it.

Existing plugin/skill directories stay in place during upgrades, including when a running host retains child-directory handles. Managed files are replaced individually and atomically, not as one multi-file transaction. If a managed file cannot be replaced, installation fails and rolls back payload changes; the completed native upgrade and scheduler data remain. If another consumer also prevents rollback, the error identifies the affected path and recovery-backup location for local inspection. No host restart or forced reload is performed.

Existing managed payload and metadata files must grant Allow access only to the current user. Shared files are refused before installation writes or native upgrade; their contents and permissions are left unchanged. Preserve the file and review its sharing locally before explicitly restoring private access. Administrators ownership alone is not sharing and is not changed merely for differing. Plugin/skill and product-manifest replacements preserve exact owner/group/DACL/inheritance and unfamiliar streams; if permission restoration fails, installation fails and restores the original file or retains its private recovery backup with an inspection hint.

Native data defaults to `%LOCALAPPDATA%/opencode-scheduler`, or `OPENCODE_SCHEDULER_HOME`. Plugin discovery uses `${XDG_CONFIG_HOME:-~/.config}/opencode/plugins/opencode-scheduler/`; the skill goes under the corresponding `skills/opencode-scheduler/`. The generated private `plugin/install.json` contains only protocol, installed binary path and data-home path, never endpoint credentials. The data-home ownership manifest records managed file hashes. Existing unrelated JSON/JSONC, plugins and skills are never edited. Unfamiliar files inside owned directories are preserved; modified managed files or nonempty directories without an ownership record cause a safe refusal rather than an overwrite. Restore the original managed file after preserving your edit before upgrading.

For isolated installation or manual PATH management:

```powershell
.\install.ps1 -DataHome $dataDirectory -ConfigHome $configDirectory -NoPath
```

Both options require absolute directories; `-ConfigHome` is the XDG config root, not its `opencode` child. The installer sets those environment variables in its process. Custom roots also need `OPENCODE_SCHEDULER_HOME` and `XDG_CONFIG_HOME` set in future terminals and in the host environment. Existing unsafe data-directory ownership/permissions or linked installation paths are rejected, never silently weakened. `-NoPath` skips persistent user PATH registration; this process still gets the native command on PATH. Native-only development installs remain available with `cargo build` then `target/debug/opencode-scheduler.exe install --no-path`, but do not register the plugin or skill, and start a daemon only if one was running before the upgrade.

### Plugin and skill readiness

OpenCode watches global discovery directories; the installer makes no config edits, never overrides an explicit plugin disable policy, and never restarts OpenCode. It observes status up to three times, with one-second gaps (each status RPC is bounded by the native 35-second transport deadline). Forced `/api/location/reload` is deliberately not automatic because it cancels pending forms and permissions.

`status` reports **plugin payload installed** separately from **Plugin host** (`active`, `not_loaded`, `failed` or `unknown`). Host observation is read-only through `/api/plugin`, scoped to the API's default host location; an active same-ID plugin from another path is not accepted. Disabled plugins may be absent from that list and therefore report `not_loaded`. An unavailable host leaves readiness unknown; installation can succeed with this explicit warning. Check the plugin list and policy in your intended OpenCode location, wait for watched discovery, and recheck status. Service readiness alone does not establish plugin readiness. An active plugin still needs actual tool invocation and skill-loading acceptance in that location.

Use `@opencode-scheduler` explicitly to load the CLI teaching skill. It has an explicit-request-only description and `opencode/autoinvoke: false`; it must not be proactively advertised or schedule anything automatically. The plugin exposes `scheduler_create/list/show/cancel/log/status`; it uses the same native scheduler, not a second timer or store.

## Schedule a prompt

```powershell
$at = (Get-Date).AddMinutes(5).ToString('o')
opencode-scheduler schedule create --session ses_example --at $at --prompt 'Continue the task' --delivery steer
opencode-scheduler schedule create --session-title 'Exact session title' --at $at --prompt 'Continue the task' --delivery queue
```

Use either `--session` or `--session-title`, not both. Title matching is exact and service-wide: **every match gets a separate schedule**. The complete recipient set is validated and committed together. Later title changes do not change that set.

### Literal quotes and multiline prompts

Windows PowerShell 5.1 remains supported for installation and simple prompts such as the examples above, but its legacy native-argument rules can strip embedded double quotes or alter adjacent backslashes even from a single-quoted variable. The CLI cannot reconstruct text lost by the shell. For exact quoted/multiline text, open **PowerShell 7.3 or newer (`pwsh`)** and run this recipe there, not as a prompt argument forwarded through PowerShell 5.1:

```powershell
if ($PSVersionTable.PSVersion -lt [version]'7.3') { throw 'PowerShell 7.3+ required' }
$PSNativeCommandArgumentPassing = 'Standard'
$prompt = @'
Say "hello" exactly.
Keep folder\subfolder\ and $variables literal.
'@
opencode-scheduler schedule create --session ses_example --at '2099-11-06T14:00:00+01:00' --prompt $prompt
```

The single-quoted here-string does not interpolate text; place its closing `'@` on a line of its own. If the prompt itself contains such a closing line, use an existing string variable containing the exact text instead. Here-string delimiter line breaks are not part of the prompt; interior line endings are preserved. Save script files for this recipe as UTF-8 (PowerShell 7 supports UTF-8 without a BOM). No CMD shim, quote reconstruction or prompt expansion is performed. `--prompt` is limited to 65536 UTF-8 bytes; the RPC/plugin input contract is unchanged.

`--at` accepts `5:00`, `05:00` or `05:00:30` for the **next strictly future local occurrence** (today if still future, otherwise tomorrow), an RFC 3339 datetime with an explicit offset, or local `YYYY-MM-DD HH:MM[:SS]`. For example, `'2099-11-06 05:00'` uses the daemon host's IANA timezone; `2099-11-06T05:00:00+01:00` specifies an explicit offset. Local input rejects ambiguous or nonexistent DST times: choose another time or specify an offset. Creation prints the full resolved date, offset and local timezone. The appointment must still be future when committed; slow validation never silently moves it to another day. Clock replay returns the original frozen appointment, even on another day.

Create prints a request UUID to stderr before contacting the daemon. To recover a lost response, repeat the identical command with `--request-id <that UUID>`. This returns the original frozen batch, even if OpenCode is now offline; different input with the same UUID conflicts.

```powershell
opencode-scheduler schedule list
opencode-scheduler schedule show <schedule-id>
opencode-scheduler schedule cancel <schedule-id>
opencode-scheduler schedule log <schedule-id> --limit 50
opencode-scheduler daemon stop
```

Commands print readable summaries by default, including resolved session IDs/titles, dates, states, attempts and delivery errors, without prompt bodies. `list` and `log` accept `--limit` (1–100) and the `--cursor` printed with the previous page. For automation, optional `--json` returns the stable API success/error envelope on stdout. Errors exit nonzero; human diagnostics go to stderr.

Every command has `--help`, including argument meanings, accepted time formats, delivery and recovery semantics. `steer` asks OpenCode to admit the prompt immediately; `queue` asks it to queue the prompt. Neither mode means the agent has completed the task.

## Recover from an error

Errors include a safe recovery hint. Malformed time syntax is rejected before directory preparation or service startup; the daemon remains authoritative for timezone resolution and replay. Private-directory security validation, its deadline, daemon startup, storage permissions, full disks and database lock contention have distinct error codes. Unknown storage failures stay explicitly unknown, rather than guessing a cause.

First run `opencode-scheduler status`. If the scheduler is absent, use `opencode-scheduler daemon start`; inspect `diagnostic.log` in the private data directory locally if startup fails. Check ownership/write permissions and free disk space without weakening ACLs or deleting the database. On Windows, a native security-validation child has a 15-second deadline; daemon startup then has a separate 30-second budget, including its own security validation. The plugin allows 85 seconds for the invoked start command: these stages, its final status RPC (up to 35 seconds), and process launch/exit overhead. A deadline does not prove startup failed: check status before acting. Sanitize diagnostics before sharing them.

For a lost creation response, preserve the printed request UUID and repeat **identical input** with `--request-id <UUID>`; never change the timestamp or use a new UUID as a blind retry. Use `schedule list`, `schedule show <id>` and `schedule log <id>` to inspect existing work. Only create a new request after confirming you want a separate appointment. Admission uncertainty is not rejection, and cancellation cannot retract accepted work. There is no manual retry/reset command; automatic retries retain their original message ID and attempt budget.

## Delivery and limits

- Six attempts maximum, with retries 10/20/40/80/160 seconds after each failed attempt completes. Restarting does not replenish attempts. Overdue work resumes when the scheduler runs; there is no expiry or catch-up burst.
- The original target, prompt, mode and message ID are preserved across retries. Restart recovery reconciles unfinished work without submitting that unfinished attempt again.
- `admitted` means OpenCode accepted the prompt, not that the agent finished. `unconfirmed` means acceptance is uncertain, including after retry exhaustion. Cancellation cannot retract admitted work or guarantee cancellation during an uncertain/in-flight attempt.
- Local list/show/cancel/log remain available while OpenCode is disconnected or a delivery is waiting on the network. `status` is read-only and never starts either service.
- The daemon is singleton per data directory, bound to IPv4 loopback and protected by a private random token and instance ID. Windows launch uses no console and inherits no caller handles. `daemon stop` leaves schedules intact. There is no login/startup registration: start it manually after signing in.

Global registration is currently Windows-only; non-Windows installation is not runtime-certified. OpenCode bootstrap uses one supported `opencode api get /api/info` helper, bounded to 15 seconds; delivery still uses authenticated HTTP, not CLI prompt forwarding. Inconclusive process ownership fails visibly rather than replacing a service. Keep the machine awake for on-time delivery; the scheduler does not wake it.

Windows x64 installed acceptance is verified with OpenCode service 2.2.5 (CLI 2.2.0): packaged global installation/native upgrade, watched plugin discovery, all six loaded tools, explicit-only skill loading, timed admission, same-message-ID retransmission, cancellation and scheduler stop/start recovery. Existing schedules, metadata, PATH and unrelated configuration were preserved. Automated fixtures cover isolated upgrades, fanout, failures and bounded bootstrap. Upgrading already-loaded plugin/skill directories, live OpenCode bootstrap/restart recovery, other service versions and non-Windows installation are not runtime-certified. No provider or quota-management policy is implemented.

## Build an audited payload

Requires a Windows x64 Rust MSVC toolchain with `rust-docs`, Node 22 or newer, npm and tar. The builder uses locked dependencies, remaps native compiler paths, bundles the plugin as ESM with only Node builtins external, and generates license notices from incorporated code. The SDK and bundler are development-only dependencies; the plugin package has no runtime npm dependencies.

```powershell
powershell -NoProfile -File scripts/build-release.ps1
node scripts/check-package.mjs dist/payload --installer
```

`dist/payload` contains the native executable, bundled plugin, explicit-only CLI reference skill, README, license and generated third-party notices. The scanner enforces a strict file allowlist and checks text/native strings for private build prefixes, credentials and debug metadata. No runtime state or installation configuration is shipped.

Normal builder mode includes `install.ps1`, creates `dist/opencode-scheduler-<version>-windows-x64.zip`, then extracts and scans the archive. `-PayloadOnly` builds staging files without an installer/ZIP and is not a complete user release. CI checks Rust and Node on Windows/Linux with Node 22/24; Windows builds the full release before running isolated installer acceptance. The release workflow builds an audited archive without publishing it.

`skills/opencode-scheduler/SKILL.md` is the shipped CLI reference. Merely building the payload does not change OpenCode configuration. Isolated fixtures import the installed bundled plugin, invoke it against the packaged native daemon, and preserve a pending appointment through reinstall. These fixtures are distinct from the loaded-host acceptance above; the installer alone does not perform tool invocation or explicit skill loading.

MIT licensed.
