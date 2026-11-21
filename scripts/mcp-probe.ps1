#Requires -Version 5.1
<#
    mcp-probe.ps1 - prove the desktop-buddy MCP bridge actually answers.

    Dot-source this file and call Invoke-DesktopBuddyMcpProbe. Sourcing it has no
    side effects.

    Why this exists: `hermes mcp test desktop-buddy` spawns its own copy of the
    bridge, so it can pass while the gateway is still holding a dead child. This
    probe speaks the real wire protocol to the real binary - newline-delimited
    JSON-RPC 2.0 on stdio (see src-tauri/src/bin/desktop-buddy-mcp.rs) - and
    reports what the server actually said. It deliberately does NOT go through
    Hermes, so it works on a machine with no Hermes at all.
#>

function Invoke-DesktopBuddyMcpProbe {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Exe,

        [string]$ApiUrl = 'http://127.0.0.1:8765',

        [int]$TimeoutMs = 15000
    )

    $result = [pscustomobject]@{
        Ok           = $false
        Protocol     = $null
        ServerName   = $null
        ToolCount    = 0
        Tools        = @()
        CallAnswered = $false
        CallIsError  = $null
        CallText     = $null
        Error        = $null
        Stderr       = @()
        Frames       = @()
    }

    if (-not (Test-Path -LiteralPath $Exe)) {
        $result.Error = "bridge binary not found: $Exe"
        return $result
    }

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ''
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $psi.StandardOutputEncoding = New-Object System.Text.UTF8Encoding $false
    $psi.StandardErrorEncoding = New-Object System.Text.UTF8Encoding $false
    if (-not $psi.EnvironmentVariables.ContainsKey('DESKTOP_BUDDY_API_URL')) {
        $psi.EnvironmentVariables['DESKTOP_BUDDY_API_URL'] = $ApiUrl
    }

    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi

    function Send-Frame {
        param($Obj)
        $json = $Obj | ConvertTo-Json -Compress -Depth 10
        $result.Frames += $json
        # Write bytes, not text: any StreamWriter (including the one inside
        # Process.StandardInput) encodes through whatever UTF8Encoding the
        # console happens to be using, and a UTF-8 that emits its preamble
        # prefixes the first frame with EF BB BF. The bridge then answers
        # "parse error: expected value at line 1 column 1" and the handshake is
        # over before it started. Encoding.GetBytes never emits a preamble.
        $bytes = [Text.Encoding]::UTF8.GetBytes($json + "`n")
        $probeStream = $proc.StandardInput.BaseStream
        $probeStream.Write($bytes, 0, $bytes.Length)
        $probeStream.Flush()
    }

    function Read-Frame {
        param([string]$What)
        $task = $proc.StandardOutput.ReadLineAsync()
        if (-not $task.Wait($TimeoutMs)) {
            throw "timed out after ${TimeoutMs}ms waiting for the $What response"
        }
        $line = $task.Result
        if ($null -eq $line) { throw "the bridge closed stdout while waiting for the $What response" }
        if ([string]::IsNullOrWhiteSpace($line)) { throw "the bridge wrote a blank line instead of the $What response" }
        return ($line | ConvertFrom-Json)
    }

    $stderrTask = $null
    $savedInputEncoding = $null
    try {
        # A redirected child stdin is opened with an encoding taken from
        # Console.InputEncoding, and .NET writes that encoding's preamble into
        # the pipe before the first byte we send. Anything in the caller that
        # has flipped the console to UTF-8 - printing a Unicode table is enough
        # - therefore puts EF BB BF in front of frame one, and the bridge
        # answers "parse error: expected value at line 1 column 1". Pin the
        # console input encoding to UTF-8 without a preamble while the child
        # runs, so the handshake does not depend on what the caller printed.
        try {
            $savedInputEncoding = [Console]::InputEncoding
            [Console]::InputEncoding = (New-Object System.Text.UTF8Encoding $false)
        }
        catch { $savedInputEncoding = $null }

        if (-not $proc.Start()) { throw "could not start $Exe" }
        # Drain stderr with ReadToEndAsync, NOT with a BeginErrorReadLine event
        # handler: PowerShell runs those scriptblocks on a thread-pool thread
        # that has no runspace, and the resulting unhandled exception kills the
        # whole PowerShell process with exit 1 and no message. The child writes
        # its startup banner to stderr, so that race is hit almost every time.
        $stderrTask = $proc.StandardError.ReadToEndAsync()

        Send-Frame @{
            jsonrpc = '2.0'
            id      = 1
            method  = 'initialize'
            params  = @{
                protocolVersion = '2025-06-18'
                capabilities    = @{}
                clientInfo      = @{ name = 'desktop-buddy-install-probe'; version = '1.0.0' }
            }
        }
        $init = Read-Frame 'initialize'
        if ($init.PSObject.Properties.Name -contains 'error') {
            throw "initialize returned an error: $($init.error.message)"
        }
        $result.Protocol = $init.result.protocolVersion
        $result.ServerName = $init.result.serverInfo.name

        Send-Frame @{ jsonrpc = '2.0'; method = 'notifications/initialized' }

        Send-Frame @{ jsonrpc = '2.0'; id = 2; method = 'tools/list'; params = @{} }
        $list = Read-Frame 'tools/list'
        if ($list.PSObject.Properties.Name -contains 'error') {
            throw "tools/list returned an error: $($list.error.message)"
        }
        $names = @($list.result.tools | ForEach-Object { $_.name })
        $result.Tools = $names
        $result.ToolCount = $names.Count
        if ($result.ToolCount -lt 1) { throw 'tools/list returned no tools' }

        Send-Frame @{
            jsonrpc = '2.0'
            id      = 3
            method  = 'tools/call'
            params  = @{ name = 'avatar_get_context'; arguments = @{} }
        }
        $call = Read-Frame 'tools/call'
        if ($call.PSObject.Properties.Name -contains 'error') {
            throw "tools/call returned a protocol error: $($call.error.message)"
        }
        $result.CallAnswered = $true
        $result.CallIsError = [bool]$call.result.isError
        $text = $null
        foreach ($item in @($call.result.content)) {
            if ($item.type -eq 'text' -and $null -ne $item.text) { $text = [string]$item.text; break }
        }
        if ($null -ne $text -and $text.Length -gt 300) { $text = $text.Substring(0, 300) + '...' }
        $result.CallText = $text

        $result.Ok = $true
    }
    catch {
        $result.Error = $_.Exception.Message
    }
    finally {
        try { if ($null -ne $savedInputEncoding) { [Console]::InputEncoding = $savedInputEncoding } } catch { }
        try { $proc.StandardInput.Close() } catch { }
        try {
            if (-not $proc.WaitForExit(3000)) { $proc.Kill() }
        }
        catch { }
        try {
            if ($null -ne $stderrTask -and $stderrTask.Wait(1000)) {
                $result.Stderr = @($stderrTask.Result -split "`r?`n" | Where-Object { $_ -ne '' })
            }
        }
        catch { }
    }

    return $result
}
