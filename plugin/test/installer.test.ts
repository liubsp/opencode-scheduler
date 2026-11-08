import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rm,
  writeFile,
} from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const root = fileURLToPath(new URL("../../", import.meta.url));
const exec = promisify(execFile);
test("Windows expanded release installs owned payload, preserves data and verifies host separately", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows product installer; portable core tested separately");
  // Catches absent installer, checkout imports, native replacement outside authenticated upgrade,
  // config clobbering, overwriting user edits, and mistaking disabled payload for loaded tools.
  assert.ok(
    existsSync(join(root, "scripts/install.ps1")),
    "Product installer must exist",
  );
  const release = join(root, ".tmp/release/expanded-zip");
  assert.ok(
    existsSync(join(release, "install.ps1")),
    "Build full release first: powershell -NoProfile -File scripts/build-release.ps1",
  );
  await mkdir(join(tmpdir(), "opencode"), { recursive: true });
  const outside = await mkdtemp(join(tmpdir(), "opencode/scheduler-product-"));
  const unpacked = join(outside, "expanded release");
  await cp(release, unpacked, { recursive: true });
  const data = join(outside, "private data");
  const config = join(outside, "config");
  const state = join(outside, "state");
  const plugin = join(config, "opencode/plugins/opencode-scheduler");
  const env = {
    ...process.env,
    OPENCODE_SCHEDULER_HOME: data,
    XDG_CONFIG_HOME: config,
    XDG_STATE_HOME: state,
    XDG_DATA_HOME: join(outside, "share"),
    LOCALAPPDATA: join(outside, "local"),
    HOME: outside,
    USERPROFILE: outside,
  };
  const native = join(data, "app/bin/opencode-scheduler.exe");
  const wrapper = join(outside, "install-and-check.ps1");
  await writeFile(
    wrapper,
    `$ErrorActionPreference='Stop'
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1')
$beforeUserPath = [Environment]::GetEnvironmentVariable('Path','User')
$beforeModulePath = $env:PSModulePath
# Model a discovery consumer retaining a child-directory handle when it sees a
# complete module. Forward real hashing unchanged; observing at this boundary
# avoids a timing-dependent race between the installer and a polling test host.
$global:schedulerTestDiscovery = [IO.Path]::GetFullPath((Join-Path $env:XDG_CONFIG_HOME 'opencode/plugins'))
$global:schedulerTestWatchers = [Collections.Generic.List[IO.FileSystemWatcher]]::new()
$global:schedulerTestFiles = [Collections.Generic.List[IO.FileStream]]::new()
$global:schedulerTestRollbackHeld = $false
$aclBreakpoint = $null
$global:schedulerTestAclInjected = $false
$global:schedulerTestAclBefore = $null
$global:schedulerTestAclRecoveryHeld = $false
function Get-FileHash {
    param([string]$LiteralPath, [string]$Algorithm = 'SHA256')
    $hash = Microsoft.PowerShell.Utility\\Get-FileHash @PSBoundParameters
    $observed = [IO.Path]::GetFullPath($LiteralPath)
    $stageParent = [IO.Path]::GetFullPath((Join-Path $env:XDG_CONFIG_HOME 'opencode'))
    if ($observed.StartsWith($stageParent + '\\.scheduler-stage-', [StringComparison]::OrdinalIgnoreCase)) {
        $a = [Security.AccessControl.FileSecurity]::new($observed,[Security.AccessControl.AccessControlSections]::Access)
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
        foreach ($rule in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -eq 'Allow' -and !$rule.IdentityReference.Equals($sid)) { throw 'Unpublished payload must remain private.' }
        }
    }
    if ($observed.StartsWith($global:schedulerTestDiscovery + '\\.scheduler-stage-', [StringComparison]::OrdinalIgnoreCase) -and $observed.EndsWith('dist\\index.js')) {
        $watcher = [IO.FileSystemWatcher]::new((Split-Path -Parent $observed))
        $watcher.EnableRaisingEvents = $true
        $global:schedulerTestWatchers.Add($watcher)
    }
    if ($env:SCHEDULER_TEST_LOCK_ROLLBACK -and !$global:schedulerTestRollbackHeld -and $observed.EndsWith('SKILL.md')) {
        $entry = Join-Path $global:schedulerTestDiscovery 'opencode-scheduler/dist/index.js'
        if ([IO.File]::ReadAllText($entry).Contains('// rollback candidate')) {
            $global:schedulerTestFiles.Add([IO.File]::Open($entry, 'Open', 'Read', 'Read'))
            $global:schedulerTestRollbackHeld = $true
        }
    }
    $hash
}
if ($env:SCHEDULER_TEST_RETAIN_TARGETS) {
    foreach ($child in @('plugins/opencode-scheduler/dist','skills/opencode-scheduler/user-folder')) {
        $watcher = [IO.FileSystemWatcher]::new((Join-Path $env:XDG_CONFIG_HOME "opencode/$child"))
        $watcher.EnableRaisingEvents = $true
        $global:schedulerTestWatchers.Add($watcher)
    }
}
$busy = [Environment]::GetEnvironmentVariable('SCHEDULER_TEST_BUSY_FILE')
if ($busy) {
    $global:schedulerTestFiles.Add([IO.File]::Open($busy, 'Open', 'Read', 'Read'))
}
try {
if ($env:SCHEDULER_TEST_FAIL_ACL_FILE) {
    $sections = [Security.AccessControl.AccessControlSections]'Owner,Group,Access'
    $global:schedulerTestAclBefore = [Security.AccessControl.FileSecurity]::new($env:SCHEDULER_TEST_FAIL_ACL_FILE,$sections).GetSecurityDescriptorSddlForm($sections)
    # Observe a real metadata boundary without replacing the setter. A changed
    # published file loses WRITE_DAC; its original backup is never modified.
    $aclBreakpoint = Set-PSBreakpoint -Command Restore-ExactFileAccess -Action {
        $entry = $env:SCHEDULER_TEST_FAIL_ACL_FILE
        $sections = [Security.AccessControl.AccessControlSections]'Owner,Group,Access'
        $a = [Security.AccessControl.FileSecurity]::new($entry,$sections)
        if (!$global:schedulerTestAclInjected -and $a.GetSecurityDescriptorSddlForm($sections) -cne $global:schedulerTestAclBefore) {
            $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new('S-1-3-4'),'ChangePermissions','Deny'))
            if ($PSVersionTable.PSEdition -eq 'Core') { [IO.FileSystemAclExtensions]::SetAccessControl([IO.FileInfo]::new($entry),$a) }
            else { ([IO.FileInfo]::new($entry)).SetAccessControl($a) }
            $global:schedulerTestAclInjected = $true
            if ($env:SCHEDULER_TEST_LOCK_ACL_RECOVERY) {
                $global:schedulerTestFiles.Add([IO.File]::Open($entry,'Open','Read','Read'))
                $global:schedulerTestAclRecoveryHeld = $true
            }
        }
    }
}
& (Join-Path $PSScriptRoot 'expanded release/install.ps1') -DataHome $env:OPENCODE_SCHEDULER_HOME -ConfigHome $env:XDG_CONFIG_HOME -NoPath
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ([Environment]::GetEnvironmentVariable('Path','User') -cne $beforeUserPath) { [Console]::Error.WriteLine('NoPath must preserve the persistent user PATH.'); exit 1 }
if ($env:PSModulePath -cne $beforeModulePath) { [Console]::Error.WriteLine('Installer must preserve the caller module search path.'); exit 1 }
$expected = Join-Path $env:OPENCODE_SCHEDULER_HOME 'app/bin/opencode-scheduler.exe'
$command = Get-Command opencode-scheduler -ErrorAction SilentlyContinue
if (!$command -or $command.Source -ne $expected) { [Console]::Error.WriteLine('Installer must refresh process PATH to its own native command.'); exit 1 }
opencode-scheduler --version
if ($LASTEXITCODE -ne 0) { exit 1 }
} finally {
    if ($aclBreakpoint) { Remove-PSBreakpoint $aclBreakpoint }
    foreach ($file in $global:schedulerTestFiles) { $file.Dispose() }
    foreach ($watcher in $global:schedulerTestWatchers) { $watcher.Dispose() }
    if ($env:SCHEDULER_TEST_FAIL_ACL_FILE -and !$global:schedulerTestAclInjected) { throw 'ACL failure must exercise a real published-file permission change.' }
    if ($env:SCHEDULER_TEST_LOCK_ACL_RECOVERY -and !$global:schedulerTestAclRecoveryHeld) { throw 'Recovery failure must retain a real published-file handle.' }
}
`,
  );
  const run = (args: string[]) =>
    exec(native, args, { env, windowsHide: true, timeout: 120000 });
  const install = (extra: NodeJS.ProcessEnv = {}, engine = "powershell.exe") =>
    exec(engine, ["-NoProfile", "-NonInteractive", "-File", wrapper], {
      env: { ...env, ...extra },
      cwd: outside,
      windowsHide: true,
      timeout: 240000,
    });
  const security = async (path: string) =>
    (
      await exec(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-Command",
          "$ErrorActionPreference='Stop'; $s=[Security.AccessControl.AccessControlSections]'Owner,Group,Access'; $p=$env:SCHEDULER_TEST_FILE; $a=if([IO.Directory]::Exists($p)){[Security.AccessControl.DirectorySecurity]::new($p,$s)}else{[Security.AccessControl.FileSecurity]::new($p,$s)}; $a.GetSecurityDescriptorSddlForm($s)",
        ],
        { env: { ...env, SCHEDULER_TEST_FILE: path }, windowsHide: true },
      )
    ).stdout.trim();
  const stream = async (path: string) =>
    (
      await exec(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-Command",
          "$ErrorActionPreference='Stop'; [Console]::Write([string](Get-Content -LiteralPath $env:SCHEDULER_TEST_FILE -Stream user-note -Raw))",
        ],
        { env: { ...env, SCHEDULER_TEST_FILE: path }, windowsHide: true },
      )
    ).stdout;
  const recoverFixtureFile = (backup: string, path: string) =>
    exec(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$ErrorActionPreference='Stop'; Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::MoveFile($env:SCHEDULER_TEST_BACKUP,$env:SCHEDULER_TEST_FILE,$true)",
      ],
      {
        env: {
          ...env,
          SCHEDULER_TEST_BACKUP: backup,
          SCHEDULER_TEST_FILE: path,
        },
        windowsHide: true,
      },
    );
  const untouched = join(config, "opencode/opencode.jsonc");
  const policy =
    '// preserve comments and explicit disable\r\n{"plugins":["*","-opencode-scheduler"],"skills":{"example":false}}\r\n';
  await mkdir(join(config, "opencode/skills/other"), { recursive: true });
  await mkdir(join(config, "opencode/plugins/other"), { recursive: true });
  await mkdir(join(state, "opencode"), { recursive: true });
  await writeFile(untouched, policy);
  await writeFile(
    join(config, "opencode/skills/other/SKILL.md"),
    "untouched skill",
  );
  await writeFile(
    join(config, "opencode/plugins/other/index.js"),
    "untouched plugin",
  );
  let hostMode = "disabled";
  const requests: string[] = [];
  const server = createServer((req, res) => {
    requests.push(`${req.method} ${req.url}`);
    assert.equal(
      req.headers.authorization,
      `Basic ${Buffer.from("opencode:fixture-password").toString("base64")}`,
    );
    res.setHeader("Content-Type", "application/json");
    if (req.url === "/api/info")
      res.end(
        JSON.stringify({
          version: "2.2.5",
          pid: process.pid,
          urls: [],
          paths: { tmp: outside },
        }),
      );
    else if (req.url === "/api/plugin")
      res.end(
        JSON.stringify({
          location: { directory: null },
          data:
            hostMode === "disabled"
              ? []
              : [
                  {
                    id: "opencode-scheduler",
                    source: { type: "local", path: plugin },
                    features: { server: true },
                    state:
                      hostMode === "active"
                        ? { status: "active" }
                        : {
                            status: "failed",
                            error: "private fixture diagnostic",
                          },
                  },
                ],
        }),
      );
    else if (req.url === "/api/session/ses_fixture")
      res.end(
        JSON.stringify({
          location: { directory: null },
          data: { id: "ses_fixture", title: "Fixture" },
        }),
      );
    else res.writeHead(404).end("{}");
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  await writeFile(
    join(state, "opencode/service.json"),
    JSON.stringify({
      url: `http://127.0.0.1:${address.port}`,
      pid: process.pid,
      version: "2.2.5",
      password: "fixture-password",
    }),
  );
  t.after(async () => {
    if (existsSync(native)) await run(["daemon", "stop"]).catch(() => {});
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    await rm(outside, { recursive: true, force: true });
  });
  await assert.rejects(
    exec(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-File",
        join(unpacked, "install.ps1"),
        "-DataHome",
        "\\scheduler-fixture-relative",
        "-ConfigHome",
        config,
        "-NoPath",
      ],
      { env, cwd: outside, windowsHide: true, timeout: 120000 },
    ),
    (e: any) => /must be absolute/i.test(e.stdout + e.stderr),
  );
  await mkdir(plugin, { recursive: true });
  await writeFile(join(plugin, "user-entry.js"), "unfamiliar plugin");
  await assert.rejects(install(), (e: any) =>
    /ownership/i.test(e.stdout + e.stderr),
  );
  assert.ok(
    !existsSync(native),
    "Unowned files fail before native installation",
  );
  assert.equal(
    await readFile(join(plugin, "user-entry.js"), "utf8"),
    "unfamiliar plugin",
  );
  await rm(join(plugin, "user-entry.js"));
  const first = await install();
  assert.match(first.stdout, /installed/i);
  assert.match(first.stdout, /opencode-scheduler 0\.1\.0/);
  assert.match(first.stdout, /not loaded|not_loaded/i);
  assert.doesNotMatch(first.stdout, /fully ready/i);
  assert.equal(await readFile(untouched, "utf8"), policy);
  assert.equal(
    await readFile(join(config, "opencode/skills/other/SKILL.md"), "utf8"),
    "untouched skill",
  );
  assert.equal(
    await readFile(join(config, "opencode/plugins/other/index.js"), "utf8"),
    "untouched plugin",
  );
  const cfg = JSON.parse(await readFile(join(plugin, "install.json"), "utf8"));
  assert.deepEqual(cfg, {
    protocol: 1,
    binary: await realpath(native),
    data_home: await realpath(data),
  });
  const skill = await readFile(
    join(config, "opencode/skills/opencode-scheduler/SKILL.md"),
    "utf8",
  );
  assert.match(skill, /description: Use when explicitly/);
  assert.match(skill, /opencode\/autoinvoke: false/);
  assert.ok(!existsSync(join(unpacked, "plugin/install.json")));
  let status = JSON.parse((await run(["status", "--json"])).stdout).result.data;
  assert.equal(status.installed.plugin, true);
  assert.equal(status.plugin_host.state, "not_loaded");
  assert.equal(status.daemon.reachable, true);
  assert.equal(status.daemon.protocol_compatible, true);
  assert.ok(existsSync(join(data, "schedules.sqlite3")));
  const endpoint = JSON.parse(
    await readFile(join(data, "endpoint.json"), "utf8"),
  );
  const health = await fetch(`${endpoint.base_url}/v1/health`, {
    headers: {
      Authorization: `Bearer ${endpoint.token}`,
      "X-Scheduler-Instance": endpoint.instance_id,
    },
  });
  assert.equal(health.status, 200);
  assert.equal(
    ((await health.json()) as any).instance_id,
    endpoint.instance_id,
  );
  hostMode = "active";
  // The bundled client's helper inherits this Node process's service-discovery environment.
  // Isolate that environment too, not just the direct CLI/installer children.
  const oldState = process.env.XDG_STATE_HOME;
  process.env.XDG_STATE_HOME = state;
  t.after(() => {
    if (oldState === undefined) delete process.env.XDG_STATE_HOME;
    else process.env.XDG_STATE_HOME = oldState;
  });
  const { default: p } = await import(
    pathToFileURL(join(plugin, "dist/index.js")).href
  );
  const tools: any[] = [];
  const close = await p.setup({
    tool: {
      transform: async (edit: any) => {
        edit({ add: (tool: any) => tools.push(tool) });
        return { dispose: async () => {} };
      },
    },
  });
  const invoke = async (name: string, input: unknown) =>
    JSON.parse(
      (
        await tools
          .find((t) => t.name === name)
          .execute(input, { signal: new AbortController().signal })
      ).content[0].text,
    );
  const created = await invoke("scheduler_create", {
    request_id: "cc0321c3-3160-44dc-8035-6b399731f702",
    target: { kind: "id", value: "ses_fixture" },
    at: "2099-11-06T14:00:00Z",
    prompt: "literal fixture",
    delivery: "queue",
  });
  assert.equal(created.ok, true, JSON.stringify(created));
  const id = created.result.data.schedules[0].id;
  const before = JSON.parse(
    (await readFile(join(data, "endpoint.json"))).toString(),
  );
  await writeFile(join(plugin, "user-note.txt"), "preserve unfamiliar file");
  const skillRoot = join(config, "opencode/skills/opencode-scheduler");
  const busyDirectory = join(skillRoot, "user-folder");
  await mkdir(busyDirectory);
  await writeFile(
    join(busyDirectory, "keep.txt"),
    "preserve busy unfamiliar subtree",
  );
  const metadata = JSON.parse(
    await readFile(join(data, "install.json"), "utf8"),
  );
  let future: any = { marker: "preserve" };
  for (let i = 0; i < 30; i++) future = { child: future };
  metadata.future_field = ["preserve", future];
  await writeFile(join(data, "install.json"), JSON.stringify(metadata));
  const scheduleBeforeUpgrade = (await run(["schedule", "show", id, "--json"]))
    .stdout;
  const metadataBeforeUpgrade = await readFile(join(data, "install.json"));
  const manifestBeforeUpgrade = await readFile(
    join(data, "product-install.json"),
  );
  const descriptorBeforeUpgrade = await readFile(join(plugin, "install.json"));
  const retainTargets = { SCHEDULER_TEST_RETAIN_TARGETS: "1" };
  // Existing private installations need not grant every managed file FullControl.
  // A replacement must retain inherited Modify, mixed explicit/inherited rules,
  // and protected rules rather than importing its staging parent's permissions.
  await exec(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `$ErrorActionPreference='Stop'
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User
$dist=Join-Path $env:XDG_CONFIG_HOME 'opencode/plugins/opencode-scheduler/dist'
$a=[Security.AccessControl.DirectorySecurity]::new($dist,[Security.AccessControl.AccessControlSections]::Access)
$a.SetAccessRuleProtection($true,$false)
$a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'Modify','ContainerInherit,ObjectInherit','None','Allow'))
([IO.DirectoryInfo]::new($dist)).SetAccessControl($a)
foreach($path in @((Join-Path $env:XDG_CONFIG_HOME 'opencode/plugins/opencode-scheduler/install.json'),(Join-Path $env:OPENCODE_SCHEDULER_HOME 'product-install.json'))) {
    $a=[Security.AccessControl.FileSecurity]::new()
    $a.SetAccessRuleProtection($true,$false)
    $a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'Modify','Allow'))
    ([IO.FileInfo]::new($path)).SetAccessControl($a)
}
$path=Join-Path $env:XDG_CONFIG_HOME 'opencode/skills/opencode-scheduler/SKILL.md'
$a=[Security.AccessControl.FileSecurity]::new($path,[Security.AccessControl.AccessControlSections]::Access)
$a.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid,'Read','Allow'))
([IO.FileInfo]::new($path)).SetAccessControl($a)`,
    ],
    { env, windowsHide: true, timeout: 120000 },
  );
  const payloadAcls = async () => {
    const result = await exec(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$ErrorActionPreference='Stop'; Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Security/Microsoft.PowerShell.Security.psd1'); $paths=$env:SCHEDULER_TEST_ACL_PATHS | ConvertFrom-Json; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $acls=@(foreach($path in $paths){$a=Get-Acl -LiteralPath $path; $rules=@($a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])); $other=@($rules | Where-Object { $_.AccessControlType -eq 'Allow' -and $_.IdentityReference.Value -ne $sid }); if($other.Count){throw 'Payload must remain private'}; @{owner=$a.Owner; group=$a.Group; protected=$a.AreAccessRulesProtected; sddl=$a.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]'Owner,Group,Access'); rules=@($rules | Select-Object @{n='sid';e={$_.IdentityReference.Value}},FileSystemRights,AccessControlType,InheritanceFlags,PropagationFlags,IsInherited)}}); ConvertTo-Json -InputObject $acls -Depth 5",
      ],
      {
        env: {
          ...env,
          SCHEDULER_TEST_ACL_PATHS: JSON.stringify([
            plugin,
            join(plugin, "dist"),
            join(plugin, "dist/index.js"),
            join(plugin, "install.json"),
            skillRoot,
            join(skillRoot, "SKILL.md"),
            data,
            join(data, "schedules.sqlite3"),
            join(data, "product-install.json"),
          ]),
        },
        windowsHide: true,
      },
    );
    return JSON.parse(result.stdout);
  };
  await t.test(
    "shared managed payload and metadata are refused before installation writes",
    async () => {
      const lock = join(config, "opencode/.scheduler-install.lock");
      for (const [path, inherited] of [
        [join(plugin, "dist/index.js"), false],
        [join(plugin, "dist/index.js"), true],
        [join(plugin, "install.json"), false],
        [join(skillRoot, "SKILL.md"), false],
        [join(data, "product-install.json"), false],
        [join(data, "install.json"), false],
      ] as const) {
        const aclPath = inherited ? join(plugin, "dist") : path;
        const originalBytes = await readFile(path);
        const endpointBytes = await readFile(join(data, "endpoint.json"));
        const nativeBytes = await readFile(native);
        await rm(lock, { force: true });
        const shared = await exec(
          "powershell.exe",
          [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference='Stop'; $p=$env:SCHEDULER_TEST_FILE; $s=[Security.AccessControl.AccessControlSections]::Access; $sid=[Security.Principal.SecurityIdentifier]::new('S-1-1-0'); if($env:SCHEDULER_TEST_INHERITED){$a=[Security.AccessControl.DirectorySecurity]::new($p,$s); $r=[Security.AccessControl.FileSystemAccessRule]::new($sid,'Read','ObjectInherit','None','Allow')}else{$a=[Security.AccessControl.FileSecurity]::new($p,$s); $r=[Security.AccessControl.FileSystemAccessRule]::new($sid,'Read','Allow')}; [Convert]::ToBase64String($a.GetSecurityDescriptorBinaryForm()); $a.AddAccessRule($r); (Get-Item -LiteralPath $p).SetAccessControl($a)",
          ],
          {
            env: {
              ...env,
              SCHEDULER_TEST_FILE: aclPath,
              ...(inherited ? { SCHEDULER_TEST_INHERITED: "1" } : {}),
            },
            windowsHide: true,
          },
        );
        const sharedSecurity = await security(path);
        const sharedParent = inherited ? await security(aclPath) : null;
        try {
          await assert.rejects(install(), (e: any) =>
            /shared managed file/i.test(e.stderr),
          );
          assert.equal(await security(path), sharedSecurity);
          if (inherited) assert.equal(await security(aclPath), sharedParent);
          assert.deepEqual(await readFile(path), originalBytes);
          assert.deepEqual(await readFile(native), nativeBytes);
          assert.deepEqual(
            await readFile(join(data, "endpoint.json")),
            endpointBytes,
          );
          assert.equal(
            existsSync(lock),
            false,
            "Refusal precedes even lock creation",
          );
          assert.equal(
            (await readdir(join(config, "opencode"))).some((name) =>
              name.startsWith(".scheduler-stage-"),
            ),
            false,
          );
        } finally {
          await exec(
            "powershell.exe",
            [
              "-NoProfile",
              "-NonInteractive",
              "-Command",
              "$ErrorActionPreference='Stop'; $p=$env:SCHEDULER_TEST_FILE; $a=if([IO.Directory]::Exists($p)){[Security.AccessControl.DirectorySecurity]::new()}else{[Security.AccessControl.FileSecurity]::new()}; $a.SetSecurityDescriptorBinaryForm([Convert]::FromBase64String($env:SCHEDULER_TEST_SECURITY),[Security.AccessControl.AccessControlSections]::Access); (Get-Item -LiteralPath $p).SetAccessControl($a)",
            ],
            {
              env: {
                ...env,
                SCHEDULER_TEST_FILE: aclPath,
                SCHEDULER_TEST_SECURITY: shared.stdout.trim(),
              },
              windowsHide: true,
            },
          );
        }
      }
    },
  );
  await exec(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "$ErrorActionPreference='Stop'; Set-Content -LiteralPath $env:SCHEDULER_TEST_FILE -Stream user-note -NoNewline -Value 'preserve unfamiliar stream'",
    ],
    {
      env: { ...env, SCHEDULER_TEST_FILE: join(plugin, "dist/index.js") },
      windowsHide: true,
    },
  );
  const beforeAcls = await payloadAcls();
  // A normal repeat must not rename the loaded target, even for identical payload.
  await t.test(
    "identical reinstall tolerates retained target child handles",
    async () => {
      await install(retainTargets);
      assert.deepEqual(
        await readFile(join(data, "product-install.json")),
        manifestBeforeUpgrade,
      );
      assert.deepEqual(await payloadAcls(), beforeAcls);
      assert.equal(
        await stream(join(plugin, "dist/index.js")),
        "preserve unfamiliar stream",
      );
    },
  );
  const sourceExe = join(unpacked, "bin/opencode-scheduler.exe");
  const originalExe = await readFile(sourceExe);
  // An inert PE overlay gives the upgrade candidate distinct bytes without a second build pipeline.
  const candidateExe = Buffer.concat([
    originalExe,
    Buffer.from("fixture upgrade overlay"),
  ]);
  await writeFile(sourceExe, candidateExe);
  const sourceBundle = join(unpacked, "plugin/dist/index.js");
  const bundlePath = join(plugin, "dist/index.js");
  const originalBundle = await readFile(sourceBundle);
  const candidateBundle = Buffer.concat([
    originalBundle,
    Buffer.from("\n// loaded-target upgrade candidate\n"),
  ]);
  const skillSource = join(unpacked, "skills/opencode-scheduler/SKILL.md");
  const candidateSkill = skill + "\nLoaded-target upgrade fixture.\n";
  await writeFile(sourceBundle, candidateBundle);
  await writeFile(skillSource, candidateSkill);
  await install(retainTargets);
  assert.deepEqual(await readFile(native), candidateExe);
  assert.deepEqual(await readFile(bundlePath), candidateBundle);
  assert.equal(
    await readFile(join(skillRoot, "SKILL.md"), "utf8"),
    candidateSkill,
  );
  assert.deepEqual(
    await readFile(join(plugin, "install.json")),
    descriptorBeforeUpgrade,
  );
  assert.deepEqual(
    JSON.parse(await readFile(join(data, "install.json"), "utf8")),
    JSON.parse(metadataBeforeUpgrade.toString()),
  );
  assert.deepEqual(await payloadAcls(), beforeAcls);
  assert.equal(await stream(bundlePath), "preserve unfamiliar stream");
  assert.equal(
    (await run(["schedule", "show", id, "--json"])).stdout,
    scheduleBeforeUpgrade,
  );
  assert.equal(await readFile(untouched, "utf8"), policy);
  assert.equal(
    await readFile(join(busyDirectory, "keep.txt"), "utf8"),
    "preserve busy unfamiliar subtree",
  );
  const manifest = JSON.parse(
    await readFile(join(data, "product-install.json"), "utf8"),
  );
  for (const [key, hash] of Object.entries(manifest.files)) {
    const path = key.startsWith("plugin/")
      ? join(plugin, key.slice(7))
      : join(skillRoot, key.slice(6));
    assert.equal(
      createHash("sha256")
        .update(await readFile(path))
        .digest("hex")
        .toUpperCase(),
      hash,
    );
  }
  let after = JSON.parse(await readFile(join(data, "endpoint.json"), "utf8"));
  assert.notEqual(after.instance_id, before.instance_id);
  assert.equal(
    (await invoke("scheduler_show", { id })).result.data.state,
    "pending",
  );
  assert.equal(
    (await invoke("scheduler_status", {})).result.data.plugin_host.state,
    "active",
  );
  assert.deepEqual(
    JSON.parse(await readFile(join(data, "install.json"), "utf8")).future_field,
    ["preserve", future],
  );
  assert.equal(
    await readFile(join(plugin, "user-note.txt"), "utf8"),
    "preserve unfamiliar file",
  );
  for (const locked of [false, true]) {
    await t.test(
      `ACL restoration failure ${locked ? "retains the private original when recovery is locked" : "restores the original without another ACL merge"}`,
      async () => {
        const beforeBundle = await readFile(bundlePath);
        const beforeManifest = await readFile(
          join(data, "product-install.json"),
        );
        const candidate = Buffer.concat([
          beforeBundle,
          Buffer.from("\n// acl restoration candidate\n"),
        ]);
        await writeFile(sourceBundle, candidate);
        let hint = "";
        try {
          await assert.rejects(
            install({
              ...retainTargets,
              SCHEDULER_TEST_FAIL_ACL_FILE: bundlePath,
              ...(locked ? { SCHEDULER_TEST_LOCK_ACL_RECOVERY: "1" } : {}),
            }),
            (e: any) => {
              hint = e.stderr;
              return (
                /permission metadata could not be preserved/i.test(hint) &&
                (!locked || /rollback needs manual inspection/i.test(hint))
              );
            },
          );
        } finally {
          await writeFile(sourceBundle, beforeBundle);
        }
        if (locked) {
          assert.deepEqual(await readFile(bundlePath), candidate);
          const parent = join(config, "opencode");
          const stages = (await readdir(parent)).filter((name) =>
            name.startsWith(".scheduler-stage-"),
          );
          assert.equal(stages.length, 1);
          const stage = join(parent, stages[0]);
          const backups = (await readdir(stage)).filter((name) =>
            name.startsWith(".previous-"),
          );
          assert.equal(backups.length, 1);
          const backup = join(stage, backups[0]);
          assert.deepEqual(await readFile(backup), beforeBundle);
          assert.equal(await security(backup), beforeAcls[2].sddl);
          assert.equal(await stream(backup), "preserve unfamiliar stream");
          assert.ok(hint.includes(await realpath(backup)));
          await exec(
            "powershell.exe",
            [
              "-NoProfile",
              "-NonInteractive",
              "-Command",
              "$ErrorActionPreference='Stop'; $a=[Security.AccessControl.DirectorySecurity]::new($env:SCHEDULER_TEST_STAGE,[Security.AccessControl.AccessControlSections]::Access); $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; if(!$a.AreAccessRulesProtected){throw 'Recovery stage must be protected'}; foreach($r in $a.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])){if($r.AccessControlType -eq 'Allow' -and !$r.IdentityReference.Equals($sid)){throw 'Recovery stage must be private'}}; Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::MoveFile($env:SCHEDULER_TEST_BACKUP,$env:SCHEDULER_TEST_FILE,$true)",
            ],
            {
              env: {
                ...env,
                SCHEDULER_TEST_STAGE: stage,
                SCHEDULER_TEST_BACKUP: backup,
                SCHEDULER_TEST_FILE: bundlePath,
              },
              windowsHide: true,
            },
          );
          await rm(stage, { recursive: true });
        }
        assert.deepEqual(await readFile(bundlePath), beforeBundle);
        assert.deepEqual(
          await readFile(join(data, "product-install.json")),
          beforeManifest,
        );
        assert.deepEqual(await payloadAcls(), beforeAcls);
        assert.equal(await stream(bundlePath), "preserve unfamiliar stream");
        assert.equal(
          (await readdir(join(config, "opencode"))).some((name) =>
            name.startsWith(".scheduler-stage-"),
          ),
          false,
        );
      },
    );
  }
  for (const locked of [false, true]) {
    await t.test(
      `private JSON ACL failure ${locked ? "retains its original when metadata recovery is locked" : "restores its original and rolls back payload changes"}`,
      async () => {
        const path = join(data, "product-install.json");
        const original = path + ".fixture-original";
        // Model a real first-installed legacy descriptor with no auto-inherited bit.
        // Keep the fixture's prior descriptor/file intact for cleanup afterward.
        await exec(
          "powershell.exe",
          [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference='Stop'; $p=$env:SCHEDULER_TEST_FILE; $tmp=$p+'.fixture-candidate'; [IO.File]::WriteAllBytes($tmp,[IO.File]::ReadAllBytes($p)); [IO.File]::Move($p,$p+'.fixture-original'); [IO.File]::Move($tmp,$p); Set-Content -LiteralPath $p -Stream user-note -NoNewline -Value 'preserve unfamiliar stream'",
          ],
          { env: { ...env, SCHEDULER_TEST_FILE: path }, windowsHide: true },
        );
        try {
          const before = await readFile(path);
          const acls = await payloadAcls();
          const bundle = await readFile(bundlePath);
          let hint = "";
          await assert.rejects(
            install({
              ...retainTargets,
              SCHEDULER_TEST_FAIL_ACL_FILE: path,
              ...(locked ? { SCHEDULER_TEST_LOCK_ACL_RECOVERY: "1" } : {}),
            }),
            (e: any) => {
              hint = e.stderr;
              return (
                /permission metadata could not be preserved/i.test(hint) &&
                (!locked ||
                  /metadata rollback needs manual inspection/i.test(hint))
              );
            },
          );
          if (locked) {
            const backups = (await readdir(data)).filter((name) =>
              name.startsWith("product-install.json.previous-"),
            );
            assert.equal(backups.length, 1);
            const backup = join(data, backups[0]);
            assert.deepEqual(await readFile(backup), before);
            assert.equal(await security(backup), acls[8].sddl);
            assert.ok(hint.includes(await realpath(backup)));
            assert.notEqual(await security(path), acls[8].sddl);
            await recoverFixtureFile(backup, path);
          }
          assert.deepEqual(await readFile(path), before);
          assert.deepEqual(await payloadAcls(), acls);
          assert.deepEqual(await readFile(bundlePath), bundle);
          assert.equal(
            (await readdir(data)).some((name) =>
              name.startsWith("product-install.json.previous-"),
            ),
            false,
          );
          assert.equal(
            (await readdir(join(config, "opencode"))).some((name) =>
              name.startsWith(".scheduler-stage-"),
            ),
            false,
          );
        } finally {
          await recoverFixtureFile(original, path);
        }
        assert.deepEqual(await payloadAcls(), beforeAcls);
      },
    );
  }
  await t.test(
    "unreplaceable skill file rolls back the changed plugin despite retained directory handles",
    async () => {
      const busy = join(skillRoot, "SKILL.md");
      const manifestPath = join(data, "product-install.json");
      const metadataPath = join(data, "install.json");
      const beforeManifest = await readFile(manifestPath);
      const beforeMetadata = await readFile(metadataPath);
      const beforeDescriptor = await readFile(join(plugin, "install.json"));
      const bundlePath = join(plugin, "dist/index.js");
      const sourceBundle = join(unpacked, "plugin/dist/index.js");
      const beforeBundle = await readFile(bundlePath);
      const beforeSchedule = (await run(["schedule", "show", id, "--json"]))
        .stdout;
      await writeFile(
        sourceBundle,
        Buffer.concat([beforeBundle, Buffer.from("\n// rollback candidate\n")]),
      );
      try {
        await assert.rejects(
          install({ ...retainTargets, SCHEDULER_TEST_BUSY_FILE: busy }),
          (e: any) => /activation failed/i.test(e.stdout + e.stderr),
        );
      } finally {
        await writeFile(sourceBundle, beforeBundle);
      }
      assert.deepEqual(await readFile(manifestPath), beforeManifest);
      assert.deepEqual(await readFile(metadataPath), beforeMetadata);
      assert.deepEqual(
        await readFile(join(plugin, "install.json")),
        beforeDescriptor,
      );
      assert.deepEqual(await readFile(bundlePath), beforeBundle);
      assert.deepEqual(await payloadAcls(), beforeAcls);
      assert.equal(
        await readFile(join(skillRoot, "SKILL.md"), "utf8"),
        candidateSkill,
      );
      assert.equal(
        await readFile(join(busyDirectory, "keep.txt"), "utf8"),
        "preserve busy unfamiliar subtree",
      );
      assert.equal(
        await readFile(join(plugin, "user-note.txt"), "utf8"),
        "preserve unfamiliar file",
      );
      assert.equal(await readFile(untouched, "utf8"), policy);
      assert.equal(
        (await run(["schedule", "show", id, "--json"])).stdout,
        beforeSchedule,
      );
      for (const parent of [
        join(config, "opencode"),
        join(config, "opencode/plugins"),
        join(config, "opencode/skills"),
      ]) {
        assert.equal(
          (await readdir(parent)).some(
            (name) =>
              name.startsWith(".scheduler-stage-") ||
              name.includes(".previous-"),
          ),
          false,
        );
      }
    },
  );
  await t.test(
    "failed rollback retains the private original-file backup and reports it",
    async () => {
      const beforeBundle = await readFile(bundlePath);
      const beforeManifest = await readFile(join(data, "product-install.json"));
      const rollbackCandidate = Buffer.concat([
        beforeBundle,
        Buffer.from("\n// rollback candidate\n"),
      ]);
      await writeFile(sourceBundle, rollbackCandidate);
      let failureHint = "";
      try {
        await assert.rejects(
          install({
            ...retainTargets,
            SCHEDULER_TEST_BUSY_FILE: join(skillRoot, "SKILL.md"),
            SCHEDULER_TEST_LOCK_ROLLBACK: "1",
          }),
          (e: any) => {
            failureHint = e.stderr;
            return /rollback needs manual inspection/i.test(failureHint);
          },
        );
      } finally {
        await writeFile(sourceBundle, beforeBundle);
      }
      assert.deepEqual(await readFile(bundlePath), rollbackCandidate);
      assert.deepEqual(
        await readFile(join(data, "product-install.json")),
        beforeManifest,
      );
      const stageParent = join(config, "opencode");
      const retained = (await readdir(stageParent)).filter((name) =>
        name.startsWith(".scheduler-stage-"),
      );
      assert.equal(
        retained.length,
        1,
        "Do not erase a backup needed for recovery",
      );
      const stage = join(stageParent, retained[0]);
      const backups = (await readdir(stage)).filter((name) =>
        name.startsWith(".previous-"),
      );
      assert.equal(backups.length, 1);
      assert.deepEqual(await readFile(join(stage, backups[0])), beforeBundle);
      assert.ok(failureHint.includes(await realpath(join(stage, backups[0]))));
      assert.deepEqual(await payloadAcls(), beforeAcls);
      assert.equal(
        await readFile(join(busyDirectory, "keep.txt"), "utf8"),
        "preserve busy unfamiliar subtree",
      );
      assert.equal(
        (await run(["schedule", "show", id, "--json"])).stdout,
        scheduleBeforeUpgrade,
      );
      // Recover only this disposable fixture after proving the manual-inspection state.
      await writeFile(bundlePath, beforeBundle);
      await rm(stage, { recursive: true });
    },
  );
  await t.test(
    "failed repair removes only newly created managed files and directories",
    async () => {
      const beforeBundle = await readFile(bundlePath);
      const beforeManifest = await readFile(join(data, "product-install.json"));
      await rm(join(plugin, "dist"), { recursive: true });
      await assert.rejects(
        install({ SCHEDULER_TEST_BUSY_FILE: join(skillRoot, "SKILL.md") }),
        (e: any) => /activation failed/i.test(e.stderr),
      );
      assert.equal(existsSync(join(plugin, "dist")), false);
      assert.deepEqual(
        await readFile(join(data, "product-install.json")),
        beforeManifest,
      );
      assert.equal(
        await readFile(join(plugin, "user-note.txt"), "utf8"),
        "preserve unfamiliar file",
      );
      await install();
      assert.deepEqual(await readFile(bundlePath), beforeBundle);
      const repairedAcls = await payloadAcls();
      // Neither a missing directory nor its missing file has a descriptor to
      // preserve. Their replacements are private; surviving descriptors are exact.
      assert.equal(repairedAcls[1].protected, true);
      assert.equal(repairedAcls[2].protected, false);
      assert.equal(repairedAcls[2].rules.length, 1);
      assert.equal(repairedAcls[2].rules[0].IsInherited, true);
      assert.equal(repairedAcls[2].rules[0].FileSystemRights, 2032127);
      assert.deepEqual(
        repairedAcls.filter(
          (_: unknown, index: number) => index !== 1 && index !== 2,
        ),
        beforeAcls.filter(
          (_: unknown, index: number) => index !== 1 && index !== 2,
        ),
      );
    },
  );
  await t.test(
    "direct PowerShell 7 reinstall preserves owned payload and private ACLs",
    async () => {
      const acls = await payloadAcls();
      const bundle = await readFile(bundlePath);
      const skill = await readFile(join(skillRoot, "SKILL.md"));
      await install(retainTargets, "pwsh.exe");
      assert.deepEqual(await readFile(bundlePath), bundle);
      assert.deepEqual(await readFile(join(skillRoot, "SKILL.md")), skill);
      assert.deepEqual(
        JSON.parse(await readFile(join(plugin, "install.json"), "utf8")),
        cfg,
      );
      assert.deepEqual(await payloadAcls(), acls);
      assert.equal(await readFile(untouched, "utf8"), policy);
      assert.equal(
        (await run(["schedule", "show", id, "--json"])).stdout,
        scheduleBeforeUpgrade,
      );
    },
  );
  after = JSON.parse(await readFile(join(data, "endpoint.json"), "utf8"));
  hostMode = "failed";
  const human = (await run(["status"])).stdout;
  assert.match(human, /Plugin host: failed/);
  assert.doesNotMatch(human, /private fixture diagnostic/);
  await close();
  const skillBytes = await readFile(skillSource);
  await rm(skillSource);
  await assert.rejects(install(), (e: any) =>
    /payload is incomplete/i.test(e.stdout + e.stderr),
  );
  await writeFile(skillSource, skillBytes);
  assert.equal(
    JSON.parse(await readFile(join(data, "endpoint.json"), "utf8")).instance_id,
    after.instance_id,
  );
  await writeFile(
    join(data, "endpoint.json"),
    JSON.stringify({ ...after, token: "cd".repeat(48) }),
  );
  try {
    await assert.rejects(install(), (e: any) =>
      /authentication|credentials/i.test(e.stdout + e.stderr),
    );
  } finally {
    await writeFile(join(data, "endpoint.json"), JSON.stringify(after));
  }
  assert.equal(
    (await run(["schedule", "show", id, "--json"])).stdout.includes(
      '"pending"',
    ),
    true,
  );
  assert.equal(
    JSON.parse(await readFile(join(data, "endpoint.json"), "utf8")).instance_id,
    after.instance_id,
  );
  // User edits and malformed payloads must fail before stopping the running scheduler.
  await writeFile(join(plugin, "dist/index.js"), "user modified");
  await assert.rejects(install(), (e: any) =>
    /modified|ownership/i.test(e.stdout + e.stderr),
  );
  assert.equal(
    JSON.parse(await readFile(join(data, "endpoint.json"), "utf8")).instance_id,
    after.instance_id,
  );
  assert.equal(
    await readFile(join(plugin, "dist/index.js"), "utf8"),
    "user modified",
  );
  assert.ok(
    requests.every((r) => r.startsWith("GET ")),
    "Installer/status never reload, restart or prompt the host",
  );
});
