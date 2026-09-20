[CmdletBinding()]
param()

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}
function Write-Ok([string]$Message) {
    Write-Host "  [ok] $Message" -ForegroundColor Green
}
function Write-Info([string]$Message) {
    Write-Host "  .. $Message" -ForegroundColor Gray
}
function Write-Fail([string]$Message) {
    Write-Host "  [fail] $Message" -ForegroundColor Red
}
$ErrorActionPreference = "Stop"
$version = $PSVersionTable.PSVersion
if ($version.Major -lt 7) {
    Write-Fail "BootstrapFilipinoBoy.ps1 needs PowerShell 7.6 or newer; found $version. Install pwsh and rerun."
    exit 1
}
$Root = Split-Path -Parent $PSScriptRoot
$env:NAW_HTTP_BIND = if ($env:NAW_HTTP_BIND) { $env:NAW_HTTP_BIND } else { "127.0.0.1" }
$env:NAW_HTTP_PORT = if ($env:NAW_HTTP_PORT) { $env:NAW_HTTP_PORT } else { "4242" }
$env:NAW_SKIN_DIR = "skins/snackers"
$env:NAW_SEED_DIR = "seeds"
$env:NAW_RUN_LIVE_TESTS = "1"
$env:NAW_TEST_REQUIRE_BRAND_ASSETS = "1"
$env:NAW_TEST_BASE_URL = "http://$($env:NAW_HTTP_BIND):$($env:NAW_HTTP_PORT)"

$started = $false
try {
    Write-Step "Booting dev stack"
    & (Join-Path $PSScriptRoot "dev-up.ps1") -WithWorker
    if ($LASTEXITCODE -ne 0) { throw "dev-up failed" }
    $started = $true
    Write-Ok "dev stack up"

    Write-Step "Checking FilianWIKI brand"
    $homePage = (& curl.exe --fail --silent --show-error --max-time 5 "$($env:NAW_TEST_BASE_URL)/home" | Out-String)
    if ($LASTEXITCODE -ne 0 -or $homePage -notmatch "FilianWIKI") { throw "FilianWIKI brand missing from /home" }
    Write-Ok "/home carries the FilianWIKI brand"
    $manifest = (& curl.exe --fail --silent --show-error --max-time 5 "$($env:NAW_TEST_BASE_URL)/site.webmanifest" | Out-String)
    if ($LASTEXITCODE -ne 0 -or $manifest -notmatch "FilianWIKI") { throw "FilianWIKI brand missing from manifest" }
    Write-Ok "manifest carries the FilianWIKI brand"

    Write-Step "Running WeCantGive500"
    Push-Location $Root
    try {
        $env:SQLX_OFFLINE = "true"
        $testProcess = Start-Process -FilePath "cargo" -ArgumentList @("test", "-p", "naw-web", "--test", "WeCantGive500", "--", "--nocapture") -WorkingDirectory $Root -PassThru -NoNewWindow
        if (-not $testProcess.WaitForExit(120000)) {
            Stop-Process -Id $testProcess.Id -Force -ErrorAction SilentlyContinue
            throw "WeCantGive500 timed out after 120 seconds"
        }
        if ($testProcess.ExitCode -ne 0) { throw "WeCantGive500 failed with exit code $($testProcess.ExitCode)" }
    } finally {
        Pop-Location
    }
    Write-Ok "WeCantGive500 passed"

    Write-Ok "BootstrapFilipinoBoy is ready: $($env:NAW_TEST_BASE_URL)"
    Write-Info "Stop application processes with: scripts/dev-down.ps1"
    exit 0
} catch {
    if ($started) {
        try {
            & (Join-Path $PSScriptRoot "dev-down.ps1")
        } catch {
            Write-Error "cleanup failed: $($_.Exception.Message)"
        }
    }
    Write-Error $_.Exception.Message
    exit 1
}
