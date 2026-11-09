# Runs the workspace checks in order: formatting, clippy (warnings are errors), tests.
# Stops at the first failing step and exits with that step's exit code.

$ErrorActionPreference = 'Stop'

$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')

Set-Location -LiteralPath (Join-Path $PSScriptRoot '..')

Write-Host '==> cargo fmt --all -- --check'
& cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) {
    Write-Host "FAILED: cargo fmt (exit $LASTEXITCODE)"
    exit $LASTEXITCODE
}

Write-Host '==> cargo clippy --workspace --all-targets -- -D warnings'
& cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) {
    Write-Host "FAILED: cargo clippy (exit $LASTEXITCODE)"
    exit $LASTEXITCODE
}

Write-Host '==> cargo test --workspace'
& cargo test --workspace
if ($LASTEXITCODE -ne 0) {
    Write-Host "FAILED: cargo test (exit $LASTEXITCODE)"
    exit $LASTEXITCODE
}

Write-Host 'All checks passed.'
exit 0
