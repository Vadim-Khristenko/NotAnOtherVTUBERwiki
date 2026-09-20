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
    Write-Fail "dev-down.ps1 needs PowerShell 7.6 or newer; found $version. Install pwsh and rerun."
    return 1
}
$StateDir = Join-Path (Split-Path -Parent $PSScriptRoot) ".dev"

function Stop-ManagedProcess([string]$PidFile, [string]$Label) {
    if (-not (Test-Path $PidFile)) { Write-Info "$Label already stopped"; return }
    $raw = (Get-Content $PidFile -Raw).Trim()
    $pidValue = 0
    if (-not [int]::TryParse($raw, [ref]$pidValue)) {
        Write-Info "$Label pid file unreadable, removing it"
        Remove-Item $PidFile -Force
        return
    }
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($process) {
        Write-Info "stopping $Label (PID $pidValue)"
        Stop-Process -Id $pidValue -Force
    } else {
        Write-Info "$Label PID $pidValue already gone"
    }
    Remove-Item $PidFile -Force
    Write-Ok "$Label stopped"
}

Write-Step "Stopping app processes"
Stop-ManagedProcess (Join-Path $StateDir "worker.pid") "worker"
Stop-ManagedProcess (Join-Path $StateDir "naw.pid") "naw"
Write-Ok "application processes stopped; Docker containers were left running"
