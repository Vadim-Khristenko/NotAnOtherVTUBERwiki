[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$version = $PSVersionTable.PSVersion
if ($version.Major -lt 7) {
    throw "BootstrapFilipinoBoy.ps1 requires PowerShell 7.6.x or newer; found $version"
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
    & (Join-Path $PSScriptRoot "dev-up.ps1") -WithWorker
    if ($LASTEXITCODE -ne 0) { throw "dev-up failed" }
    $started = $true

    $homePage = (& curl.exe --fail --silent --show-error --max-time 5 "$($env:NAW_TEST_BASE_URL)/home" | Out-String)
    if ($LASTEXITCODE -ne 0 -or $homePage -notmatch "FilianWIKI") { throw "FilianWIKI brand missing from /home" }
    $manifest = (& curl.exe --fail --silent --show-error --max-time 5 "$($env:NAW_TEST_BASE_URL)/site.webmanifest" | Out-String)
    if ($LASTEXITCODE -ne 0 -or $manifest -notmatch "FilianWIKI") { throw "FilianWIKI brand missing from manifest" }

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

    Write-Output "BootstrapFilipinoBoy is ready: $($env:NAW_TEST_BASE_URL)"
    Write-Output "Stop application processes with: scripts/dev-down.ps1"
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
