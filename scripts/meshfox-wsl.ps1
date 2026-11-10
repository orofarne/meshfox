<#
.SYNOPSIS
  Runs `meshfox serve` inside WSL2: installs/updates the binary (and the
  DuckDB CLI that table previews need) and keeps the canvases in a Windows
  directory (reached as /mnt/c/... from WSL).

.EXAMPLE
  .\meshfox-wsl.ps1                       # canvases in %USERPROFILE%\meshfox, port 4590
  .\meshfox-wsl.ps1 -CanvasDir D:\notes -Port 4600
  .\meshfox-wsl.ps1 -SkipUpdate
#>
[CmdletBinding()]
param(
    [string]$CanvasDir = (Join-Path $env:USERPROFILE 'meshfox'),
    [int]   $Port      = 4590,
    [string]$Distro    = 'Ubuntu-24.04',
    [string]$DuckDbVersion = '',    # e.g. 2.0.0; empty = latest stable
    [switch]$SkipUpdate,
    [switch]$NoBrowser
)
$ErrorActionPreference = 'Stop'

function Invoke-Wsl([string]$Command) {
    # The command must not contain double quotes, or Windows PowerShell 5.1
    # mangles the argument.
    & wsl.exe -d $Distro --exec bash -lc $Command
    return $LASTEXITCODE
}

# --- 1. WSL and the distribution -------------------------------------------
if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
    throw 'wsl.exe not found. Enable WSL first: wsl --install (PowerShell as administrator).'
}
# `wsl -l -q` prints UTF-16, hence stripping the NUL characters.
$installed = (& wsl.exe -l -q) -replace "`0", '' | ForEach-Object { $_.Trim() } | Where-Object { $_ }
if ($installed -notcontains $Distro) {
    Write-Host "Distribution $Distro is not installed, installing..."
    & wsl.exe --install -d $Distro
    Write-Host 'Finish the first-run setup (username/password) in the window that opens, reboot if asked, then run this script again.'
    return
}

# --- 2. Canvas directory and its path inside WSL ---------------------------
New-Item -ItemType Directory -Force -Path $CanvasDir | Out-Null
$CanvasDir = (Resolve-Path $CanvasDir).Path
$wslRoot = (& wsl.exe -d $Distro --exec wslpath -a $CanvasDir).Trim()
if (-not $wslRoot) { throw "Could not translate $CanvasDir to a WSL path." }
if ($wslRoot -notlike '/mnt/*') {
    Write-Warning "$CanvasDir is not on a Windows drive (WSL path: $wslRoot)."
}
$rootQ = "'" + ($wslRoot -replace "'", "'\''") + "'"

# --- 3. Install or update meshfox ------------------------------------------
$bin = '$HOME/.local/bin/meshfox'
$null = Invoke-Wsl "test -x $bin"
$hasBin = ($LASTEXITCODE -eq 0)

if (-not $hasBin) {
    Write-Host 'meshfox not found, installing...'
    $rc = Invoke-Wsl 'command -v curl >/dev/null || { echo curl is not installed: sudo apt install -y curl; exit 1; }; curl -fsSL https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/install.sh | sh -s -- -y --install-dir $HOME/.local/bin'
    if ($rc -ne 0) { throw "Installation failed (exit code $rc)." }
} elseif (-not $SkipUpdate) {
    Write-Host 'Checking for updates...'
    $rc = Invoke-Wsl "$bin check-updates -y"
    if ($rc -ne 0) { Write-Warning "check-updates exited with code $rc, continuing with the current version." }
}

$rc = Invoke-Wsl "$bin --version"
if ($rc -ne 0) {
    throw 'meshfox does not start. A common cause is glibc older than 2.39 (use Ubuntu 24.04+ or Debian 13).'
}

# --- 3b. DuckDB CLI (needed for display="table") ---------------------------
# meshfox never installs it itself. The official installer puts it in
# ~/.duckdb/cli/latest/duckdb, which is one of the places meshfox looks.
# The installer has no install-dir option: it always installs to
# ~/.duckdb/cli/<version>/ and points ~/.duckdb/cli/latest at the newest one
# (only when that is the latest stable release). Re-running it is the update
# path: it downloads nothing if that version is already installed. The binary
# is handed to meshfox explicitly via MESHFOX_DUCKDB below.
if ($DuckDbVersion) {
    $duckVer  = $DuckDbVersion.TrimStart('v')
    $duckEnv  = "DUCKDB_VERSION=$duckVer "
    $duckPath = "`$HOME/.duckdb/cli/$duckVer/duckdb"
} else {
    $duckEnv  = ''
    $duckPath = '$HOME/.duckdb/cli/latest/duckdb'
}
$null = Invoke-Wsl "test -x $duckPath"
$hasDuck = ($LASTEXITCODE -eq 0)

if (-not $hasDuck -or -not $SkipUpdate) {
    Write-Host ($(if ($hasDuck) { 'Checking the DuckDB CLI version...' } else { 'DuckDB CLI not found, installing...' }))
    $rc = Invoke-Wsl "for t in curl zcat tar; do command -v `$t >/dev/null || { echo `$t is missing: sudo apt install -y curl gzip tar; exit 1; }; done; ${duckEnv}curl -fsSL https://install.duckdb.org | ${duckEnv}sh"
    if ($rc -ne 0) {
        Write-Warning "DuckDB installation failed (exit code $rc); display=table nodes will not work until it is installed."
    }
}
$null = Invoke-Wsl "test -x $duckPath"
if ($LASTEXITCODE -ne 0) {
    Write-Warning "DuckDB CLI not found at $duckPath; meshfox will fall back to its default lookup."
    $duckPath = $null
}

# --- 4. Access token (kept between runs) -----------------------------------
$stateDir  = Join-Path $env:LOCALAPPDATA 'meshfox'
$tokenFile = Join-Path $stateDir 'token'
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
if (-not (Test-Path $tokenFile)) {
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    ($bytes | ForEach-Object { $_.ToString('x2') }) -join '' | Set-Content -NoNewline -Encoding ascii $tokenFile
}
$token = (Get-Content -Raw $tokenFile).Trim()

# --- 5. Start the coordinator ----------------------------------------------
# The socket must live on the Linux filesystem: unix sockets and chmod 0600
# are unreliable on /mnt/c.
$socket = '$HOME/.meshfox/serve.sock'
Write-Host ''
Write-Host "Canvases: $CanvasDir"
Write-Host "URL:      http://localhost:$Port/"
Write-Host "Token:    $token   (file: $tokenFile)"
Write-Host 'Stop with Ctrl+C. While this window is open WSL keeps the distribution running.'
Write-Host ''

if (-not $NoBrowser) {
    Start-Job -ScriptBlock { param($p) Start-Sleep 2; Start-Process "http://localhost:$p/" } -ArgumentList $Port | Out-Null
}

$duckVar = if ($duckPath) { "MESHFOX_DUCKDB=$duckPath " } else { '' }
$serve = "mkdir -p `$HOME/.meshfox && MESHFOX_TOKEN=$token ${duckVar}exec $bin serve --socket $socket --http 127.0.0.1:$Port --root $rootQ"
Invoke-Wsl $serve | Out-Null
