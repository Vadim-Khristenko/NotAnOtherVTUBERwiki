[CmdletBinding()]
param(
    [switch]$WithWorker
)

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
    Write-Fail "dev-up.ps1 needs PowerShell 7.6 or newer; found $version. Install pwsh and rerun."
    return 1
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

Write-Step "Starting postgres + valkey"
& docker compose -f (Join-Path $Root "docker-compose.yml") up -d postgres valkey
if ($LASTEXITCODE -ne 0) { Write-Fail "docker compose failed"; return 1 }
Write-Ok "compose services requested"

function Wait-ContainerHealthy([string]$Name) {
    for ($i = 0; $i -lt 60; $i++) {
        $status = (& docker inspect -f '{{.State.Health.Status}}' $Name 2>$null).Trim()
        if ($status -eq "healthy") { return }
        Start-Sleep -Seconds 1
    }
    throw "container did not become healthy: $Name. Check docker logs for $Name."
}

Wait-ContainerHealthy "nawwk-postgres"
Write-Ok "postgres healthy"
Wait-ContainerHealthy "nawwk-valkey"
Write-Ok "valkey healthy"

Write-Step "Checking naw binary"

$Binary = Join-Path $Root "target\debug\naw.exe"
if (-not (Test-Path $Binary)) {
    Write-Step "Building naw (debug)"
    Push-Location $Root
    try {
        $env:SQLX_OFFLINE = "true"
        & cargo build --bin naw
        if ($LASTEXITCODE -ne 0) { Write-Fail "cargo build failed"; return 1 }
    } finally {
        Pop-Location
    }
    Write-Ok "naw built"
} else {
    Write-Ok "naw binary found"
}

Write-Step "Migrating + seeding FilianWIKI"
Push-Location $Root
try {
    & $Binary migrate
    if ($LASTEXITCODE -ne 0) { Write-Fail "migration failed"; return 1 }
    Write-Ok "migrations applied"
    & $Binary seed --flavor filian --slug filian --name FilianWIKI --domain snackers.vai-rice.space --vtuber Filian --community Snackers
    if ($LASTEXITCODE -ne 0) { Write-Fail "seed failed"; return 1 }
    Write-Ok "FilianWIKI seed applied"
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
    if (-not (Get-Command bun -ErrorAction SilentlyContinue)) { Write-Fail "-WithWorker needs Bun >= 1.4.2"; return 1 }
    Write-Step "Starting worker"
    $workerPid = Start-ManagedProcess $WorkerPidFile $WorkerLog $WorkerErr "bun" @("run", "worker/src/index.ts") @{ WORKER_PORT = "8081" }
    Write-Ok "worker PID: $workerPid"
}

Write-Step "Waiting for /health"

for ($i = 0; $i -lt 30; $i++) {
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Uri "http://$($env:NAW_HTTP_BIND):$($env:NAW_HTTP_PORT)/health" -TimeoutSec 2
        if ($response.StatusCode -eq 200) {
    Write-Ok "FilianWIKI dev server: http://$($env:NAW_HTTP_BIND):$($env:NAW_HTTP_PORT)"
    Write-Info "Logs: $AppLog"
    Write-Info "Stop with: scripts/dev-down.ps1"
    return 0
        }
    } catch { }
    if (-not (Get-Process -Id $appPid -ErrorAction SilentlyContinue)) {
        Get-Content $AppErr, $AppLog -Tail 40 -ErrorAction SilentlyContinue
        Write-Fail "naw exited before /health became ready"
        return 1
    }
    Start-Sleep -Seconds 1
}
Write-Fail "naw did not become ready on port $($env:NAW_HTTP_PORT)"
return 1
