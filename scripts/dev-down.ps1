[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$version = $PSVersionTable.PSVersion
if ($version.Major -lt 7) {
    throw "dev-down.ps1 requires PowerShell 7.6.x or newer; found $version"
}
$StateDir = Join-Path (Split-Path -Parent $PSScriptRoot) ".dev"

function Stop-ManagedProcess([string]$PidFile) {
    if (-not (Test-Path $PidFile)) { return }
    $pidValue = [int](Get-Content $PidFile -Raw)
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($process) {
        Stop-Process -Id $pidValue -Force
    }
    Remove-Item $PidFile -Force
}

Stop-ManagedProcess (Join-Path $StateDir "worker.pid")
Stop-ManagedProcess (Join-Path $StateDir "naw.pid")
Write-Output "application processes stopped; Docker containers were left running"
exit 0
