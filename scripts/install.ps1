# Run from a trusted, expanded Windows x64 release. No administrator rights required.
[CmdletBinding()]
param(
    [string]$DataHome = $env:OPENCODE_SCHEDULER_HOME,
    [string]$ConfigHome = $env:XDG_CONFIG_HOME,
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:problem = 'Installation failed. Hint: check free space and directory ownership; preserve scheduler data before retrying.'
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
            Stop-Install 'Installation path contains a link. Hint: select real directories.'
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
                Stop-Install 'Payload or installation contains a link. Hint: use an expanded release and real directories.'
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
    if ((Get-Item -LiteralPath $Path).Length -gt 65536) { Stop-Install 'Installation metadata is too large. Hint: preserve it for local inspection before repairing ownership.' }
    Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}
function Read-PrivateFileSecurity([string]$Path) {
    $acl = [Security.AccessControl.FileSecurity]::new($Path, [Security.AccessControl.AccessControlSections]'Owner,Group,Access')
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $raw = [Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
    if ($null -eq $raw.DiscretionaryAcl) { Stop-Install "Existing shared managed file at '$Path'. Hint: preserve it, review sharing locally and restore current-user-only access before reinstalling." }
    foreach ($ace in $raw.DiscretionaryAcl) {
        # Inspect qualified ACEs directly, including callback/object Allow ACEs
        # that the convenience GetAccessRules projection can omit.
        if ($ace -isnot [Security.AccessControl.QualifiedAce]) { Stop-Install "Cannot verify private permissions at '$Path'. Hint: preserve the file and inspect its permissions locally." }
        if ($ace.AceQualifier -eq 'AccessAllowed' -and !$ace.SecurityIdentifier.Equals($sid)) {
            Stop-Install "Existing shared managed file at '$Path'. Hint: preserve it, review sharing locally and restore current-user-only access before reinstalling."
        }
    }
    return $acl
}
function Restore-ExactFileAccess([string]$Path, [Security.AccessControl.FileSecurity]$Original) {
    # Modern ACL setters recompute inheritance from the current parent; ReplaceFile
    # has already merged the private stage's inherited rules. The file-specific
    # legacy API applies the saved DACL without that second inheritance calculation.
    # AUTO_INHERIT_REQ retains its already-computed auto-inheritance control state.
    # Verify the full owner/group/Access descriptor; there is no permissive fallback.
    $raw = [Security.AccessControl.RawSecurityDescriptor]::new($Original.GetSecurityDescriptorBinaryForm(), 0)
    $raw.SetFlags($raw.ControlFlags -bor [Security.AccessControl.ControlFlags]::DiscretionaryAclAutoInheritRequired)
    $bytes = [byte[]]::new($raw.BinaryLength)
    $raw.GetBinaryForm($bytes, 0)
    [SchedulerInstaller.FileAcl]::SetDacl($Path, $bytes)
}
function Replace-PrivateFile([string]$Source, [string]$Destination, [string]$Backup) {
    $sections = [Security.AccessControl.AccessControlSections]'Owner,Group,Access'
    $original = Read-PrivateFileSecurity $Destination
    $expected = $original.GetSecurityDescriptorSddlForm($sections)
    # ReplaceFile retains DACL metadata, not the old owner/group. Prepare only
    # differing ownership fields while the incoming file is still private.
    $incoming = [Security.AccessControl.FileSecurity]::new($Source, [Security.AccessControl.AccessControlSections]'Owner,Group')
    $changed = $false
    $sidType = [Security.Principal.SecurityIdentifier]
    if (!$incoming.GetOwner($sidType).Equals($original.GetOwner($sidType))) { $incoming.SetOwner($original.GetOwner($sidType)); $changed = $true }
    if (!$incoming.GetGroup($sidType).Equals($original.GetGroup($sidType))) { $incoming.SetGroup($original.GetGroup($sidType)); $changed = $true }
    if ($changed) {
        $file = [IO.FileInfo]::new($Source)
        if ($PSVersionTable.PSEdition -eq 'Core') { [IO.FileSystemAclExtensions]::SetAccessControl($file, $incoming) }
        else { $file.SetAccessControl($incoming) }
    }
    # An original backup is mandatory before any fallible post-publication step.
    # Only current-user Allow ACEs can be merged into an unpublished candidate.
    [IO.File]::Replace($Source, $Destination, $Backup)
    try {
        $actual = [Security.AccessControl.FileSecurity]::new($Destination, $sections)
        if ($actual.GetSecurityDescriptorSddlForm($sections) -cne $expected) {
            Restore-ExactFileAccess $Destination $original
            $actual = [Security.AccessControl.FileSecurity]::new($Destination, $sections)
            if ($actual.GetSecurityDescriptorSddlForm($sections) -cne $expected) { throw 'File permission verification failed.' }
        }
    } catch {
        $script:problem = 'Payload activation failed: permission metadata could not be preserved. Hint: keep any reported recovery backup; check permissions and files in use before retrying.'
        throw
    }
}
function Restore-OriginalFile([string]$Backup, [string]$Path) {
    # File.Replace established the same-volume backup. This built-in no-UI
    # Windows overwrite-rename restores the original file, streams and descriptor
    # without merging ACLs or a fallible metadata write after consuming it.
    [Microsoft.VisualBasic.FileIO.FileSystem]::MoveFile($Backup, $Path, $true)
}
function Write-PrivateJson([string]$Path, $Value) {
    # All callers write within a directory already protected for the current user.
    $tmp = "$Path.$([guid]::NewGuid().ToString('N')).tmp"
    $backup = "$Path.previous-$([guid]::NewGuid().ToString('N'))"
    $retainBackup = $false
    try {
        [IO.File]::WriteAllText($tmp, ($Value | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
        if (Test-Path -LiteralPath $Path) {
            try { Replace-PrivateFile $tmp $Path $backup }
            catch {
                if (Test-Path -LiteralPath $backup) {
                    try { Restore-OriginalFile $backup $Path }
                    catch {
                        $retainBackup = $true
                        $script:problem += " Metadata rollback needs manual inspection at '$Path'; preserve the original private backup at '$backup'."
                    }
                }
                throw
            }
        } else { [IO.File]::Move($tmp, $Path) }
    } finally {
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue }
        if (!$retainBackup -and (Test-Path -LiteralPath $backup)) { Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue }
    }
}

try {
    if ($env:OS -ne 'Windows_NT') { Stop-Install 'This installer supports Windows x64 only.' }
    # A modern-shell/Node/Windows-PowerShell hop can inherit another engine's
    # module search path. Resolve built-in hashing from this engine, locally.
    Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1') -Scope Local -NoClobber
    if (!$DataHome) { $DataHome = Join-Path $env:LOCALAPPDATA 'opencode-scheduler' }
    if (!$ConfigHome) { $ConfigHome = Join-Path $HOME '.config' }
    foreach ($path in @($DataHome, $ConfigHome)) {
        # IsPathRooted alone also accepts drive-relative and root-relative paths.
        if (![IO.Path]::IsPathRooted($path) -or [IO.Path]::GetPathRoot($path).Length -lt 3) {
            Stop-Install 'Installation directories must be absolute Windows paths. Hint: use full paths for -DataHome and -ConfigHome.'
        }
        if ([IO.Path]::GetFullPath($path).TrimEnd('\', '/') -eq [IO.Path]::GetPathRoot($path).TrimEnd('\', '/')) {
            Stop-Install 'Installation directories cannot be filesystem roots. Hint: select dedicated data and config directories.'
        }
        Assert-RealPath $path
    }
    $DataHome = [IO.Path]::GetFullPath($DataHome).TrimEnd('\', '/')
    $ConfigHome = [IO.Path]::GetFullPath($ConfigHome).TrimEnd('\', '/')
    $release = [IO.Path]::GetFullPath($PSScriptRoot).TrimEnd('\', '/')
    foreach ($path in @($DataHome, $ConfigHome)) {
        if ($path.Equals($release, [StringComparison]::OrdinalIgnoreCase) -or $path.StartsWith($release + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Installation must be outside the expanded release. Hint: use the default data and config directories.'
        }
    }
    $pluginTarget = Join-Path $ConfigHome 'opencode/plugins/opencode-scheduler'
    $skillTarget = Join-Path $ConfigHome 'opencode/skills/opencode-scheduler'
    $binary = Join-Path $DataHome 'app/bin/opencode-scheduler.exe'
    foreach ($target in @($pluginTarget, $skillTarget)) {
        if ($DataHome.Equals($target, [StringComparison]::OrdinalIgnoreCase) -or $DataHome.StartsWith($target + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Data-home overlaps a plugin or skill directory. Hint: choose a data directory outside both.'
        }
    }
    # The reused native installer stages/replaces the executable and may restart its
    # daemon. Check every fixed path those operations touch before any product writes.
    # Random candidate/metadata temporary names use these already-checked directories.
    $privateFiles = @()
    foreach ($relative in @('app/bin/opencode-scheduler.exe', 'app/bin/opencode-scheduler.previous.exe',
        'install.lock', 'install.json', 'startup.lock', 'daemon.lock', 'endpoint.json',
        'diagnostic.log', 'diagnostic.log.1', 'schedules.sqlite3', 'schedules.sqlite3-wal',
        'schedules.sqlite3-shm', 'schedules.sqlite3-journal', 'product-install.json')) {
        $nativePath = Join-Path $DataHome $relative
        if ($ConfigHome.Equals($nativePath, [StringComparison]::OrdinalIgnoreCase) -or $ConfigHome.StartsWith($nativePath + '\', [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Install 'Config-home overlaps a native file path. Hint: choose a separate config directory.'
        }
        Assert-RealPath $nativePath
        $privateFiles += $nativePath
    }
    $pluginFiles = @('package.json', 'dist/index.js', 'LICENSE', 'THIRD-PARTY-NOTICES.txt')
    $skillFiles = @('SKILL.md')
    foreach ($name in @($pluginFiles) + @('install.json')) { $privateFiles += Join-Path $pluginTarget $name }
    foreach ($name in $skillFiles) { $privateFiles += Join-Path $skillTarget $name }
    # Check every managed leaf before even creating the product lock or stages.
    # An Administrators owner is not sharing: only outsider Allow ACEs are refused.
    foreach ($file in $privateFiles) {
        Assert-RealPath $file
        if (Test-Path -LiteralPath $file -PathType Leaf) { [void](Read-PrivateFileSecurity $file) }
    }
    Assert-RealTree $release
    foreach ($file in @('bin/opencode-scheduler.exe', 'README.md', 'LICENSE', 'THIRD-PARTY-NOTICES.txt', 'install.ps1') + @($pluginFiles | ForEach-Object { "plugin/$_" }) + @('skills/opencode-scheduler/SKILL.md')) {
        if (!(Test-Path -LiteralPath (Join-Path $release $file) -PathType Leaf)) {
            Stop-Install 'Release payload is incomplete. Hint: expand the complete Windows x64 ZIP into a fresh directory.'
        }
    }
    if (Test-Path -LiteralPath (Join-Path $release 'plugin/install.json')) {
        Stop-Install 'Release contains private installation configuration. Hint: obtain a clean public release; do not share runtime configuration.'
    }
    $package = Read-Json (Join-Path $release 'plugin/package.json')
    if ($package.name -ne 'opencode-scheduler-plugin' -or $package.type -ne 'module' -or $package.exports.'.' -ne './dist/index.js') {
        Stop-Install 'Invalid plugin entrypoint. Hint: use an unmodified release.'
    }
    foreach ($field in @('dependencies', 'optionalDependencies', 'peerDependencies', 'bundledDependencies')) {
        if ($package.PSObject.Properties[$field] -and @($package.$field.PSObject.Properties).Count -gt 0) {
            Stop-Install 'Plugin requires unshipped dependencies. Hint: obtain the standalone release.'
        }
    }
    $skillText = Get-Content -LiteralPath (Join-Path $release 'skills/opencode-scheduler/SKILL.md') -Raw
    if ($skillText -notmatch '(?m)^description: Use when explicitly' -or $skillText -notmatch '(?m)^  opencode/autoinvoke: false\r?$') {
        Stop-Install 'Skill is not explicit-only. Hint: use an unmodified release.'
    }
    $sourceBinary = Join-Path $release 'bin/opencode-scheduler.exe'
    $version = & $sourceBinary --version 2>&1
    if ($LASTEXITCODE -ne 0 -or "$version" -notmatch '^opencode-scheduler \d+\.\d+\.\d+') {
        Stop-Install 'Cannot validate native payload. Hint: use a trusted Windows x64 release.'
    }
    $manifestPath = Join-Path $DataHome 'product-install.json'
    Add-Type -AssemblyName Microsoft.VisualBasic
    if (!('SchedulerInstaller.FileAcl' -as [type])) {
        Add-Type -TypeDefinition @'
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace SchedulerInstaller {
    public static class FileAcl {
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetFileSecurityW(string path, uint information, byte[] descriptor);
        public static void SetDacl(string path, byte[] descriptor) {
            if (!SetFileSecurityW(path, 4, descriptor)) // DACL_SECURITY_INFORMATION only
                throw new Win32Exception(Marshal.GetLastWin32Error());
        }
    }
}
'@
    }
    # Serialize the complete product operation, including preflight and native upgrade.
    # This empty lock file contains no paths, configuration or credentials.
    $lockPath = Join-Path $ConfigHome 'opencode/.scheduler-install.lock'
    Assert-RealPath $lockPath
    New-Item -ItemType Directory -Path (Split-Path -Parent $lockPath) -Force | Out-Null
    $script:problem = 'Installation lock unavailable. Hint: let other installers finish and check config-directory write access.'
    $productLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
    # Recheck after serialization; the initial check is what makes refusal no-write.
    foreach ($file in $privateFiles) { if (Test-Path -LiteralPath $file -PathType Leaf) { [void](Read-PrivateFileSecurity $file) } }
    $script:problem = 'Installation preflight failed. Hint: preserve existing files; check ownership, free space and metadata locally.'
    $manifest = $null
    if (Test-Path -LiteralPath $manifestPath) {
        Assert-RealPath $manifestPath
        $manifest = Read-Json $manifestPath
        if ($manifest.protocol -ne 1 -or $manifest.plugin_path -ne $pluginTarget -or $manifest.skill_path -ne $skillTarget) {
            Stop-Install 'Installation ownership does not match these directories. Hint: rerun with the original -DataHome and -ConfigHome.'
        }
    }
    $targets = @(@{ Name = 'plugin'; Path = $pluginTarget; Source = (Join-Path $release 'plugin'); Files = $pluginFiles }, @{ Name = 'skill'; Path = $skillTarget; Source = (Join-Path $release 'skills/opencode-scheduler'); Files = $skillFiles })
    foreach ($target in $targets) {
        Assert-RealTree $target.Path
        if (Test-Path -LiteralPath $target.Path) {
            if (!$manifest -and @(Get-ChildItem -LiteralPath $target.Path -Force).Count -gt 0) {
                Stop-Install 'Existing plugin or skill has no ownership record. Hint: preserve the directory and resolve ownership before installing.'
            }
            foreach ($name in @($target.Files) + $(if ($target.Name -eq 'plugin') { 'install.json' })) {
                $file = Join-Path $target.Path $name
                if ($manifest -and (Test-Path -LiteralPath $file)) {
                    $key = "$($target.Name)/$name"
                    $owned = $manifest.files.PSObject.Properties[$key]
                    if (!$owned -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $owned.Value) {
                        Stop-Install 'Managed file was modified or ownership is unknown. Hint: preserve your edit and restore the original file before upgrading.'
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
            if ((Get-FileHash -LiteralPath $destination).Hash -ne (Get-FileHash -LiteralPath (Join-Path $target.Source $name)).Hash) { Stop-Install 'Staged payload validation failed. Hint: check the disk and re-expand the trusted release.' }
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
    $script:problem = 'Native installation failed. Hint: run opencode-scheduler status; inspect diagnostic.log locally and check ownership/free space. Preserve the database.'
    $nativeOutput = & $sourceBinary @nativeArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        $reply = "$nativeOutput" | ConvertFrom-Json
        if ($reply.error.message) { $script:problem = $reply.error.message }
        throw 'Native installation failed.'
    }
    $script:problem = 'CLI installed, but plugin/skill activation failed. Hint: check directory access and files in use, then rerun with the same directories. Preserve existing data.'
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
            # ReplaceFile retains streams/attributes; explicitly verify/restore its
            # merged DACL while the original stays private outside discovery.
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
                if ($exists) { Replace-PrivateFile (Join-Path $target.Stage $name) $destination $backup }
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
    $script:problem = 'Payload installed; scheduler startup not verified. Hint: run opencode-scheduler status, then daemon start if absent. Inspect diagnostic.log locally if startup fails.'
    $startText = & $binary daemon start --json 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'Scheduler startup failed.' }
    $started = $startText | ConvertFrom-Json
    if (!$started.ok -or !$started.result.data.daemon.reachable -or !$started.result.data.daemon.protocol_compatible) {
        throw 'Scheduler startup was not verified.'
    }
    $bin = Split-Path -Parent $binary
    $others = @($env:PATH -split ';' | Where-Object { $_.Trim().Trim('"').TrimEnd('\', '/') -ine $bin })
    $env:PATH = (@($bin) + $others) -join ';'
    Write-Output 'Installed CLI, plugin payload and @opencode-scheduler skill.'
    if (!$NoPath) { Write-Output 'PATH refreshed here; open a new terminal for other running shells.' }
    else { Write-Output 'PATH refreshed here only; user PATH registration skipped.' }
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
    if ($hostState -eq 'active') { Write-Output 'Plugin host: active in the queried location.' }
    else { Write-Output "Plugin host: $hostState. Hint: check the intended location's plugin list and disable policy; wait for discovery, then rerun opencode-scheduler status." }
} catch {
    # Roll back completed payload operations only; native upgrade and data are retained.
    [array]::Reverse($backups)
    foreach ($backup in $backups) {
        try {
            if ($backup.Kind -eq 'file') {
                if ($backup.HadTarget -and (Test-Path -LiteralPath $backup.Backup)) {
                    Restore-OriginalFile $backup.Backup $backup.Path
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
