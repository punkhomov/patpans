$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo test --features testing --test windows_hook_e2e -- --nocapture --test-threads=1
if ($LASTEXITCODE -ne 0) {
    Write-Host "error: windows e2e failed" -ForegroundColor Red
    exit $LASTEXITCODE
}
Write-Host "e2e: OK" -ForegroundColor Green
