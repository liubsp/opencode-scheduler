[CmdletBinding()]
param([switch]$PayloadOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $PSScriptRoot 'install.ps1'
if (!$PayloadOnly -and !(Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw 'scripts/install.ps1 is required for a complete release. Use -PayloadOnly to build the audited staging payload.'
}
if ($env:OS -ne 'Windows_NT') { throw 'The release builder targets Windows x64 only.' }

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

$oldFlags = $env:RUSTFLAGS
$oldEncoded = $env:CARGO_ENCODED_RUSTFLAGS
$oldPrefixes = $env:SCHEDULER_PRIVATE_PREFIXES
$dist = Join-Path $root 'dist'
$payload = Join-Path $dist 'payload'
$scratch = Join-Path $root '.tmp/release'
Push-Location $root
try {
    $sysroot = (& rustc --print sysroot).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve Rust sysroot.' }
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
    $prefixes = @($HOME, $cargoHome, [IO.Path]::GetTempPath().TrimEnd('\', '/'), $env:TEMP, $env:TMP, $sysroot, $root) | Where-Object { $_ } | Select-Object -Unique
    $flags = @('-C', 'debuginfo=0', '-C', 'strip=debuginfo', '-C', 'link-arg=/DEBUG:NONE')
    $index = 0
    foreach ($prefix in $prefixes) {
        $index++
        $flags += "--remap-path-prefix=$prefix=/build/$index"
        $flags += "--remap-path-prefix=$($prefix.Replace('\', '/'))=/build/$index"
    }
    $env:RUSTFLAGS = $null
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31
    $env:SCHEDULER_PRIVATE_PREFIXES = $prefixes -join '|'
    Invoke-Checked cargo @('build', '--locked', '--release', '--target', 'x86_64-pc-windows-msvc')
    Invoke-Checked npm.cmd @('ci', '--prefix', 'plugin')
    Invoke-Checked npm.cmd @('run', 'build', '--prefix', 'plugin')
    New-Item -ItemType Directory -Path $scratch -Force | Out-Null
    # npm pack ignores --prefix: its working directory must be the plugin itself.
    Push-Location (Join-Path $root 'plugin')
    try {
        $packed = & npm.cmd pack --ignore-scripts --json --pack-destination $scratch
        if ($LASTEXITCODE -ne 0) { throw 'npm pack failed.' }
        $pack = ($packed | ConvertFrom-Json)[0]
    } finally { Pop-Location }
    $unpacked = Join-Path $scratch 'unpacked'
    if (Test-Path $unpacked) { Remove-Item -LiteralPath $unpacked -Recurse -Force }
    New-Item -ItemType Directory -Path $unpacked -Force | Out-Null
    Invoke-Checked tar @('-xzf', (Join-Path $scratch $pack.filename), '-C', $unpacked)
    Invoke-Checked node @('scripts/check-package.mjs', (Join-Path $unpacked 'package'), '--plugin')
    if (Test-Path $payload) { Remove-Item -LiteralPath $payload -Recurse -Force }
    New-Item -ItemType Directory -Path (Join-Path $payload 'bin'), (Join-Path $payload 'skills/opencode-scheduler') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'target/x86_64-pc-windows-msvc/release/opencode-scheduler.exe') -Destination (Join-Path $payload 'bin/opencode-scheduler.exe')
    Copy-Item -LiteralPath (Join-Path $unpacked 'package') -Destination (Join-Path $payload 'plugin') -Recurse
    Copy-Item -LiteralPath (Join-Path $root 'skills/opencode-scheduler/SKILL.md') -Destination (Join-Path $payload 'skills/opencode-scheduler/SKILL.md')
    foreach ($name in @('README.md', 'LICENSE')) { Copy-Item -LiteralPath (Join-Path $root $name) -Destination $payload }
    Invoke-Checked node @('scripts/notices.mjs', (Join-Path $payload 'THIRD-PARTY-NOTICES.txt'))
    if ($PayloadOnly) {
        Invoke-Checked node @('scripts/check-package.mjs', $payload)
        Write-Output 'Audited staging payload: dist/payload (not a complete installer release).'
    } else {
        Copy-Item -LiteralPath $installer -Destination (Join-Path $payload 'install.ps1')
        Invoke-Checked node @('scripts/check-package.mjs', $payload, '--installer')
        $version = (Get-Content -LiteralPath (Join-Path $root 'plugin/package.json') -Raw | ConvertFrom-Json).version
        $archive = Join-Path $dist "opencode-scheduler-$version-windows-x64.zip"
        Compress-Archive -Path (Join-Path $payload '*') -DestinationPath $archive -Force
        $expanded = Join-Path $scratch 'expanded-zip'
        if (Test-Path $expanded) { Remove-Item -LiteralPath $expanded -Recurse -Force }
        Expand-Archive -LiteralPath $archive -DestinationPath $expanded
        Invoke-Checked node @('scripts/check-package.mjs', $expanded, '--installer')
        Write-Output "Audited release archive: dist/opencode-scheduler-$version-windows-x64.zip"
    }
} finally {
    $env:RUSTFLAGS = $oldFlags
    $env:CARGO_ENCODED_RUSTFLAGS = $oldEncoded
    $env:SCHEDULER_PRIVATE_PREFIXES = $oldPrefixes
    Pop-Location
}
