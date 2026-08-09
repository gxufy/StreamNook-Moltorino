<#
.SYNOPSIS
  Local-only beta packaging prototype for StreamNook + bundled Moltorino runtime.

.DESCRIPTION
  Verifies and stages a pinned Moltorino runtime ZIP, builds an NSIS beta
  installer via the beta Tauri config, assembles a portable ZIP, and generates
  checksums. LOCAL TESTING ONLY -- the runtime ZIP still carries
  NOTICE-INCOMPLETE.txt and is not approved for public distribution.

  Nothing here uploads, publishes, tags, or commits.

.NOTES
  Written for Windows PowerShell 5.1 compatibility.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $RuntimeZip,

    [Parameter(Mandatory = $true)]
    [string] $RuntimeSha256,

    [string] $BetaVersion = "8.4.0-beta.1",

    [string] $OutputRoot = "C:\Dev\StreamNook-Beta-Staging\app-output"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# --- constants ---------------------------------------------------------------
$RepoRoot     = Split-Path -Parent $PSScriptRoot          # scripts\ -> repo root
$SrcTauri     = Join-Path $RepoRoot "src-tauri"
$StagingDir   = Join-Path $SrcTauri "target\beta-package-resources"
$BetaConfig   = Join-Path $SrcTauri "tauri.beta.conf.json"
$CargoBinName = "StreamNook"   # from [[bin]] name in Cargo.toml (NOT productName)
$ExpectedExeSha = "4fa0ce90391bc4d4c9008af79c673b97559954d5b2941efe3cb5a5377c8dc2d1"
$PortableName = "StreamNook-Moltorino-$BetaVersion-windows-x64"

function Fail([string] $msg) {
    Write-Host "FATAL: $msg" -ForegroundColor Red
    exit 1
}

function Section([string] $t) {
    Write-Host ""
    Write-Host "=== $t ===" -ForegroundColor Cyan
}

function Get-Sha256([string] $path) {
    return (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLower()
}

# =============================================================================
# A. Validate inputs
# =============================================================================
Section "A. Validate runtime ZIP"

$zip = Resolve-Path -Path $RuntimeZip -ErrorAction SilentlyContinue
if (-not $zip) { Fail "RuntimeZip not found: $RuntimeZip" }
$zip = $zip.Path
Write-Host "Runtime ZIP : $zip"

$actualZipSha = Get-Sha256 $zip
$expectedZipSha = $RuntimeSha256.ToLower()
Write-Host "expected SHA: $expectedZipSha"
Write-Host "actual   SHA: $actualZipSha"
if ($actualZipSha -ne $expectedZipSha) {
    Fail "Runtime ZIP SHA-256 mismatch. Refusing to extract."
}
Write-Host "ZIP hash verified." -ForegroundColor Green

if (-not (Test-Path $BetaConfig)) { Fail "Beta config missing: $BetaConfig" }

# Validate beta config CONTENT before building. Guards against filename/installer
# drift: filenames derive from $BetaVersion, so the config version must match, and
# the isolation-critical fields (identifier/productName/target/mode/resources)
# must be exactly the beta values or the build could collide with production.
$betaCfg = Get-Content -Raw -Path $BetaConfig | ConvertFrom-Json
if ($betaCfg.version -ne $BetaVersion) {
    Fail "Beta config version '$($betaCfg.version)' != BetaVersion parameter '$BetaVersion'."
}
if ($betaCfg.productName -ne "StreamNook Moltorino Beta") {
    Fail "Beta config productName unexpected: '$($betaCfg.productName)'."
}
if ($betaCfg.identifier -ne "com.gxufy.streamnook-moltorino.beta") {
    Fail "Beta config identifier unexpected: '$($betaCfg.identifier)'."
}
if ($betaCfg.bundle.active -ne $true) {
    Fail "Beta config bundle.active must be true."
}
if (($betaCfg.bundle.targets -join ",") -ne "nsis") {
    Fail "Beta config bundle.targets must be exactly [nsis]. Got: $($betaCfg.bundle.targets -join ',')"
}
if ($betaCfg.bundle.windows.nsis.installMode -ne "currentUser") {
    Fail "Beta config NSIS installMode must be 'currentUser'. Got: '$($betaCfg.bundle.windows.nsis.installMode)'."
}
$resProp = $betaCfg.bundle.resources.PSObject.Properties['target/beta-package-resources/']
if ($null -eq $resProp -or $resProp.Value -ne "") {
    Fail "Beta config resource 'target/beta-package-resources/' must map to an empty destination."
}
Write-Host "Beta config validated: version=$($betaCfg.version) id=$($betaCfg.identifier) target=nsis mode=currentUser." -ForegroundColor Green

# =============================================================================
# B. Ignored staging only
# =============================================================================
Section "B. Reset staging directory (ignored)"

# Guard: only ever touch the exact staging dir under src-tauri\target.
$expectedStagingTail = "src-tauri\target\beta-package-resources"
if ($StagingDir -notlike "*$expectedStagingTail") {
    Fail "Staging path guard failed: $StagingDir"
}
if (Test-Path $StagingDir) {
    Remove-Item -Path $StagingDir -Recurse -Force
    Write-Host "Removed previous staging dir."
}
New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
Write-Host "Staging dir : $StagingDir"

# =============================================================================
# C. Extract + structural verification
# =============================================================================
Section "C. Extract and verify structure"

Expand-Archive -Path $zip -DestinationPath $StagingDir -Force

$rootEntries = Get-ChildItem -Path $StagingDir | Select-Object -ExpandProperty Name | Sort-Object
$expectedRoot = @("SOURCE.txt", "licenses", "moltorino", "runtime-manifest.json") | Sort-Object
$rootJoined = ($rootEntries -join ", ")
Write-Host "root entries: $rootJoined"
if (($rootEntries -join "|") -ne ($expectedRoot -join "|")) {
    Fail "Unexpected ZIP root entries. Got: $rootJoined"
}

$mustExist = @(
    "moltorino\Moltorino7.exe",
    "moltorino\platforms\qwindows.dll",
    "moltorino\imageformats\qwebp.dll"
)
foreach ($rel in $mustExist) {
    $p = Join-Path $StagingDir $rel
    if (-not (Test-Path $p)) { Fail "Required file missing after extract: $rel" }
    Write-Host "  [OK] $rel"
}

# Verify extracted runtime against runtime-manifest.json
$manifestPath = Join-Path $StagingDir "runtime-manifest.json"
$manifest = Get-Content -Raw -Path $manifestPath | ConvertFrom-Json
$moltRoot = Join-Path $StagingDir "moltorino"

$mfCount = @($manifest.files).Count
$diskFiles = Get-ChildItem -Path $moltRoot -Recurse -File
$diskCount = @($diskFiles).Count
$mismatch = 0
$byteTotal = 0
foreach ($f in $manifest.files) {
    $fp = Join-Path $moltRoot $f.path
    if (-not (Test-Path $fp)) { Fail "Manifest file missing on disk: $($f.path)" }
    $item = Get-Item $fp
    $byteTotal += $item.Length
    if ($item.Length -ne $f.size) {
        Write-Host "  size mismatch: $($f.path) disk=$($item.Length) manifest=$($f.size)" -ForegroundColor Red
        $mismatch++
        continue
    }
    $h = Get-Sha256 $fp
    if ($h -ne $f.sha256.ToLower()) {
        Write-Host "  hash mismatch: $($f.path)" -ForegroundColor Red
        $mismatch++
    }
}
Write-Host "manifest files : $mfCount"
Write-Host "disk files     : $diskCount"
Write-Host "byte total     : $byteTotal"
Write-Host "mismatches     : $mismatch"
if ($mismatch -ne 0) { Fail "Manifest verification failed ($mismatch mismatches)." }
if ($diskCount -ne $mfCount) { Fail "Disk file count ($diskCount) != manifest count ($mfCount)." }

$exeSha = Get-Sha256 (Join-Path $moltRoot "Moltorino7.exe")
Write-Host "Moltorino7.exe : $exeSha"
if ($exeSha -ne $ExpectedExeSha) { Fail "Moltorino7.exe SHA-256 mismatch." }
Write-Host "Runtime verified against manifest." -ForegroundColor Green

# =============================================================================
# D. Sanitize only the staged manifest copy
# =============================================================================
Section "D. Sanitize staged manifest"

# Rebuild the object so we drop staged_root and add archive_root without
# touching the files array (paths/sizes/hashes preserved verbatim).
$staged = [ordered]@{}
foreach ($prop in $manifest.PSObject.Properties) {
    if ($prop.Name -eq "staged_root") { continue }
    $staged[$prop.Name] = $prop.Value
}
$staged["archive_root"] = "moltorino"
($staged | ConvertTo-Json -Depth 20) | Set-Content -Path $manifestPath -Encoding UTF8
Write-Host "Removed staged_root; added archive_root=moltorino."

# Confirm no absolute dev path remains in any staged text file.
$textExt = @(".json", ".txt", ".toml", ".md")
$hits = 0
Get-ChildItem -Path $StagingDir -Recurse -File | Where-Object { $textExt -contains $_.Extension.ToLower() } | ForEach-Object {
    $c = Get-Content -Raw -Path $_.FullName
    if ($c -match "C:\\Dev" -or $c -match "C:\\Users") {
        Write-Host "  dev-path in: $($_.FullName)" -ForegroundColor Red
        $hits++
    }
}
Write-Host "absolute dev-path hits in staged text files: $hits"
if ($hits -ne 0) { Fail "Absolute dev path found in staged text file(s)." }

# =============================================================================
# E. Build frontend + NSIS
# =============================================================================
Section "E. Build frontend + NSIS installer"

# The beta binary sources its version from the compile-time STREAMNOOK_BETA_VERSION
# env var (see src-tauri/src/build_identity.rs); without it, a --features beta-build
# compile fails by design. Capture whatever value (if any) was already in the
# environment so we can restore it verbatim in the finally block -- this script
# must not leak a beta version into the caller's shell. Under Set-StrictMode the
# $env: provider returns $null for an unset variable rather than throwing.
$priorBetaVersion = $env:STREAMNOOK_BETA_VERSION

Push-Location $RepoRoot
try {
    $env:STREAMNOOK_BETA_VERSION = $BetaVersion
    Write-Host "STREAMNOOK_BETA_VERSION set to: $env:STREAMNOOK_BETA_VERSION"

    Write-Host "Running: npm run build"
    & npm run build
    if ($LASTEXITCODE -ne 0) { Fail "npm run build failed (exit $LASTEXITCODE)." }

    # Both flags are mandatory: --features beta-build flips every build-identity
    # helper, and --config layers the beta identifier/scheme/bundle over production.
    # Neither alone yields an isolated beta.
    Write-Host "Running: npm run tauri -- build --config src-tauri/tauri.beta.conf.json --features beta-build"
    $buildStart = Get-Date
    & npm run tauri -- build --config "src-tauri/tauri.beta.conf.json" --features beta-build
    if ($LASTEXITCODE -ne 0) { Fail "Tauri beta build failed (exit $LASTEXITCODE)." }
}
finally {
    Pop-Location
    # Restore the caller's environment exactly: re-set a prior value, or remove the
    # variable entirely if it wasn't present before this script ran.
    if ($null -eq $priorBetaVersion) {
        Remove-Item Env:\STREAMNOOK_BETA_VERSION -ErrorAction SilentlyContinue
    } else {
        $env:STREAMNOOK_BETA_VERSION = $priorBetaVersion
    }
}

# =============================================================================
# F. Locate outputs unambiguously
# =============================================================================
Section "F. Locate build outputs"

$releaseExe = Join-Path $SrcTauri "target\release\$CargoBinName.exe"
if (-not (Test-Path $releaseExe)) { Fail "Release executable not found: $releaseExe" }
$releaseExe = (Resolve-Path $releaseExe).Path
Write-Host "release exe : $releaseExe"

$nsisDir = Join-Path $SrcTauri "target\release\bundle\nsis"
if (-not (Test-Path $nsisDir)) { Fail "NSIS bundle dir not found: $nsisDir" }

# Locate the newly generated installer: *.exe in the nsis dir modified at/after
# the build start. Fail on zero or multiple ambiguous matches.
$nsisAll = Get-ChildItem -Path $nsisDir -Filter "*.exe" -File
$nsisNew = @($nsisAll | Where-Object { $_.LastWriteTime -ge $buildStart })
if ($nsisNew.Count -eq 0) {
    Fail "No newly generated NSIS installer found in $nsisDir (built after $buildStart)."
}
if ($nsisNew.Count -gt 1) {
    $names = ($nsisNew | Select-Object -ExpandProperty Name) -join ", "
    Fail "Ambiguous NSIS installers ($($nsisNew.Count)): $names"
}
$nsisInstaller = $nsisNew[0].FullName
Write-Host "NSIS setup  : $nsisInstaller"

# =============================================================================
# G. Assemble portable package (outside Git)
# =============================================================================
Section "G. Assemble portable package"

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$portableRoot = Join-Path $OutputRoot $PortableName
if (Test-Path $portableRoot) { Remove-Item -Path $portableRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $portableRoot | Out-Null

Copy-Item -Path $releaseExe -Destination (Join-Path $portableRoot "$CargoBinName.exe")

# Copy staged package resources (moltorino\, licenses\, manifest, SOURCE.txt)
Copy-Item -Path (Join-Path $StagingDir "moltorino") -Destination $portableRoot -Recurse
Copy-Item -Path (Join-Path $StagingDir "licenses") -Destination $portableRoot -Recurse
Copy-Item -Path (Join-Path $StagingDir "runtime-manifest.json") -Destination $portableRoot
Copy-Item -Path (Join-Path $StagingDir "SOURCE.txt") -Destination $portableRoot

$portableExe = Join-Path $portableRoot "moltorino\Moltorino7.exe"
if (-not (Test-Path $portableExe)) { Fail "Portable layout missing moltorino\Moltorino7.exe" }
Write-Host "portable root: $portableRoot"
Write-Host "  [OK] moltorino\Moltorino7.exe present"

# ZIP with StreamNook.exe directly at root (no wrapper dir): archive the
# CONTENTS of the portable root.
$portableZip = Join-Path $OutputRoot "$PortableName-portable.zip"
if (Test-Path $portableZip) { Remove-Item -Path $portableZip -Force }

$sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue)
if ($sevenZip) {
    Push-Location $portableRoot
    try {
        & 7z a -tzip -mx=9 $portableZip "*" | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail "7z portable zip failed (exit $LASTEXITCODE)." }
    } finally { Pop-Location }
} else {
    # Fallback: Compress-Archive of the contents (\* avoids a wrapper dir).
    Compress-Archive -Path (Join-Path $portableRoot "*") -DestinationPath $portableZip -Force
}
if (-not (Test-Path $portableZip)) { Fail "Portable ZIP was not created." }

$portableZipSize = (Get-Item $portableZip).Length
$portableZipSha = Get-Sha256 $portableZip
Write-Host "portable zip : $portableZip"
Write-Host "  size       : $portableZipSize"
Write-Host "  sha256     : $portableZipSha"

# =============================================================================
# H. Copy + rename installer
# =============================================================================
Section "H. Copy + rename installer"

$setupOut = Join-Path $OutputRoot "$PortableName-setup.exe"
Copy-Item -Path $nsisInstaller -Destination $setupOut -Force
$setupSize = (Get-Item $setupOut).Length
$setupSha = Get-Sha256 $setupOut
Write-Host "setup exe   : $setupOut"
Write-Host "  size      : $setupSize"
Write-Host "  sha256    : $setupSha"

# =============================================================================
# I. Checksums
# =============================================================================
Section "I. Checksums"

$checksums = Join-Path $OutputRoot "$PortableName-checksums.txt"
$lines = @(
    "$portableZipSha  $PortableName-portable.zip",
    "$setupSha  $PortableName-setup.exe"
)
$lines | Set-Content -Path $checksums -Encoding UTF8
Write-Host "checksums   : $checksums"
$lines | ForEach-Object { Write-Host "  $_" }

# =============================================================================
# J. Final report
# =============================================================================
Section "J. FINAL REPORT"

# E. Re-verify the source runtime ZIP is byte-identical (unchanged since step A).
# Presence of outputs is not proof the source was left untouched.
$finalZipSha = Get-Sha256 $zip
if ($finalZipSha -ne $expectedZipSha) {
    Fail "Source runtime ZIP changed during packaging! start=$expectedZipSha end=$finalZipSha"
}
Write-Host "source ZIP re-verified    : $finalZipSha (byte-identical)" -ForegroundColor Green
Write-Host "runtime ZIP hash verified : $actualZipSha"
Write-Host "staged runtime files      : $diskCount"
Write-Host "staged runtime bytes      : $byteTotal"
Write-Host "release executable        : $releaseExe"
Write-Host "original NSIS installer   : $nsisInstaller"
Write-Host "portable ZIP              : $portableZip"
Write-Host "  bytes                   : $portableZipSize"
Write-Host "  sha256                  : $portableZipSha"
Write-Host "renamed setup exe         : $setupOut"
Write-Host "  bytes                   : $setupSize"
Write-Host "  sha256                  : $setupSha"
Write-Host "checksums file            : $checksums"
Write-Host ""
Write-Host "LOCAL-ONLY prototype complete. Nothing uploaded, released, tagged, or committed." -ForegroundColor Green
