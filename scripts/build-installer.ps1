# Builds the Equalizerer Windows installer (installer/output/Equalizerer-Setup-<version>.exe).
# Steps: read the version, build eq-app in release mode, make sure the VB-CABLE driver pack is
# extracted, then compile installer/equalizerer.iss with Inno Setup 6 (ISCC.exe).
# Stops at the first failing step and exits non-zero.

[CmdletBinding()]
param(
    # Fail instead of downloading the VB-CABLE driver pack when it is not extracted yet.
    [switch]$SkipDownload
)

$ErrorActionPreference = 'Stop'

$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $repoRoot

$vbCableUrl = 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip'
$vendorDir = Join-Path $repoRoot 'installer\vendor'
$zipPath = Join-Path $vendorDir 'VBCABLE_Driver_Pack45.zip'
$vbCableDir = Join-Path $vendorDir 'vbcable'
$vbCableSetup = Join-Path $vbCableDir 'VBCABLE_Setup_x64.exe'
$issPath = Join-Path $repoRoot 'installer\equalizerer.iss'
$outputDir = Join-Path $repoRoot 'installer\output'

# 1. Version from the workspace package section of the root Cargo.toml.
$cargoToml = Get-Content -LiteralPath (Join-Path $repoRoot 'Cargo.toml') -Raw
$section = [regex]::Match($cargoToml, '(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\z)')
if (-not $section.Success) {
    throw 'Cargo.toml has no [workspace.package] section.'
}
$versionMatch = [regex]::Match($section.Groups[1].Value, '(?m)^\s*version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw 'Cargo.toml [workspace.package] has no version.'
}
$version = $versionMatch.Groups[1].Value
Write-Host "==> Equalizerer version $version"

# 2. Release build of the application.
Write-Host '==> cargo build --release -p eq-app'
& cargo build --release -p eq-app
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed (exit $LASTEXITCODE). If eq-app.exe is locked, close the running Equalizerer first."
}

# 3. VB-CABLE driver pack, extracted into installer/vendor/vbcable.
if (-not (Test-Path -LiteralPath $vbCableSetup)) {
    if ($SkipDownload) {
        throw "VB-CABLE setup not found at $vbCableSetup and -SkipDownload was given. Run the script without -SkipDownload or place the extracted pack there."
    }
    New-Item -ItemType Directory -Force -Path $vendorDir | Out-Null
    Write-Host "==> Downloading VB-CABLE driver pack from $vbCableUrl"
    Invoke-WebRequest -Uri $vbCableUrl -OutFile $zipPath -UseBasicParsing
    $hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
    Write-Host "    SHA-256: $hash"
    if (Test-Path -LiteralPath $vbCableDir) {
        Remove-Item -LiteralPath $vbCableDir -Recurse -Force
    }
    Write-Host "==> Extracting to $vbCableDir"
    Expand-Archive -LiteralPath $zipPath -DestinationPath $vbCableDir -Force
    if (-not (Test-Path -LiteralPath $vbCableSetup)) {
        throw "The VB-CABLE pack does not contain VBCABLE_Setup_x64.exe at its top level."
    }
}
$signature = Get-AuthenticodeSignature -LiteralPath $vbCableSetup
Write-Host "==> VB-CABLE setup signature: $($signature.Status) ($($signature.SignerCertificate.Subject))"

# 4. Inno Setup compiler.
$isccCandidates = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
)
$iscc = $isccCandidates | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
if (-not $iscc) {
    $onPath = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($onPath) { $iscc = $onPath.Source }
}
if (-not $iscc) {
    throw 'Inno Setup 6 (ISCC.exe) was not found. Install it with: winget install JRSoftware.InnoSetup'
}
Write-Host "==> Using $iscc"

# 5. Compile the installer.
Write-Host '==> Compiling installer/equalizerer.iss'
& $iscc "/DAppVersion=$version" "/DRepoRoot=$repoRoot" $issPath
if ($LASTEXITCODE -ne 0) {
    throw "ISCC failed (exit $LASTEXITCODE)."
}

$setupPath = Join-Path $outputDir "Equalizerer-Setup-$version.exe"
if (-not (Test-Path -LiteralPath $setupPath)) {
    throw "ISCC reported success but $setupPath does not exist."
}
$sizeMb = [math]::Round((Get-Item -LiteralPath $setupPath).Length / 1MB, 2)
Write-Host "==> Built $setupPath ($sizeMb MB)"
exit 0
