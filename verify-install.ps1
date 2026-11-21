#Requires -Version 5.1
<#
.SYNOPSIS
    Verifies a desktop-buddy installation: source, build artifacts, tests, the
    Hermes MCP registration, the live API and the MCP bridge itself.

.DESCRIPTION
    Read-only. Prints one line per check, then a summary, and exits 0 only when
    every required check passed. Warnings (an app that is not running, a
    missing Hermes, a stale WebView2) do not fail the run; anything that means
    "this install cannot work" does.

    Pure ASCII on purpose: Windows PowerShell 5.1 decodes a BOM-less file as the
    ANSI code page, and one non-ASCII glyph can stop the file parsing.

.PARAMETER InstallDir
    The checkout to verify. Default C:\hermes\desktop-buddy.

.PARAMETER SkipTests
    Skip the cargo test stage (the slowest check).

.PARAMETER SkipProbe
    Skip the MCP stdio handshake (useful when the bridge binary is held open by
    a running gateway and cannot be spawned).

.EXAMPLE
    .\verify-install.ps1

.EXAMPLE
    .\verify-install.ps1 -InstallDir D:\tools\desktop-buddy -SkipTests
#>
[CmdletBinding()]
param(
    [string]$InstallDir = 'C:\hermes\desktop-buddy',
    [string]$ApiUrl = 'http://127.0.0.1:8765',
    [switch]$SkipTests,
    [switch]$SkipProbe
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:Failures = 0
$script:Warnings = 0

function Write-Pass { param([string]$Msg) Write-Host "  [ OK ] $Msg" -ForegroundColor Green }
function Write-Fail { param([string]$Msg) Write-Host "  [FAIL] $Msg" -ForegroundColor Red; $script:Failures++ }
function Write-Warn { param([string]$Msg) Write-Host "  [WARN] $Msg" -ForegroundColor Yellow; $script:Warnings++ }
function Write-Info { param([string]$Msg) Write-Host "         $Msg" -ForegroundColor DarkGray }

function Test-IsWindows {
    # $IsWindows does not exist in Windows PowerShell 5.1 - it is $null there,
    # so any "if ($IsWindows)" guard silently skips its whole section.
    if ($PSVersionTable.PSVersion.Major -lt 6) { return $true }
    return [bool]$IsWindows
}

$target = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
$srcTauri = Join-Path $target 'src-tauri'
$debugDir = Join-Path $srcTauri 'target\debug'
$appExe = Join-Path $debugDir 'desktop-buddy.exe'
$mcpExe = Join-Path $debugDir 'desktop-buddy-mcp.exe'
$watcherExe = Join-Path $debugDir 'desktop-buddy-watcher.exe'

Write-Host '=== desktop-buddy installation verification ==='
Write-Host "install dir: $target"
Write-Host ''

# ---------------------------------------------------------------- source tree
Write-Host '-- source tree'
if (Test-Path -LiteralPath (Join-Path $target '.git')) { Write-Pass 'git checkout present' }
else { Write-Fail "no .git at $target" }
foreach ($name in @('README.md', 'INSTALL.md', 'install.ps1', 'verify-install.ps1', 'scripts\build-plugin.sh', 'scripts\mcp-probe.ps1')) {
    $path = Join-Path $target $name
    if (Test-Path -LiteralPath $path) { Write-Pass "$name present" }
    else { Write-Fail "$name missing" }
}
if (Test-Path -LiteralPath (Join-Path $srcTauri 'Cargo.toml')) { Write-Pass 'src-tauri/Cargo.toml present' }
else { Write-Fail 'src-tauri/Cargo.toml missing' }

# ------------------------------------------------------------------ binaries
Write-Host ''
Write-Host '-- binaries'
foreach ($bin in @($appExe, $mcpExe, $watcherExe)) {
    if (Test-Path -LiteralPath $bin) {
        $mb = [math]::Round((Get-Item -LiteralPath $bin).Length / 1MB, 1)
        Write-Pass "$(Split-Path -Leaf $bin) ($mb MB)"
    }
    else { Write-Fail "$(Split-Path -Leaf $bin) missing (run install.ps1 or cargo build --bins)" }
}

# ------------------------------------------------------------------- plugins
Write-Host ''
Write-Host '-- Wasm plugin guests'
$pluginRoot = Join-Path $target 'plugins'
$pluginDirs = @(Get-ChildItem -LiteralPath $pluginRoot -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'Cargo.toml') })
if ($pluginDirs.Count -eq 0) { Write-Fail "no plugin crates under $pluginRoot" }
foreach ($dir in $pluginDirs) {
    $wasm = Join-Path $pluginRoot "$($dir.Name).wasm"
    if (Test-Path -LiteralPath $wasm) {
        $kb = [math]::Round((Get-Item -LiteralPath $wasm).Length / 1KB, 0)
        Write-Pass "$($dir.Name).wasm ($kb KB)"
    }
    else { Write-Fail "$($dir.Name).wasm missing (run .\install.ps1 or scripts\build-plugin.sh)" }
}

# --------------------------------------------------------------------- tests
Write-Host ''
if ($SkipTests) {
    Write-Warn 'tests skipped (-SkipTests)'
}
else {
    Write-Host '-- tests (cargo test --lib --bins --tests)'
    Push-Location $srcTauri
    try {
        & cargo test --lib --bins --tests
        $code = $LASTEXITCODE
    }
    finally { Pop-Location }
    if ($code -eq 0) { Write-Pass 'the whole suite passed (lib + both bins + tests/)' }
    else { Write-Fail "cargo test exited $code" }
}

# ------------------------------------------------------- Hermes registration
Write-Host ''
Write-Host '-- Hermes wiring'
$hermesAvailable = ($null -ne (Get-Command hermes -ErrorAction SilentlyContinue))
if (-not $hermesAvailable) {
    Write-Warn 'the "hermes" command is not on PATH; skipping the MCP registration checks'
}
else {
    $listText = (& hermes mcp list 2>&1 | Out-String)
    if ($listText -match '(?m)^\s+desktop-buddy\s{2,}') { Write-Pass 'desktop-buddy is registered ("hermes mcp list")' }
    else { Write-Fail 'desktop-buddy is not registered; run install.ps1 (without -SkipHermes)' }

    if (Test-IsWindows) {
        $configPath = Join-Path $env:LOCALAPPDATA 'hermes\config.yaml'
        if (Test-Path -LiteralPath $configPath) {
            $configText = Get-Content -LiteralPath $configPath -Raw
            if ($configText -match 'DESKTOP_BUDDY_API_URL') { Write-Pass 'DESKTOP_BUDDY_API_URL is in the registered env block' }
            else { Write-Warn 'no DESKTOP_BUDDY_API_URL in config.yaml; the bridge falls back to 127.0.0.1:8765' }
        }
        else { Write-Warn "Hermes config.yaml not found at $configPath" }

        # The gateway is the scheduled task \Hermes_Gateway. Its absence is not a
        # wiring failure (a bare CLI install has no gateway), so this warns.
        & schtasks /Query /TN '\Hermes_Gateway' 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { Write-Pass 'gateway scheduled task \Hermes_Gateway exists' }
        else { Write-Warn 'no \Hermes_Gateway scheduled task (no gateway service, or it has another name)' }
    }
}

# ---------------------------------------------------------------- live API
Write-Host ''
Write-Host '-- live API'
$appRunning = $false
try {
    $health = Invoke-RestMethod -Uri "$ApiUrl/health" -TimeoutSec 3 -ErrorAction Stop
    if ($health.ok -eq $true) {
        Write-Pass "/health answered ok: true (version $($health.version), uptime $($health.uptime_secs)s)"
        $appRunning = $true
    }
    else { Write-Fail "/health answered but ok is '$($health.ok)'" }
}
catch { Write-Warn "app not running ($ApiUrl/health did not answer) - start $appExe" }

if ($appRunning) {
    try {
        $ctx = Invoke-RestMethod -Uri "$ApiUrl/context" -TimeoutSec 3 -ErrorAction Stop
        if ($ctx.PSObject.Properties.Name -contains 'process_name') {
            Write-Pass "/context reports process_name '$($ctx.process_name)', idle_secs $($ctx.idle_secs), source '$($ctx.source)'"
        }
        else { Write-Fail '/context has no process_name field (API shape changed)' }
    }
    catch { Write-Fail "/context failed: $($_.Exception.Message)" }
}

# ------------------------------------------------------------ MCP bridge
Write-Host ''
Write-Host '-- MCP bridge (real JSON-RPC over stdio)'
$probeScript = Join-Path $target 'scripts\mcp-probe.ps1'
if ($SkipProbe) {
    Write-Warn 'bridge probe skipped (-SkipProbe)'
}
elseif (-not (Test-Path -LiteralPath $probeScript)) {
    Write-Fail "probe script missing: $probeScript"
}
else {
    . $probeScript
    $probe = Invoke-DesktopBuddyMcpProbe -Exe $mcpExe -ApiUrl $ApiUrl
    if (-not $probe.Ok) {
        Write-Fail "handshake failed: $($probe.Error)"
        if ($probe.Frames.Count -gt 0) { Write-Info "sent: $($probe.Frames -join ' || ')" }
        if ($probe.Stderr.Count -gt 0) { Write-Info ($probe.Stderr -join ' | ') }
    }
    else {
        Write-Pass "initialize negotiated $($probe.Protocol) with '$($probe.ServerName)'"
        Write-Pass "tools/list returned $($probe.ToolCount) tools"
        if (-not $probe.CallAnswered) { Write-Fail 'tools/call avatar_get_context got no answer' }
        elseif ($probe.CallIsError -and $appRunning) { Write-Fail "avatar_get_context errored while the app is up: $($probe.CallText)" }
        elseif ($probe.CallIsError) { Write-Pass 'avatar_get_context answered (app down, so it reported the avatar as unreachable)' }
        else { Write-Pass 'avatar_get_context returned live desktop context' }
    }
}

# ------------------------------------------------------------------ summary
Write-Host ''
if ($script:Failures -eq 0) {
    Write-Host "All checks passed ($script:Warnings warning(s))." -ForegroundColor Green
    if ($appRunning) {
        Write-Host 'The app is running; the gateway still has to be seen picking the server up -' -ForegroundColor Green
        Write-Host 'call mcp__desktop_buddy__avatar_get_context from the agent to prove that.' -ForegroundColor Green
    }
    exit 0
}
Write-Host "$script:Failures check(s) failed, $script:Warnings warning(s)." -ForegroundColor Red
exit 1
