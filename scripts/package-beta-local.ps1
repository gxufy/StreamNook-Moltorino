<#
.SYNOPSIS
  Build a local-only StreamNook beta with a staged Bluzyrino chat runtime.

.DESCRIPTION
  Validates a pinned, externally staged Bluzyrino runtime and its generated
  manifest, stages it as chat-runtime\ for Tauri, builds an isolated NSIS beta,
  assembles a portable ZIP, and generates local checksums.

  LOCAL TESTING ONLY. Nothing here uploads, publishes, tags, or commits.

.NOTES
  Written for Windows PowerShell 5.1 compatibility.
#>
[CmdletBinding()]
param(
    [string] $RuntimeRoot = "C:\Dev\Bluzyrino_staged",
    [string] $BetaVersion = "8.4.0-beta.2",
    [string] $OutputRoot = "C:\Dev\StreamNook-Bluzyrino-Staging\app-output"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot       = Split-Path -Parent $PSScriptRoot
$SrcTauri       = Join-Path $RepoRoot "src-tauri"
$StagingDir     = Join-Path $SrcTauri "target\beta-package-resources"
$BetaConfig     = Join-Path $SrcTauri "tauri.beta.conf.json"
$CargoBinName   = "StreamNook"
$RuntimeDirName = "chat-runtime"
$RuntimeExeName = "Bluzyrino.exe"
$ExpectedExeSha = "aa4b2101ffab24d271361d1b25c01026d8b61bfcda3e32b08d932262021af6ed"
$PortableName   = "StreamNook-Bluzyrino-$BetaVersion-windows-x64"

function Fail([string] $Message) {
    Write-Host "FATAL: $Message" -ForegroundColor Red
    exit 1
}

function Section([string] $Title) {
    Write-Host ""
    Write-Host "=== $Title ===" -ForegroundColor Cyan
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-SafeRelativePath([string] $RelativePath) {
    if ([string]::IsNullOrWhiteSpace($RelativePath)) {
        Fail "Manifest contains an empty path."
    }
    $normalized = $RelativePath.Replace("\", "/")
    if ($normalized -ne $RelativePath) {
        Fail "Manifest path must use forward slashes: $RelativePath"
    }
    if ([IO.Path]::IsPathRooted($RelativePath) -or $RelativePath.Contains(":")) {
        Fail "Manifest path is rooted or contains a drive designator: $RelativePath"
    }
    $segments = $RelativePath.Split("/")
    if ($segments -contains "" -or $segments -contains "." -or $segments -contains "..") {
        Fail "Manifest path contains an unsafe segment: $RelativePath"
    }
}

function Test-RuntimeManifest([string] $Root) {
    $resolvedRoot = (Resolve-Path -LiteralPath $Root -ErrorAction SilentlyContinue)
    if (-not $resolvedRoot) { Fail "RuntimeRoot not found: $Root" }
    $resolvedRoot = $resolvedRoot.Path.TrimEnd("\")

    $manifestPath = Join-Path $resolvedRoot "runtime-manifest.json"
    $entrypoint = Join-Path $resolvedRoot $RuntimeExeName
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        Fail "Runtime manifest missing: $manifestPath"
    }
    if (-not (Test-Path -LiteralPath $entrypoint -PathType Leaf)) {
        Fail "Runtime entrypoint missing: $entrypoint"
    }

    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if ($manifest.runtime_id -ne "bluzyrino") { Fail "runtime_id must be 'bluzyrino'." }
    if ($manifest.version -ne "2.0.3") { Fail "Runtime version must be 2.0.3; got '$($manifest.version)'." }
    if ($manifest.entrypoint -ne $RuntimeExeName) { Fail "Unexpected runtime entrypoint '$($manifest.entrypoint)'." }
    if ($manifest.architecture -ne "x86_64") { Fail "Runtime architecture must be x86_64; got '$($manifest.architecture)'." }
    if ($manifest.archive_root -ne $RuntimeDirName) { Fail "archive_root must be '$RuntimeDirName'." }

    $exeSha = Get-Sha256 $entrypoint
    if ($exeSha -ne $ExpectedExeSha) {
        Fail "$RuntimeExeName SHA-256 mismatch. Expected $ExpectedExeSha; got $exeSha."
    }

    $rootPrefix = $resolvedRoot + "\"
    $seen = @{}
    $manifestBytes = [int64]0
    foreach ($file in @($manifest.files)) {
        $relative = [string]$file.path
        Assert-SafeRelativePath $relative
        $key = $relative.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { Fail "Duplicate manifest path: $relative" }
        $seen[$key] = $true

        $fullPath = [IO.Path]::GetFullPath((Join-Path $resolvedRoot $relative.Replace("/", "\")))
        if (-not $fullPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            Fail "Manifest path escapes runtime root: $relative"
        }
        if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
            Fail "Manifest file missing: $relative"
        }
        $item = Get-Item -LiteralPath $fullPath
        if ([int64]$item.Length -ne [int64]$file.size) {
            Fail "Manifest size mismatch for $relative."
        }
        if ((Get-Sha256 $fullPath) -ne ([string]$file.sha256).ToLowerInvariant()) {
            Fail "Manifest hash mismatch for $relative."
        }
        $manifestBytes += [int64]$item.Length
    }

    $diskFiles = @(Get-ChildItem -LiteralPath $resolvedRoot -Recurse -File |
        Where-Object { $_.FullName -ne $manifestPath })
    if ($diskFiles.Count -ne @($manifest.files).Count) {
        Fail "Disk file count ($($diskFiles.Count)) != manifest count ($(@($manifest.files).Count))."
    }
    foreach ($diskFile in $diskFiles) {
        $relative = $diskFile.FullName.Substring($rootPrefix.Length).Replace("\", "/")
        if (-not $seen.ContainsKey($relative.ToLowerInvariant())) {
            Fail "Unlisted runtime file found: $relative"
        }
    }
    if ([int64]$manifest.file_count -ne $diskFiles.Count) { Fail "Manifest file_count is incorrect." }
    if ([int64]$manifest.total_size_bytes -ne $manifestBytes) { Fail "Manifest total_size_bytes is incorrect." }

    return [PSCustomObject]@{
        Root = $resolvedRoot
        ManifestPath = $manifestPath
        ManifestSha = Get-Sha256 $manifestPath
        Version = [string]$manifest.version
        Architecture = [string]$manifest.architecture
        FileCount = $diskFiles.Count
        TotalBytes = $manifestBytes
        ExeSha = $exeSha
    }
}

Section "A. Validate staged Bluzyrino runtime"
$runtime = Test-RuntimeManifest $RuntimeRoot
Write-Host "runtime root : $($runtime.Root)"
Write-Host "version      : $($runtime.Version)"
Write-Host "architecture : $($runtime.Architecture)"
Write-Host "files        : $($runtime.FileCount)"
Write-Host "bytes        : $($runtime.TotalBytes)"
Write-Host "exe SHA-256  : $($runtime.ExeSha)"
Write-Host "Manifest validation passed." -ForegroundColor Green

if (-not (Test-Path -LiteralPath $BetaConfig -PathType Leaf)) { Fail "Beta config missing: $BetaConfig" }
$betaCfg = Get-Content -Raw -LiteralPath $BetaConfig | ConvertFrom-Json
if ($betaCfg.version -ne $BetaVersion) { Fail "Beta config version '$($betaCfg.version)' != '$BetaVersion'." }
if ($betaCfg.productName -ne "StreamNook Bluzyrino") { Fail "Unexpected beta productName." }
if ($betaCfg.identifier -ne "com.gxufy.streamnook-moltorino.beta") { Fail "Unexpected beta identifier." }
if ($betaCfg.bundle.active -ne $true) { Fail "Beta bundle must be active." }
if (($betaCfg.bundle.targets -join ",") -ne "nsis") { Fail "Beta target must be exactly NSIS." }
if ($betaCfg.bundle.windows.nsis.installMode -ne "currentUser") { Fail "NSIS installMode must be currentUser." }
$resProp = $betaCfg.bundle.resources.PSObject.Properties["target/beta-package-resources/"]
if ($null -eq $resProp -or $resProp.Value -ne "") { Fail "Unexpected beta resource mapping." }

Section "B. Recreate generated package resources"
$expectedStaging = Join-Path $SrcTauri "target\beta-package-resources"
if ($StagingDir -ne $expectedStaging) { Fail "Staging path guard failed: $StagingDir" }
if (Test-Path -LiteralPath $StagingDir) { Remove-Item -LiteralPath $StagingDir -Recurse -Force }
New-Item -ItemType Directory -Path $StagingDir -Force | Out-Null
$stagedRuntime = Join-Path $StagingDir $RuntimeDirName
Copy-Item -LiteralPath $runtime.Root -Destination $stagedRuntime -Recurse -Force
$null = Test-RuntimeManifest $stagedRuntime

# Remove only stale generated runtime resources that could leak from beta.1.
$releaseRoot = Join-Path $SrcTauri "target\release"
$staleGenerated = @(
    (Join-Path $releaseRoot "moltorino"),
    (Join-Path $releaseRoot "chat-runtime"),
    (Join-Path $releaseRoot "licenses"),
    (Join-Path $releaseRoot "runtime-manifest.json"),
    (Join-Path $releaseRoot "SOURCE.txt")
)
foreach ($path in $staleGenerated) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
}
Write-Host "Staged target layout: $stagedRuntime"

Section "C. Build frontend and isolated NSIS beta"
$priorBetaVersion = $env:STREAMNOOK_BETA_VERSION
Push-Location $RepoRoot
try {
    $env:STREAMNOOK_BETA_VERSION = $BetaVersion
    & npm run build
    if ($LASTEXITCODE -ne 0) { Fail "npm run build failed (exit $LASTEXITCODE)." }
    $buildStart = Get-Date
    & npm run tauri -- build --config "src-tauri/tauri.beta.conf.json" --features beta-build
    if ($LASTEXITCODE -ne 0) { Fail "Tauri beta build failed (exit $LASTEXITCODE)." }
}
finally {
    Pop-Location
    if ($null -eq $priorBetaVersion) {
        Remove-Item Env:\STREAMNOOK_BETA_VERSION -ErrorAction SilentlyContinue
    } else {
        $env:STREAMNOOK_BETA_VERSION = $priorBetaVersion
    }
}

Section "D. Locate and validate generated outputs"
$releaseExe = Join-Path $releaseRoot "$CargoBinName.exe"
$releaseRuntime = Join-Path $releaseRoot "$RuntimeDirName\$RuntimeExeName"
if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) { Fail "Release executable missing." }
if (-not (Test-Path -LiteralPath $releaseRuntime -PathType Leaf)) { Fail "Release chat runtime missing." }
if (Test-Path -LiteralPath (Join-Path $releaseRoot "moltorino\Moltorino7.exe")) {
    Fail "Stale legacy Moltorino runtime exists in release output."
}
if ((Get-Sha256 $releaseRuntime) -ne $ExpectedExeSha) { Fail "Release Bluzyrino hash mismatch." }
$null = Test-RuntimeManifest (Join-Path $releaseRoot $RuntimeDirName)

$nsisDir = Join-Path $releaseRoot "bundle\nsis"
$nsisNew = @(Get-ChildItem -LiteralPath $nsisDir -Filter "*.exe" -File |
    Where-Object { $_.LastWriteTime -ge $buildStart })
if ($nsisNew.Count -ne 1) { Fail "Expected exactly one newly generated NSIS installer; found $($nsisNew.Count)." }
$nsisInstaller = $nsisNew[0].FullName

Section "E. Assemble clean portable package"
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$portableRoot = Join-Path $OutputRoot $PortableName
$portableZip = Join-Path $OutputRoot "$PortableName-portable.zip"
$setupOut = Join-Path $OutputRoot "$PortableName-setup.exe"
$checksums = Join-Path $OutputRoot "$PortableName-checksums.txt"
foreach ($path in @($portableRoot, $portableZip, $setupOut, $checksums)) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
}
New-Item -ItemType Directory -Path $portableRoot | Out-Null
Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $portableRoot "$CargoBinName.exe")
Copy-Item -LiteralPath (Join-Path $releaseRoot $RuntimeDirName) -Destination $portableRoot -Recurse

$portableRuntime = Join-Path $portableRoot "$RuntimeDirName\$RuntimeExeName"
if (-not (Test-Path -LiteralPath $portableRuntime -PathType Leaf)) { Fail "Portable Bluzyrino entrypoint missing." }
if (Test-Path -LiteralPath (Join-Path $portableRoot "moltorino\Moltorino7.exe")) {
    Fail "Legacy Moltorino runtime leaked into portable output."
}
$null = Test-RuntimeManifest (Join-Path $portableRoot $RuntimeDirName)
$portableRuntimeSha = Get-Sha256 $portableRuntime

$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if ($sevenZip) {
    Push-Location $portableRoot
    try {
        & 7z a -tzip -mx=9 $portableZip "*" | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail "7z failed (exit $LASTEXITCODE)." }
    } finally { Pop-Location }
} else {
    Compress-Archive -Path (Join-Path $portableRoot "*") -DestinationPath $portableZip -Force
}
if (-not (Test-Path -LiteralPath $portableZip -PathType Leaf)) { Fail "Portable ZIP missing." }

Copy-Item -LiteralPath $nsisInstaller -Destination $setupOut -Force
$portableZipSha = Get-Sha256 $portableZip
$setupSha = Get-Sha256 $setupOut
@(
    "$portableZipSha  $PortableName-portable.zip",
    "$setupSha  $PortableName-setup.exe",
    "$portableRuntimeSha  $RuntimeDirName/$RuntimeExeName"
) | Set-Content -LiteralPath $checksums -Encoding UTF8

Section "F. Final local-only report"
$runtimeFinal = Test-RuntimeManifest $runtime.Root
if ($runtimeFinal.ManifestSha -ne $runtime.ManifestSha -or $runtimeFinal.ExeSha -ne $runtime.ExeSha) {
    Fail "External staged runtime changed during packaging."
}
Write-Host "runtime version       : $($runtime.Version)"
Write-Host "runtime files         : $($runtime.FileCount)"
Write-Host "runtime bytes         : $($runtime.TotalBytes)"
Write-Host "runtime executable SHA: $portableRuntimeSha"
Write-Host "portable root         : $portableRoot"
Write-Host "portable ZIP          : $portableZip"
Write-Host "portable ZIP SHA-256  : $portableZipSha"
Write-Host "installer             : $setupOut"
Write-Host "installer SHA-256     : $setupSha"
Write-Host "checksums             : $checksums"
Write-Host "Legacy moltorino runtime absent from new portable/release output." -ForegroundColor Green
Write-Host "LOCAL-ONLY beta packaging complete. Nothing uploaded, released, tagged, or committed." -ForegroundColor Green
