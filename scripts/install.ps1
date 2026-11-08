# Run from a trusted, expanded Windows x64 release. No administrator rights required.
[CmdletBinding()]
param(
    [string]$DataHome = $env:OPENCODE_SCHEDULER_HOME,
    [string]$ConfigHome = $env:XDG_CONFIG_HOME,
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:problem = 'Installation failed. Hint: check free disk space and directory ownership, preserve scheduler data, then rerun this installer.'
$stages = @()
$backups = @()
$createdDirectories = @()
$retainedStages = @()
$productLock = $null

function Stop-Install([string]$Message) {
    $script:problem = $Message
    throw 'Installation stopped.'
}
function Assert-RealPath([string]$Path) {
    $part = $Path
    while ($part) {
        # Inspect the entry itself, including dangling reparse points. Test-Path can
        # report a missing target instead and let a linked descendant pass preflight.
        $item = $null
        try { $item = Get-Item -LiteralPath $part -Force -ErrorAction Stop }
        catch [System.Management.Automation.ItemNotFoundException] { }
        if ($item -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            Stop-Install 'Installation path contains a link. Hint: select real directories; no files were intentionally replaced through a link.'
        }
        $parent = Split-Path -Parent $part
        if ($parent -eq $part) { break }
        $part = $parent
    }
}
function Assert-RealTree([string]$Path) {
    Assert-RealPath $Path
    if (Test-Path -LiteralPath $Path) {
        foreach ($item in Get-ChildItem -LiteralPath $Path -Force -Recurse) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                Stop-Install 'Payload or installation contains a link. Hint: use an expanded release and real directories; preserve unfamiliar files separately.'
            }
        }
    }
}
function New-PrivateDirectory([string]$Path) {
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
    $directory = [IO.DirectoryInfo]::new($Path)
    # Core moved ACL-aware creation to this built-in extension; both engines
    # apply the private descriptor at creation, never after publication.
    if ($PSVersionTable.PSEdition -eq 'Core') { [IO.FileSystemAclExtensions]::Create($directory, $acl) }
    else { $directory.Create($acl) }
}
function Read-Json([string]$Path) {
    if ((Get-Item -LiteralPath $Path).Length -gt 65536) { Stop-Install 'Installation metadata is too large. Hint: preserve it and inspect it locally before repairing installation ownership.' }
    Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}
function Write-PrivateJson([string]$Path, $Value) {
    # All callers write within a directory already protected for the current user.
    $tmp = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($tmp, ($Value | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
        if (Test-Path -LiteralPath $Path) { [IO.File]::Replace($tmp, $Path, [NullString]::Value) }
        else { [IO.File]::Move($tmp, $Path) }
    } finally { if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force } }
}

try {
    if ($env:OS -ne 'Windows_NT') { Stop-Install 'This installer supports Windows x64 only. Hint: use the portable core build on other platforms; global installation there is not certified.' }
    # A modern-shell/Node/Windows-PowerShell hop can inherit another engine's
    # module search path. Resolve built-in hashing from this engine, locally.
    Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1') -Scope Local -NoClobber
    if (!$DataHome) { $DataHome = Join-Path $env:LOCALAPPDATA 'opencode-scheduler' }
    if (!$ConfigHome) { $ConfigHome = Join-Path $HOME '.config' }
    foreach ($path in @($DataHome, $ConfigHome)) {
        # IsPathRooted alone also accepts drive-relative and root-relative paths.
        if (![IO.Path]::IsPathRooted($path) -or [IO.Path]::GetPathRoot($path).Length -lt 3) {
            Stop-Install 'Installation directories must be absolute Windows paths. Hint: pass -DataHome and -ConfigHome with full directory paths.'
        }
        if ([IO.Path]::GetFullPath($path).TrimEnd('\', '/') -eq [IO.Path]::GetPathRoot($path).TrimEnd('\', '/')) {
            Stop-Install 'Installation directories must be below a volume or share root. Hint: select dedicated private-data and config directories, not a filesystem root.'
        }
        Assert-RealPath $path
    }
    $DataHome = [IO.Path]::GetFullPath($DataHome).TrimEnd('\', '/')
    $ConfigHome = [IO.Path]::GetFullPath($ConfigHome).TrimEnd('\', '/')
    $release = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\', '/')
    foreach ($path in @($DataHome, $ConfigHome)) {
        if ($path.Equals($release, [StringComparison]::OrdinalIgnoreCase) -or $path.StartsWith($release + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Runtime installation must be outside the expanded release. Hint: use the default private data and global config directories.'
        }
    }
    $pluginTarget = Join-Path $ConfigHome 'opencode/plugins/opencode-scheduler'
    $skillTarget = Join-Path $ConfigHome 'opencode/skills/opencode-scheduler'
    $binary = Join-Path $DataHome 'app/bin/opencode-scheduler.exe'
    foreach ($target in @($pluginTarget, $skillTarget)) {
        if ($DataHome.Equals($target, [StringComparison]::OrdinalIgnoreCase) -or $DataHome.StartsWith($target + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Data-home overlaps a swapped plugin or skill directory. Hint: select a data directory outside both installed payload subtrees; no installation directories were created.'
        }
    }
    # The reused native installer stages/replaces the executable and may restart its
    # daemon. Check every fixed path those operations touch before any product writes.
    # Random candidate/metadata temporary names use these already-checked directories.
    foreach ($relative in @('app/bin/opencode-scheduler.exe', 'app/bin/opencode-scheduler.previous.exe',
        'install.lock', 'install.json', 'startup.lock', 'daemon.lock', 'endpoint.json',
        'diagnostic.log', 'diagnostic.log.1', 'schedules.sqlite3', 'schedules.sqlite3-wal',
        'schedules.sqlite3-shm', 'schedules.sqlite3-journal', 'product-install.json')) {
        $nativePath = Join-Path $DataHome $relative
        if ($ConfigHome.Equals($nativePath, [StringComparison]::OrdinalIgnoreCase) -or $ConfigHome.StartsWith($nativePath + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Config-home overlaps a native executable or control-file path. Hint: use a dedicated config directory that does not occupy a native file path; no installation directories were created.'
        }
        Assert-RealPath $nativePath
    }
    $pluginFiles = @('package.json', 'dist/index.js', 'LICENSE', 'THIRD-PARTY-NOTICES.txt')
    $skillFiles = @('SKILL.md')
    Assert-RealTree $release
    foreach ($file in @('bin/opencode-scheduler.exe', 'README.md', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', 'install.ps1') + @($pluginFiles | ForEach-Object { "plugin/$_" }) + @('skills/opencode-scheduler/SKILL.md')) {
        if (!(Test-Path -LiteralPath (Join-Path $release $file) -PathType Leaf)) {
            Stop-Install 'Release payload is incomplete. Hint: expand the complete audited Windows x64 ZIP into a fresh directory and run its install.ps1.'
        }
    }
    if (Test-Path -LiteralPath (Join-Path $release 'plugin/install.json')) {
        Stop-Install 'Release contains private installation configuration. Hint: obtain a clean public release; do not distribute runtime configuration.'
    }
    $package = Read-Json (Join-Path $release 'plugin/package.json')
    if ($package.name -ne 'opencode-scheduler-plugin' -or $package.type -ne 'module' -or $package.exports.'.' -ne './dist/index.js') {
        Stop-Install 'Plugin package entrypoint is invalid. Hint: use the complete audited release without editing its payload.'
    }
    foreach ($field in @('dependencies', 'optionalDependencies', 'peerDependencies', 'bundledDependencies')) {
        if ($package.PSObject.Properties[$field] -and @($package.$field.PSObject.Properties).Count -gt 0) {
            Stop-Install 'Plugin payload requires unshipped dependencies. Hint: obtain the standalone audited release.'
        }
    }
    $skillText = Get-Content -LiteralPath (Join-Path $release 'skills/opencode-scheduler/SKILL.md') -Raw
    if ($skillText -notmatch '(?m)^description: Use when explicitly' -or $skillText -notmatch '(?m)^  opencode/autoinvoke: false\r?$') {
        Stop-Install 'Skill is not explicit-only. Hint: use the unmodified audited release; automatic scheduling is not enabled by this installer.'
    }
    $sourceBinary = Join-Path $release 'bin/opencode-scheduler.exe'
    $version = & $sourceBinary --version 2>&1
    if ($LASTEXITCODE -ne 0 -or "$version" -notmatch '^opencode-scheduler \d+\.\d+\.\d+') {
        Stop-Install 'Native payload could not be validated. Hint: use a trusted Windows x64 release compatible with this machine.'
    }
    $manifestPath = Join-Path $DataHome 'product-install.json'
    # Serialize the complete product operation, including preflight and native upgrade.
    # This empty lock file contains no paths, configuration or credentials.
    $lockPath = Join-Path $ConfigHome 'opencode/.scheduler-install.lock'
    Assert-RealPath $lockPath
    New-Item -ItemType Directory -Path (Split-Path -Parent $lockPath) -Force | Out-Null
    $script:problem = 'Another product installation may be running, or the config directory is not writable. Hint: let that installer finish and check directory access before retrying.'
    $productLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
    $script:problem = 'Installation preflight failed. Hint: check directory ownership, free space and installation metadata locally; preserve existing files.'
    $manifest = $null
    if (Test-Path -LiteralPath $manifestPath) {
        Assert-RealPath $manifestPath
        $manifest = Read-Json $manifestPath
        if ($manifest.protocol -ne 1 -or $manifest.plugin_path -ne $pluginTarget -or $manifest.skill_path -ne $skillTarget) {
            Stop-Install 'Existing installation ownership does not match these directories. Hint: rerun with the original -DataHome and -ConfigHome; preserve existing files.'
        }
    }
    $targets = @(@{ Name = 'plugin'; Path = $pluginTarget; Source = (Join-Path $release 'plugin'); Files = $pluginFiles }, @{ Name = 'skill'; Path = $skillTarget; Source = (Join-Path $release 'skills/opencode-scheduler'); Files = $skillFiles })
    foreach ($target in $targets) {
        Assert-RealTree $target.Path
        if (Test-Path -LiteralPath $target.Path) {
            if (!$manifest -and @(Get-ChildItem -LiteralPath $target.Path -Force).Count -gt 0) {
                Stop-Install 'Existing plugin or skill has no installer ownership record. Hint: preserve the unfamiliar directory and resolve ownership before installing; nothing there will be overwritten.'
            }
            foreach ($name in @($target.Files) + $(if ($target.Name -eq 'plugin') { 'install.json' })) {
                $file = Join-Path $target.Path $name
                if ($manifest -and (Test-Path -LiteralPath $file)) {
                    $key = "$($target.Name)/$name"
                    $owned = $manifest.files.PSObject.Properties[$key]
                    if (!$owned -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $owned.Value) {
                        Stop-Install 'An installer-managed file was modified or its ownership is unknown. Hint: preserve your edit and restore the original owned file before upgrading; unfamiliar user work is not overwritten.'
                    }
                }
            }
        }
        $parent = Split-Path -Parent $target.Path
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
        # Keep incomplete modules outside discovery. A consumer can retain a handle
        # to a staged descendant (e.g. dist), preventing Windows from renaming its
        # ancestor even with delete-sharing. This checked parent is on the same volume.
        $stage = Join-Path (Split-Path -Parent $lockPath) ('.scheduler-stage-' + [guid]::NewGuid().ToString('N'))
        New-PrivateDirectory $stage
        $stages += $stage
        foreach ($name in $target.Files) {
            $destination = Join-Path $stage $name
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            Copy-Item -LiteralPath (Join-Path $target.Source $name) -Destination $destination -Force
            if ((Get-FileHash -LiteralPath $destination).Hash -ne (Get-FileHash -LiteralPath (Join-Path $target.Source $name)).Hash) { Stop-Install 'Staged payload validation failed. Hint: check the disk and re-expand the trusted release before retrying.' }
        }
        $target.Stage = $stage
    }
    Write-PrivateJson (Join-Path $targets[0].Stage 'install.json') @{ protocol = 1; binary = $binary; data_home = $DataHome }
    # Delegate all native locks, private data ACL checks, PATH registration and authenticated
    # stop/replace/restart to the existing native installer. Never manage the host service.
    $env:OPENCODE_SCHEDULER_HOME = $DataHome
    $env:XDG_CONFIG_HOME = $ConfigHome
    $nativeArgs = @('install', '--json')
    if ($NoPath) { $nativeArgs += '--no-path' }
    $script:problem = 'Native installation failed. Hint: run opencode-scheduler status, inspect diagnostic.log locally and check ownership/free space; preserve the database and do not stop unrelated processes.'
    $nativeOutput = & $sourceBinary @nativeArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        $reply = "$nativeOutput" | ConvertFrom-Json
        if ($reply.error.message) { $script:problem = $reply.error.message }
        throw 'Native installation failed.'
    }
    $script:problem = 'Native command is installed, but plugin/skill activation failed. Hint: preserve scheduler data and existing files, check directory access or files in use, then rerun this installer with the same directories.'
    $hashes = @{}
    foreach ($target in $targets) {
        $names = @($target.Files) + $(if ($target.Name -eq 'plugin') { 'install.json' })
        foreach ($name in $names) {
            $hashes["$($target.Name)/$name"] = (Get-FileHash -LiteralPath (Join-Path $target.Stage $name)).Hash
        }
        $hadTarget = Test-Path -LiteralPath $target.Path
        if ($hadTarget -and $manifest) {
            # A loaded host can retain descendant-directory handles that prevent an
            # ancestor rename. Keep existing directories/unfamiliar work in place;
            # ReplaceFile (via .NET) atomically replaces each owned leaf, preserving
            # its ACL. Backups stay private and outside discovery on the same volume.
            foreach ($name in $names) {
                $destination = Join-Path $target.Path $name
                $parent = Split-Path -Parent $destination
                if (!(Test-Path -LiteralPath $parent)) {
                    New-PrivateDirectory $parent
                    $createdDirectories += $parent
                }
                $backup = Join-Path $target.Stage ('.previous-' + [guid]::NewGuid().ToString('N'))
                $exists = Test-Path -LiteralPath $destination
                $backups += @{ Kind = 'file'; Path = $destination; Backup = $backup; HadTarget = $exists; Activated = $false; Stage = $target.Stage }
                if ($exists) { [IO.File]::Replace((Join-Path $target.Stage $name), $destination, $backup) }
                else { [IO.File]::Move((Join-Path $target.Stage $name), $destination) }
                $backups[-1].Activated = $true
            }
        } else {
            # First publication still moves a complete private directory. An existing
            # unowned directory passed preflight only if empty, so no work is replaced.
            $backup = "$($target.Path).previous-$([guid]::NewGuid().ToString('N'))"
            if ($hadTarget) { [IO.Directory]::Move($target.Path, $backup) }
            $backups += @{ Kind = 'directory'; Path = $target.Path; Backup = $backup; HadTarget = $hadTarget; Activated = $false; Stage = $target.Stage }
            [IO.Directory]::Move($target.Stage, $target.Path)
            $backups[-1].Activated = $true
        }
    }
    # Keep product ownership separate: only the native installer reads/writes native
    # install.json, preserving even unfamiliar deeply nested metadata without PowerShell conversion.
    Write-PrivateJson $manifestPath @{ protocol = 1; plugin_path = $pluginTarget; skill_path = $skillTarget; files = $hashes }
    # Remove committed backups; their private stages are cleaned in finally.
    foreach ($backup in $backups) { if ($backup.HadTarget) { Remove-Item -LiteralPath $backup.Backup -Recurse -Force -ErrorAction SilentlyContinue } }
    $backups = @()
    # Complete product installation starts even a previously absent scheduler.
    # The returned status authenticates the installed daemon; host observation is
    # read-only and must not bootstrap or restart OpenCode.
    $script:problem = 'Payload installed, but scheduler startup could not be verified. Hint: run opencode-scheduler status, then opencode-scheduler daemon start if absent; inspect diagnostic.log locally and preserve existing data.'
    $startText = & $binary daemon start --json 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'Scheduler startup failed.' }
    $started = $startText | ConvertFrom-Json
    if (!$started.ok -or !$started.result.data.daemon.reachable -or !$started.result.data.daemon.protocol_compatible) {
        throw 'Scheduler startup was not verified.'
    }
    $bin = Split-Path -Parent $binary
    $others = @($env:PATH -split ';' | Where-Object { $_.Trim().Trim('"').TrimEnd('\', '/') -ine $bin })
    $env:PATH = (@($bin) + $others) -join ';'
    Write-Output 'Native command, plugin payload and explicit-only @opencode-scheduler skill installed. Existing schedules and unrelated configuration preserved.'
    if (!$NoPath) { Write-Output 'This PowerShell process PATH is refreshed. Open a new terminal for other already-running shells; no OpenCode restart is needed.' }
    else { Write-Output 'User PATH registration skipped; only this process PATH is refreshed. Other terminals need manual PATH management.' }
    # Config/plugin directories are watched. Observe only; full location reload cancels
    # pending forms/permissions, so it is not an automatic installation step.
    $hostState = 'unknown'
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        try {
            $statusText = & $binary status --json 2>$null
            if ($LASTEXITCODE -eq 0) { $hostState = ($statusText | ConvertFrom-Json).result.data.plugin_host.state }
        } catch { $hostState = 'unknown' }
        if ($hostState -eq 'active') { break }
        if ($attempt -lt 2) { Start-Sleep -Seconds 1 }
    }
    if ($hostState -eq 'active') { Write-Output 'Plugin host: active in the queried host location. Explicit skill loading and actual tool invocation still require host acceptance checks.' }
    else { Write-Output "Plugin host: $hostState (not loaded or not verified; installed payload is not full readiness). Hint: check OpenCode's plugin list and explicit disable policy in the intended location, wait for watched discovery and rerun opencode-scheduler status. No disable settings were changed and no host service was restarted." }
} catch {
    # Roll back completed payload operations only; native upgrade and data are retained.
    [array]::Reverse($backups)
    foreach ($backup in $backups) {
        try {
            if ($backup.Kind -eq 'file') {
                if ($backup.HadTarget -and (Test-Path -LiteralPath $backup.Backup)) {
                    if (Test-Path -LiteralPath $backup.Path) { [IO.File]::Replace($backup.Backup, $backup.Path, [NullString]::Value) }
                    else { [IO.File]::Move($backup.Backup, $backup.Path) }
                } elseif ($backup.Activated) {
                    if ($backup.HadTarget) { throw 'Payload backup is missing.' }
                    [IO.File]::Delete($backup.Path)
                }
            } else {
                if ($backup.Activated) { Remove-Item -LiteralPath $backup.Path -Recurse -Force }
                if ($backup.HadTarget) { [IO.Directory]::Move($backup.Backup, $backup.Path) }
            }
        } catch {
            $retainedStages += $backup.Stage
            $script:problem += " Payload rollback needs manual inspection at '$($backup.Path)'; preserve any installer backup at '$($backup.Backup)'."
        }
    }
    [array]::Reverse($createdDirectories)
    foreach ($directory in $createdDirectories) {
        try { [IO.Directory]::Delete($directory, $false) }
        catch { $script:problem += ' A newly created payload directory could not be removed; preserve unfamiliar files and inspect it locally.' }
    }
    [Console]::Error.WriteLine($script:problem)
    exit 1
} finally {
    if ($productLock) { $productLock.Dispose() }
    foreach ($stage in $stages) { if ($stage -notin $retainedStages -and (Test-Path -LiteralPath $stage)) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue } }
}
