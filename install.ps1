#Requires -Version 5.1
<#
.SYNOPSIS
    Builds desktop-buddy from source and wires it into Hermes as an MCP server.

.DESCRIPTION
    Written to be run unattended by an AI agent (or a human) on a Windows 11
    machine: install the prerequisites, clone, build the Wasm plugin guests and
    the three Rust binaries, run the test suite, register the MCP bridge with
    Hermes, restart the Hermes gateway, launch the app and prove the bridge
    answers by speaking MCP to it.

    Every claim printed here is backed by a command whose exit code was
    checked, and the final gate is a real JSON-RPC call to
    desktop-buddy-mcp.exe rather than "hermes mcp test" (which spawns its own
    copy and can be green while the gateway holds a dead child).

    This script is deliberately pure ASCII: Windows PowerShell 5.1 decodes a
    BOM-less file as the ANSI code page, so a single non-ASCII glyph (a check
    mark, a box-drawing rule) becomes a smart quote and the file stops parsing.
    Keep it ASCII if you edit it.

.PARAMETER InstallDir
    Where to clone. Default C:\hermes\desktop-buddy.

.PARAMETER RepoUrl
    Clone URL. Override it to install a fork, or to test against a local clone.

.PARAMETER Branch
    Branch to check out. Default main.

.PARAMETER SkipPrereqs
    Assume Rust (with the wasm32 target), Git and WebView2 are already present.

.PARAMETER SkipHermes
    Build only - do not touch the Hermes config or gateway.

.PARAMETER SkipTests
    Skip the cargo test stage (not recommended; it is the build-integrity gate).

.PARAMETER NoLaunch
    Do not start the app at the end (the MCP gate then reports the app as down).

.PARAMETER Force
    Re-clone over an existing clean checkout. Refuses to delete a directory that
    has uncommitted changes or is not this repository.

.EXAMPLE
    .\install.ps1

.EXAMPLE
    .\install.ps1 -InstallDir D:\tools\desktop-buddy -SkipHermes

.EXAMPLE
    .\install.ps1 -RepoUrl C:/src/desktop-buddy -SkipPrereqs -SkipTests
#>
[CmdletBinding()]
param(
    [string]$InstallDir = 'C:\hermes\desktop-buddy',
    [string]$RepoUrl = 'https://github.com/dunkzinski/desktop-buddy',
    [string]$Branch = 'main',
    [switch]$SkipPrereqs,
    [switch]$SkipHermes,
    [switch]$SkipTests,
    [switch]$NoLaunch,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$ApiUrl = 'http://127.0.0.1:8765'

# ---------------------------------------------------------------------------
# output helpers
# ---------------------------------------------------------------------------

function Write-Section { param([string]$Msg) Write-Host ''; Write-Host "=== $Msg ===" -ForegroundColor Cyan }
function Write-Step { param([string]$Msg) Write-Host "  > $Msg" -ForegroundColor Green }
function Write-Note { param([string]$Msg) Write-Host "    $Msg" -ForegroundColor DarkGray }
function Write-Warn { param([string]$Msg) Write-Host "  ! $Msg" -ForegroundColor Yellow }
function Stop-Install { param([string]$Msg) throw $Msg }

$script:Warnings = 0

function Write-WarnOnce {
    param([string]$Msg)
    $script:Warnings++
    Write-Warn $Msg
}

# Native commands do not throw on a non-zero exit even with
# $ErrorActionPreference = 'Stop', so every one of them goes through here.
function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$Exe,
        [string[]]$ArgList = @(),
        [Parameter(Mandatory = $true)][string]$What,
        [switch]$Quiet,
        [string]$Stdin
    )
    if ($Quiet) {
        $out = & $Exe @ArgList 2>&1
        $code = $LASTEXITCODE
        if ($code -ne 0) {
            $joined = ($out | Out-String).Trim()
            Stop-Install "$What failed (exit $code): $Exe $($ArgList -join ' ')`n$joined"
        }
        return ($out | Out-String).Trim()
    }
    # Some Hermes subcommands prompt even when they look scriptable:
    # `hermes mcp add` ends with "Enable all N tools? [Y/n/select]" and treats
    # end-of-input as "Cancelled", exiting 0 with nothing written. Feeding the
    # answer in is the only way to drive it unattended.
    if ($PSBoundParameters.ContainsKey('Stdin')) {
        $Stdin | & $Exe @ArgList
    }
    else {
        & $Exe @ArgList
    }
    if ($LASTEXITCODE -ne 0) {
        Stop-Install "$What failed (exit $LASTEXITCODE): $Exe $($ArgList -join ' ')"
    }
}

function Test-CommandAvailable {
    param([string]$Name)
    return ($null -ne (Get-Command $Name -ErrorAction SilentlyContinue))
}

function Update-PathFromRegistry {
    $machine = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    $user = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $env:PATH = $machine + ';' + $user
}

function Test-AvatarApi {
    param([int]$TimeoutSec = 2)
    try {
        $health = Invoke-RestMethod -Uri "$ApiUrl/health" -TimeoutSec $TimeoutSec -ErrorAction Stop
        return ($health.ok -eq $true)
    }
    catch { return $false }
}

function Stop-RunningAvatar {
    Write-Step 'Shutting down the running app so the build can replace the binaries'
    try { Invoke-RestMethod -Method Post -Uri "$ApiUrl/shutdown" -TimeoutSec 5 -ErrorAction Stop | Out-Null }
    catch { Write-WarnOnce "the shutdown endpoint did not answer: $($_.Exception.Message)" }
    for ($i = 0; $i -lt 20; $i++) {
        if (-not (Test-AvatarApi)) { Write-Note 'app stopped'; return }
        Start-Sleep -Milliseconds 500
    }
    Write-WarnOnce 'the app still answers on the API port; the build may fail with "Access is denied"'
}

# A running .exe cannot be overwritten but it can be renamed, and that is how a
# rebuild gets past the Hermes gateway holding desktop-buddy-mcp.exe open.
function Clear-LockedBinary {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $stalePath = "$Path.stale"
    try {
        # [IO.File]::Move only has the two-argument overload on .NET Framework
        # (the overwrite overload is .NET Core), so clear the old .stale first.
        if (Test-Path -LiteralPath $stalePath) {
            Remove-Item -LiteralPath $stalePath -Force -ErrorAction SilentlyContinue
        }
        [IO.File]::Move($Path, $stalePath)
        Write-Note "renamed the locked $(Split-Path -Leaf $Path) aside so it can be rebuilt"
    }
    catch {
        Write-WarnOnce "could not rename the locked $(Split-Path -Leaf $Path): $($_.Exception.Message)"
    }
}

# ---------------------------------------------------------------------------
# 0. target directory
# ---------------------------------------------------------------------------

Write-Section 'desktop-buddy installer'

$target = [IO.Path]::GetFullPath($InstallDir).TrimEnd('\')
Write-Step "Target: $target"

$reuseCheckout = $false
if (Test-Path -LiteralPath $target) {
    $isRepo = Test-Path -LiteralPath (Join-Path $target '.git')
    if (-not $isRepo) {
        $entries = @(Get-ChildItem -LiteralPath $target -Force -ErrorAction SilentlyContinue)
        if ($entries.Count -gt 0) {
            Stop-Install "$target exists and is not a git checkout. Refusing to touch it: move it aside or pass -InstallDir."
        }
        Write-Note 'directory exists but is empty; installing into it'
    }
    else {
        Push-Location $target
        try {
            $dirty = (& git status --porcelain 2>$null | Out-String).Trim()
            $origin = (& git remote get-url origin 2>$null | Out-String).Trim()
        }
        finally { Pop-Location }
        if ($dirty -ne '') {
            Stop-Install "$target has uncommitted changes. Refusing to delete or reset it: commit, stash or move it aside, then re-run."
        }
        if ($origin -ne $RepoUrl -and $origin -ne ($RepoUrl + '.git')) {
            Stop-Install "$target is a checkout of '$origin', not '$RepoUrl'. Refusing to touch it."
        }
        if ($Force) {
            Write-Step 'Existing clean checkout found; -Force given, re-cloning it'
            Remove-Item -LiteralPath $target -Recurse -Force
        }
        else {
            Write-Step 'Existing clean checkout found; updating it instead of cloning'
            Push-Location $target
            try {
                Invoke-Native -Exe 'git' -ArgList @('fetch', 'origin', $Branch) -What 'git fetch'
                Invoke-Native -Exe 'git' -ArgList @('checkout', $Branch) -What 'git checkout'
                Invoke-Native -Exe 'git' -ArgList @('pull', '--ff-only', 'origin', $Branch) -What 'git pull --ff-only'
            }
            finally { Pop-Location }
            $reuseCheckout = $true
        }
    }
}

# ---------------------------------------------------------------------------
# 1. prerequisites
# ---------------------------------------------------------------------------

if (-not $SkipPrereqs) {
    Write-Section 'Prerequisites'

    if (-not (Test-CommandAvailable 'winget')) {
        Stop-Install 'winget is missing. Install "App Installer" from the Microsoft Store, or install Rust (https://rustup.rs), Git (https://git-scm.com) and the WebView2 runtime by hand, then re-run with -SkipPrereqs.'
    }

    if (-not (Test-CommandAvailable 'rustc')) {
        Write-Step 'Installing Rust (MSVC toolchain) via winget'
        Invoke-Native -Exe 'winget' -ArgList @('install', '--id', 'Rustlang.Rust.MSVC', '--silent', '--accept-source-agreements', '--accept-package-agreements', '--disable-interactivity') -What 'winget install Rustlang.Rust.MSVC'
        Update-PathFromRegistry
        if (-not (Test-CommandAvailable 'cargo')) {
            Stop-Install 'Rust was installed but cargo is not on PATH in this shell. Open a new terminal (or set PATH from the registry) and re-run.'
        }
    }
    Write-Step "Rust: $(& rustc --version)"

    if (-not (Test-CommandAvailable 'git')) {
        Write-Step 'Installing Git via winget'
        Invoke-Native -Exe 'winget' -ArgList @('install', '--id', 'Git.Git', '--silent', '--accept-source-agreements', '--accept-package-agreements', '--disable-interactivity') -What 'winget install Git.Git'
        Update-PathFromRegistry
        if (-not (Test-CommandAvailable 'git')) {
            Stop-Install 'Git was installed but is not on PATH in this shell. Open a new terminal and re-run.'
        }
    }
    Write-Step "Git: $(& git --version)"

    $installedTargets = Invoke-Native -Exe 'rustup' -ArgList @('target', 'list', '--installed') -What 'rustup target list --installed' -Quiet
    if ($installedTargets -notmatch 'wasm32-unknown-unknown') {
        Write-Step 'Adding the wasm32-unknown-unknown target'
        Invoke-Native -Exe 'rustup' -ArgList @('target', 'add', 'wasm32-unknown-unknown') -What 'rustup target add wasm32-unknown-unknown'
    }
    else { Write-Step 'wasm32-unknown-unknown target present' }

    $edges = @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    )
    $webview2Version = $null
    foreach ($edge in $edges) {
        $entry = Get-ItemProperty -Path $edge -ErrorAction SilentlyContinue
        if ($null -ne $entry -and $entry.pv) { $webview2Version = $entry.pv; break }
    }
    if ($null -eq $webview2Version) {
        Write-WarnOnce 'WebView2 runtime not detected. Windows 11 normally ships it; without it the app window will not open. Install "Microsoft Edge WebView2 Runtime" if the app fails to start.'
    }
    else { Write-Step "WebView2 runtime: $webview2Version" }
}
else { Write-Step 'Prerequisites skipped (-SkipPrereqs)' }

# ---------------------------------------------------------------------------
# 2. clone
# ---------------------------------------------------------------------------

if (-not $reuseCheckout) {
    Write-Section 'Clone'
    Write-Step "git clone --branch $Branch $RepoUrl"
    Invoke-Native -Exe 'git' -ArgList @('clone', '--branch', $Branch, $RepoUrl, $target) -What 'git clone'
}
else { Write-Section 'Checkout is up to date' }

$srcTauri = Join-Path $target 'src-tauri'
if (-not (Test-Path -LiteralPath (Join-Path $srcTauri 'Cargo.toml'))) {
    Stop-Install "$target does not look like desktop-buddy (src-tauri/Cargo.toml missing)"
}

# ---------------------------------------------------------------------------
# 3. Wasm plugin guests
# ---------------------------------------------------------------------------

Write-Section 'Build the Wasm plugin guests'
Write-Note 'plugins/*.wasm are build artifacts and are not in git, so a fresh clone has none'

$pluginDirs = @(Get-ChildItem -LiteralPath (Join-Path $target 'plugins') -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'Cargo.toml') })
if ($pluginDirs.Count -eq 0) { Stop-Install 'no plugin crates found under plugins/' }

foreach ($dir in $pluginDirs) {
    $name = $dir.Name
    Write-Step "cargo build --release --target wasm32-unknown-unknown ($name)"
    Invoke-Native -Exe 'cargo' -ArgList @('build', '--release', '--target', 'wasm32-unknown-unknown', '--manifest-path', (Join-Path $dir.FullName 'Cargo.toml')) -What "building the $name guest"

    $releaseDir = Join-Path $dir.FullName 'target\wasm32-unknown-unknown\release'
    $libName = 'desktop_buddy_{0}_plugin.wasm' -f ($name -replace '-', '_')
    $artifact = Join-Path $releaseDir $libName
    if (-not (Test-Path -LiteralPath $artifact)) {
        $newest = Get-ChildItem -LiteralPath $releaseDir -Filter '*.wasm' -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending | Select-Object -First 1
        if ($null -eq $newest) { Stop-Install "the $name guest produced no .wasm under $releaseDir" }
        $artifact = $newest.FullName
    }
    $destination = Join-Path (Join-Path $target 'plugins') "$name.wasm"
    Copy-Item -LiteralPath $artifact -Destination $destination -Force
    $kb = [math]::Round((Get-Item -LiteralPath $destination).Length / 1KB, 0)
    Write-Note "$destination ($kb KB)"
}

# ---------------------------------------------------------------------------
# 4. the three binaries
# ---------------------------------------------------------------------------

Write-Section 'Build the app (desktop-buddy, -mcp, -watcher)'

$appExe = Join-Path $srcTauri 'target\debug\desktop-buddy.exe'
$mcpExe = Join-Path $srcTauri 'target\debug\desktop-buddy-mcp.exe'
$watcherExe = Join-Path $srcTauri 'target\debug\desktop-buddy-watcher.exe'

if (Test-AvatarApi) { Stop-RunningAvatar }

# The Hermes gateway keeps desktop-buddy-mcp.exe open once the bridge is
# registered, and Windows refuses to overwrite a running image.
if ($null -ne (Get-Process -Name 'desktop-buddy-mcp' -ErrorAction SilentlyContinue)) {
    Clear-LockedBinary -Path $mcpExe
}

Push-Location $srcTauri
try {
    Write-Step 'cargo build --bins'
    Invoke-Native -Exe 'cargo' -ArgList @('build', '--bins') -What 'cargo build --bins'

    foreach ($bin in @($appExe, $mcpExe, $watcherExe)) {
        if (-not (Test-Path -LiteralPath $bin)) { Stop-Install "expected binary missing after the build: $bin" }
        $mb = [math]::Round((Get-Item -LiteralPath $bin).Length / 1MB, 1)
        Write-Note "$(Split-Path -Leaf $bin) ($mb MB)"
    }

    # --lib --bins --tests: the lib and both bins, plus the integration tests in
    # tests/. Doctests are excluded on purpose - this toolchain ships no
    # rustdoc.exe and the doctest stage exits 1.
    if (-not $SkipTests) {
        Write-Section 'Run the test suite (cargo test --lib --bins --tests)'
        Invoke-Native -Exe 'cargo' -ArgList @('test', '--lib', '--bins', '--tests') -What 'cargo test --lib --bins --tests'
    }
    else { Write-Step 'Tests skipped (-SkipTests)' }
}
finally { Pop-Location }

foreach ($stale in @(Get-ChildItem -LiteralPath (Join-Path $srcTauri 'target\debug') -Filter '*.exe.stale' -ErrorAction SilentlyContinue)) {
    Remove-Item -LiteralPath $stale.FullName -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# 5. Hermes MCP registration
# ---------------------------------------------------------------------------

$gatewayTaskName = '\Hermes_Gateway'
$mcpRegistered = $false

if ($SkipHermes) {
    Write-Section 'Hermes wiring skipped (-SkipHermes)'
}
elseif (-not (Test-CommandAvailable 'hermes')) {
    Write-Section 'Hermes not found - skipping the MCP registration'
    Write-WarnOnce 'the "hermes" command is not on PATH; to wire the bridge by hand add this to %LOCALAPPDATA%\hermes\config.yaml'
    Write-Note 'mcp_servers:'
    Write-Note '  desktop-buddy:'
    Write-Note "    command: '$($mcpExe.Replace('\', '\\'))'"
    Write-Note '    args: []'
    Write-Note '    env:'
    Write-Note "      DESKTOP_BUDDY_API_URL: $ApiUrl"
    Write-Note "and restart the gateway: schtasks /End /TN $gatewayTaskName ; schtasks /Run /TN $gatewayTaskName"
}
else {
    Write-Section 'Register the MCP bridge with Hermes'

    # `hermes mcp add` owns the config format. Never hand-edit config.yaml and
    # never rewrite it with Set-Content: PowerShell 5.1 writes a UTF-8 BOM, and
    # the YAML loader then sees the BOM as part of the first key.
    $listText = Invoke-Native -Exe 'hermes' -ArgList @('mcp', 'list') -What 'hermes mcp list' -Quiet
    if ($listText -match '(?m)^\s+desktop-buddy\s{2,}') {
        Write-Step 'desktop-buddy is already registered'
    }
    else {
        Write-Step "hermes mcp add desktop-buddy --command $mcpExe"
        Invoke-Native -Exe 'hermes' -Stdin 'y' -ArgList @(
            'mcp', 'add', 'desktop-buddy',
            '--command', $mcpExe,
            '--env', "DESKTOP_BUDDY_API_URL=$ApiUrl"
        ) -What 'hermes mcp add'
    }

    $listText = Invoke-Native -Exe 'hermes' -ArgList @('mcp', 'list') -What 'hermes mcp list' -Quiet
    if ($listText -match '(?m)^\s+desktop-buddy\s{2,}') {
        Write-Step 'verified: desktop-buddy is a row in "hermes mcp list"'
        $mcpRegistered = $true
    }
    else {
        Write-WarnOnce 'desktop-buddy is NOT listed by "hermes mcp list" after the add; check %LOCALAPPDATA%\hermes\config.yaml'
    }

    $configPath = Join-Path $env:LOCALAPPDATA 'hermes\config.yaml'
    if (Test-Path -LiteralPath $configPath) {
        $configText = Get-Content -LiteralPath $configPath -Raw
        if ($configText -match 'DESKTOP_BUDDY_API_URL') {
            Write-Step 'verified: DESKTOP_BUDDY_API_URL is in the registered env block'
        }
        else {
            Write-WarnOnce 'the config has no DESKTOP_BUDDY_API_URL entry; the bridge will fall back to 127.0.0.1:8765'
        }
    }

    # The gateway is a scheduled task named Hermes_Gateway (not HermesGateway).
    # schtasks exits non-zero for an unknown task and does not throw, so its
    # exit code is the only way to know the restart happened.
    Write-Step "Restarting the Hermes gateway (scheduled task $gatewayTaskName)"
    & schtasks /Query /TN $gatewayTaskName 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-WarnOnce "scheduled task $gatewayTaskName not found. Restart Hermes by hand so it picks the server up: hermes gateway restart"
    }
    else {
        & schtasks /End /TN $gatewayTaskName 2>&1 | Out-Null
        $endCode = $LASTEXITCODE
        Start-Sleep -Seconds 2
        & schtasks /Run /TN $gatewayTaskName 2>&1 | Out-Null
        $runCode = $LASTEXITCODE
        if ($endCode -ne 0 -or $runCode -ne 0) {
            Write-WarnOnce "the gateway task did not restart cleanly (end=$endCode run=$runCode). Run: hermes gateway restart"
        }
        else {
            Write-Step 'gateway task ended and started again'
            Start-Sleep -Seconds 5
        }
    }
}

# ---------------------------------------------------------------------------
# 6. launch, then prove the bridge answers
# ---------------------------------------------------------------------------

Write-Section 'Launch the app'

$appRunning = Test-AvatarApi
if ($NoLaunch) {
    Write-Step 'Launch skipped (-NoLaunch)'
}
elseif ($appRunning) {
    Write-Step 'the app is already answering on the API port'
}
else {
    Write-Step "starting $appExe"
    Start-Process -FilePath $appExe -WorkingDirectory $srcTauri | Out-Null
    for ($i = 0; $i -lt 60; $i++) {
        if (Test-AvatarApi -TimeoutSec 1) { $appRunning = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if ($appRunning) { Write-Step 'verified: /health answered with ok: true' }
    else { Write-WarnOnce "the app did not answer on $ApiUrl/health within 30s. Start $appExe and run verify-install.ps1." }
}

Write-Section 'Prove the MCP bridge answers (real JSON-RPC over stdio)'

$probeScript = Join-Path $target 'scripts\mcp-probe.ps1'
$bridgeProven = $false
if (-not (Test-Path -LiteralPath $probeScript)) {
    Write-WarnOnce "probe script not found at $probeScript; skipping the bridge proof"
}
else {
    . $probeScript
    $probe = Invoke-DesktopBuddyMcpProbe -Exe $mcpExe -ApiUrl $ApiUrl
    if (-not $probe.Ok) {
        Write-WarnOnce "the bridge did not complete the MCP handshake: $($probe.Error)"
    }
    else {
        Write-Step "initialize negotiated $($probe.Protocol); tools/list returned $($probe.ToolCount) avatar_* tools"
        if ($probe.CallAnswered -and -not $probe.CallIsError) {
            Write-Step 'verified: tools/call avatar_get_context returned live desktop context'
            $bridgeProven = $true
        }
        elseif ($probe.CallAnswered -and $appRunning) {
            Write-WarnOnce "avatar_get_context came back as an error while the app is up: $($probe.CallText)"
        }
        elseif ($probe.CallAnswered) {
            Write-Step 'verified: the bridge answered avatar_get_context (app not running, so it reported the avatar as unreachable)'
            $bridgeProven = $true
        }
        else {
            Write-WarnOnce 'tools/call did not get an answer from the bridge'
        }
    }
}

# ---------------------------------------------------------------------------
# 7. summary
# ---------------------------------------------------------------------------

Write-Section 'Summary'

$hermesState = 'not registered'
if ($mcpRegistered) { $hermesState = 'registered' }
$bridgeState = 'not proven'
if ($bridgeProven) { $bridgeState = 'proven' }

Write-Note "repo       : $target"
Write-Note "app        : $appExe"
Write-Note "mcp bridge : $mcpExe"
Write-Note "watcher    : $watcherExe"
Write-Note "plugins    : $(Join-Path $target 'plugins')"
Write-Note "hermes     : $hermesState"
Write-Note "app running: $appRunning"
Write-Note "bridge     : $bridgeState"
Write-Note "warnings   : $script:Warnings"

Write-Host ''
if ($mcpRegistered -and $bridgeProven) {
    Write-Host 'Install complete. One step is left, and only the calling agent can do it:' -ForegroundColor Green
    Write-Host '  call avatar_get_context through its own tool surface (in Hermes:' -ForegroundColor Green
    Write-Host '  mcp__desktop_buddy__avatar_get_context). That is what proves the gateway' -ForegroundColor Green
    Write-Host '  connected the server, which no separate process can observe for it.' -ForegroundColor Green
    exit 0
}

if ($SkipHermes) {
    Write-Host 'Build complete (Hermes wiring skipped).' -ForegroundColor Green
    exit 0
}

Write-Host 'Install finished with warnings - read the "!" lines above.' -ForegroundColor Yellow
exit 1
