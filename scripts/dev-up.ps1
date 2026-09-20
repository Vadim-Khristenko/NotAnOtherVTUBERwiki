[CmdletBinding()]
param(
    [switch]$WithWorker
)

$ErrorActionPreference = "Stop"
$version = $PSVersionTable.PSVersion
if ($version.Major -lt 7) {
    throw "dev-up.ps1 requires PowerShell 7.6.x or newer; found $version"
}
$Root = Split-Path -Parent $PSScriptRoot
$StateDir = Join-Path $Root ".dev"
$AppPidFile = Join-Path $StateDir "naw.pid"
$WorkerPidFile = Join-Path $StateDir "worker.pid"
$AppLog = Join-Path $StateDir "naw.log"
$AppErr = Join-Path $StateDir "naw.err"
$WorkerLog = Join-Path $StateDir "worker.log"
$WorkerErr = Join-Path $StateDir "worker.err"

New-Item -ItemType Directory -Force -Path $StateDir | Out-Null
$env:DATABASE_URL = if ($env:DATABASE_URL) { $env:DATABASE_URL } else { "postgres://naw:naw@127.0.0.1:5433/naw_dev" }
$env:VALKEY_URL = if ($env:VALKEY_URL) { $env:VALKEY_URL } else { "redis://127.0.0.1:6380" }
$env:NAW_HTTP_BIND = if ($env:NAW_HTTP_BIND) { $env:NAW_HTTP_BIND } else { "127.0.0.1" }
$env:NAW_HTTP_PORT = if ($env:NAW_HTTP_PORT) { $env:NAW_HTTP_PORT } else { "4242" }
$env:NAW_SKIN_DIR = if ($env:NAW_SKIN_DIR) { $env:NAW_SKIN_DIR } else { "skins/snackers" }
$env:NAW_SEED_DIR = if ($env:NAW_SEED_DIR) { $env:NAW_SEED_DIR } else { "seeds" }

& docker compose -f (Join-Path $Root "docker-compose.yml") up -d postgres valkey
if ($LASTEXITCODE -ne 0) { throw "docker compose failed" }

function Wait-ContainerHealthy([string]$Name) {
    for ($i = 0; $i -lt 60; $i++) {
        $status = (& docker inspect -f '{{.State.Health.Status}}' $Name 2>$null).Trim()
        if ($status -eq "healthy") { return }
        Start-Sleep -Seconds 1
    }
    throw "container did not become healthy: $Name"
}

Wait-ContainerHealthy "nawwk-postgres"
Wait-ContainerHealthy "nawwk-valkey"

$Binary = Join-Path $Root "target\debug\naw.exe"
if (-not (Test-Path $Binary)) {
    Push-Location $Root
    try {
        $env:SQLX_OFFLINE = "true"
        & cargo build --bin naw
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }
}

Push-Location $Root
try {
    & $Binary migrate
    if ($LASTEXITCODE -ne 0) { throw "migration failed" }
    & $Binary seed --flavor filian --slug filian --name FilianWIKI --domain snackers.vai-rice.space --vtuber Filian --community Snackers
    if ($LASTEXITCODE -ne 0) { throw "seed failed" }
} finally {
    Pop-Location
}

function Start-ManagedProcess([string]$PidFile, [string]$OutFile, [string]$ErrFile, [string]$FilePath, [string[]]$Arguments, [hashtable]$Environment) {
    if (Test-Path $PidFile) {
        $oldPid = [int](Get-Content $PidFile -Raw)
        if (Get-Process -Id $oldPid -ErrorAction SilentlyContinue) {
            return $oldPid
        }
        Remove-Item $PidFile -Force
    }
    $previous = @{}
    foreach ($key in $Environment.Keys) {
        $previous[$key] = [Environment]::GetEnvironmentVariable($key, "Process")
        [Environment]::SetEnvironmentVariable($key, [string]$Environment[$key], "Process")
    }
    try {
        $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -WorkingDirectory $Root -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile -PassThru
    } finally {
        foreach ($key in $Environment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $previous[$key], "Process")
        }
    }
    Set-Content -Path $PidFile -Value $process.Id -NoNewline
    return $process.Id
}

$appPid = Start-ManagedProcess $AppPidFile $AppLog $AppErr $Binary @("serve") @{
    NAW_HTTP_BIND = $env:NAW_HTTP_BIND
    NAW_HTTP_PORT = $env:NAW_HTTP_PORT
    NAW_SKIN_DIR = $env:NAW_SKIN_DIR
    NAW_SEED_DIR = $env:NAW_SEED_DIR
    DATABASE_URL = $env:DATABASE_URL
    VALKEY_URL = $env:VALKEY_URL
}

if ($WithWorker) {
    if (-not (Get-Command bun -ErrorAction SilentlyContinue)) { throw "-WithWorker requires Bun >= 1.4.2" }
    $workerPid = Start-ManagedProcess $WorkerPidFile $WorkerLog $WorkerErr "bun" @("run", "worker/src/index.ts") @{ WORKER_PORT = "8081" }
    Write-Output "Worker PID: $workerPid"
}

for ($i = 0; $i -lt 30; $i++) {
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Uri "http://$($env:NAW_HTTP_BIND):$($env:NAW_HTTP_PORT)/health" -TimeoutSec 2
        if ($response.StatusCode -eq 200) {
    Write-Output "FilianWIKI dev server: http://$($env:NAW_HTTP_BIND):$($env:NAW_HTTP_PORT)"
    Write-Output "Logs: $AppLog"
    exit 0
        }
    } catch { }
    if (-not (Get-Process -Id $appPid -ErrorAction SilentlyContinue)) {
        Get-Content $AppErr, $AppLog -Tail 40 -ErrorAction SilentlyContinue
        throw "naw exited before /health became ready"
    }
    Start-Sleep -Seconds 1
}
throw "naw did not become ready on port $($env:NAW_HTTP_PORT)"
