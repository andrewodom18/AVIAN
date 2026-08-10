[CmdletBinding()]
param(
    [string]$EthernetAdapter = 'Ethernet 2',
    [string]$DirectRadioIPv6 = 'fe80::21e:3fff:fe17:abf0',
    [string]$RemoteRadioIPv6 = 'fe80::21e:3fff:fe20:9a10',
    [ValidateRange(1, 30)]
    [int]$IntervalSeconds = 2,
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-ScopedIPv6Ping {
    param([string]$Address, [int]$InterfaceIndex)

    $target = "$Address%$InterfaceIndex"
    $reply = (& ping.exe -6 -n 1 -w 900 $target 2>&1 | Out-String)
    [pscustomobject]@{
        Target = $target
        Reachable = [bool]($reply -match '(?i)TTL[= ]\d+')
        Output = $reply.Trim()
    }
}

$adapter = Get-NetAdapter -Name $EthernetAdapter -ErrorAction Stop
$interfaceIndex = [int]$adapter.ifIndex
$runId = Get-Date -Format 'yyyyMMdd-HHmmss'
$resultDirectory = Join-Path $ResultsRoot "$runId-two-radio-mesh"
New-Item -ItemType Directory -Force -Path $resultDirectory | Out-Null
$timelinePath = Join-Path $resultDirectory 'two-radio-mesh-timeline.csv'
$summaryPath = Join-Path $resultDirectory 'summary.txt'

Write-Host "`n=== Two-radio TrellisWare mesh test ===" -ForegroundColor Cyan
Write-Host "Direct radio (Radio 2): $DirectRadioIPv6%$interfaceIndex"
Write-Host "Remote radio (Radio 1): $RemoteRadioIPv6%$interfaceIndex"
Write-Host 'Power both radios. Connect only Radio 2 to Ethernet 2; Radio 1 must remain Ethernet-unplugged.' -ForegroundColor Yellow
Read-Host 'Press Enter after both radios are powered and Radio 2 is connected' | Out-Null

$firstDirect = $null
$firstRemote = $null
Write-Host 'Monitoring. Press Q to stop and save the report.' -ForegroundColor Green

try {
    while ($true) {
        $adapter = Get-NetAdapter -Name $EthernetAdapter -ErrorAction SilentlyContinue
        $direct = Test-ScopedIPv6Ping -Address $DirectRadioIPv6 -InterfaceIndex $interfaceIndex
        $remote = Test-ScopedIPv6Ping -Address $RemoteRadioIPv6 -InterfaceIndex $interfaceIndex
        $timestamp = (Get-Date).ToString('o')

        if (-not $firstDirect -and $direct.Reachable) { $firstDirect = $timestamp }
        if (-not $firstRemote -and $remote.Reachable) { $firstRemote = $timestamp }

        [pscustomobject]@{
            Timestamp = $timestamp
            EthernetStatus = if ($adapter) { [string]$adapter.Status } else { 'Missing' }
            DirectTarget = $direct.Target
            DirectReachable = $direct.Reachable
            RemoteTarget = $remote.Target
            RemoteReachable = $remote.Reachable
        } | Export-Csv -LiteralPath $timelinePath -NoTypeInformation -Append

        $directColor = if ($direct.Reachable) { 'Green' } else { 'Yellow' }
        $remoteColor = if ($remote.Reachable) { 'Green' } else { 'Yellow' }
        Write-Host "[$timestamp] Radio 2 direct: $($direct.Reachable)" -ForegroundColor $directColor
        Write-Host "[$timestamp] Radio 1 over mesh: $($remote.Reachable)" -ForegroundColor $remoteColor

        if ([Console]::KeyAvailable -and [Console]::ReadKey($true).Key -eq [ConsoleKey]::Q) { break }
        Start-Sleep -Seconds $IntervalSeconds
    }
} finally {
    $verdict = if ($firstRemote) {
        'PASS: Radio 1 was reachable through the Radio 2 Ethernet attachment and RF path.'
    } elseif ($firstDirect) {
        'PARTIAL: Radio 2 was locally reachable, but Radio 1 was not reachable over the RF path.'
    } else {
        'FAIL: The directly attached Radio 2 IPv6 management address was not reachable.'
    }
    @(
        "Two-radio mesh test: $runId",
        "Ethernet adapter: $EthernetAdapter (#$interfaceIndex)",
        "Radio 2 direct IPv6: $DirectRadioIPv6",
        "Radio 1 remote IPv6: $RemoteRadioIPv6",
        "First direct reply: $(if ($firstDirect) { $firstDirect } else { 'not observed' })",
        "First remote reply: $(if ($firstRemote) { $firstRemote } else { 'not observed' })",
        $verdict
    ) | Set-Content -LiteralPath $summaryPath
    Write-Host "`n$verdict" -ForegroundColor Cyan
    Write-Host "Results saved to: $resultDirectory" -ForegroundColor Green
}
