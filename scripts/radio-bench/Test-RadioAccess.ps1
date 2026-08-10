[CmdletBinding()]
param(
    [string]$RadioIp = '10.1.0.2',
    [string]$EthernetAdapter,
    [string]$PcIp = '10.1.0.20',
    [int]$PrefixLength = 24,
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\radio-access')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Section {
    param([string]$Message)
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function Select-EthernetAdapter {
    param([string]$Requested)

    $physical = @(Get-NetAdapter -Physical -ErrorAction SilentlyContinue | Sort-Object Name)
    if ($Requested) {
        $match = $physical | Where-Object Name -eq $Requested | Select-Object -First 1
        if (-not $match) { throw "Ethernet adapter '$Requested' was not found." }
        return $match.Name
    }

    $candidates = @($physical | Where-Object {
        $_.Name -notmatch 'Wi-?Fi|Wireless|WLAN' -and
        $_.InterfaceDescription -notmatch 'Wi-?Fi|Wireless|802\.11'
    })
    $suggested = $candidates | Where-Object {
        @(Get-NetIPAddress -InterfaceAlias $_.Name -AddressFamily IPv4 -ErrorAction SilentlyContinue |
            Where-Object IPAddress -like '10.1.0.*').Count -gt 0
    } | Select-Object -First 1
    if (-not $suggested) { $suggested = $candidates | Where-Object Name -eq 'Ethernet 2' | Select-Object -First 1 }
    if (-not $suggested) { $suggested = $candidates | Where-Object Status -eq 'Up' | Select-Object -First 1 }
    if (-not $suggested) { $suggested = $candidates | Select-Object -First 1 }
    if (-not $suggested) { throw 'No physical Ethernet adapter was found.' }

    $answer = Read-Host "Ethernet adapter to use [$($suggested.Name)]"
    if ([string]::IsNullOrWhiteSpace($answer)) { return $suggested.Name }
    $chosen = $physical | Where-Object Name -eq $answer | Select-Object -First 1
    if (-not $chosen) { throw "Ethernet adapter '$answer' was not found." }
    return $chosen.Name
}

function Test-TruePing {
    param([string]$Address)
    $text = (& ping.exe -n 1 -w 900 $Address 2>&1 | Out-String)
    [pscustomobject]@{
        Success = $text -match '(?i)TTL[= ]\d+'
        Output = $text.Trim()
    }
}

function Test-TcpPort {
    param([string]$Address, [int]$Port, [int]$TimeoutMs = 1200)
    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $task = $client.ConnectAsync($Address, $Port)
        if (-not $task.Wait($TimeoutMs)) { return $false }
        return $client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Get-RadioMac {
    param([string]$Address, [string]$Adapter)
    $neighbor = Get-NetNeighbor -InterfaceAlias $Adapter -AddressFamily IPv4 -IPAddress $Address -ErrorAction SilentlyContinue |
        Where-Object { $_.State -notin @('Unreachable', 'Incomplete') } |
        Select-Object -First 1
    if ($neighbor) { return [string]$neighbor.LinkLayerAddress }
    return ''
}

function Save-CurlProbe {
    param([string]$Url, [string]$OutputPath)
    $arguments = @('-k', '-v', '--max-time', '10', '--connect-timeout', '4', $Url)
    # Windows PowerShell 5 wraps a native program's stderr as NativeCommandError.
    # curl -v intentionally writes its diagnostics to stderr, so temporarily use
    # Continue while capturing both streams instead of letting the script stop.
    $previousErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = (& curl.exe @arguments 2>&1 | Out-String)
        $curlExitCode = $LASTEXITCODE
        $output | Set-Content -LiteralPath $OutputPath
        return $curlExitCode
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }
}

function Invoke-RadioTest {
    param([int]$Sequence, [string]$Adapter)

    Write-Section "Radio $Sequence connection"
    Write-Host 'Connect and power on exactly ONE radio. Do not Ethernet-connect both radios while they share 10.1.0.2.' -ForegroundColor Yellow
    Read-Host 'Press Enter after the radio has been powered for at least 30 seconds and Ethernet is connected' | Out-Null

    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $directory = Join-Path $ResultsRoot "radio-$Sequence-$stamp"
    New-Item -ItemType Directory -Force -Path $directory | Out-Null

    $adapterInfo = Get-NetAdapter -Name $Adapter -ErrorAction Stop
    $addresses = @(Get-NetIPAddress -InterfaceAlias $Adapter -AddressFamily IPv4 -ErrorAction SilentlyContinue)
    $hasPcIp = @($addresses | Where-Object IPAddress -eq $PcIp).Count -gt 0

    if (-not $hasPcIp) {
        Write-Host "$Adapter does not currently have $PcIp/$PrefixLength." -ForegroundColor Yellow
        $configure = Read-Host "Configure that PC-side address now? This does not change the radio. [Y/N]"
        if ($configure -match '^(?i)y(es)?$') {
            Set-NetIPInterface -InterfaceAlias $Adapter -AddressFamily IPv4 -Dhcp Disabled
            New-NetIPAddress -InterfaceAlias $Adapter -IPAddress $PcIp -PrefixLength $PrefixLength | Out-Null
            Write-Host "Configured $Adapter as $PcIp/$PrefixLength with no gateway." -ForegroundColor Green
            $hasPcIp = $true
        }
    }

    Write-Host "Waiting up to 90 seconds for $RadioIp to answer..."
    $deadline = (Get-Date).AddSeconds(90)
    $ping = $null
    do {
        $ping = Test-TruePing -Address $RadioIp
        if ($ping.Success) { break }
        Write-Host '.' -NoNewline
        Start-Sleep -Seconds 2
    } until ((Get-Date) -ge $deadline)
    Write-Host ''

    $mac = Get-RadioMac -Address $RadioIp -Adapter $Adapter
    $ports = [ordered]@{}
    foreach ($port in @(22, 23, 80, 443, 830)) {
        $ports[[string]$port] = Test-TcpPort -Address $RadioIp -Port $port
    }

    $httpExit = Save-CurlProbe -Url "http://$RadioIp/" -OutputPath (Join-Path $directory 'http-probe.txt')
    $httpsExit = Save-CurlProbe -Url "https://$RadioIp/" -OutputPath (Join-Path $directory 'https-probe.txt')

    $chudReachable = $false
    $chudDevices = $null
    $chudError = ''
    try {
        $chudDevices = Invoke-RestMethod -Uri 'http://127.0.0.1:8443/api/radio/devices' -TimeoutSec 3
        $chudReachable = $true
        $chudDevices | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $directory 'chud-radio-devices.json')
    } catch {
        $chudError = $_.Exception.Message
        $chudError | Set-Content -LiteralPath (Join-Path $directory 'chud-error.txt')
    }

    Get-NetAdapter | Format-Table Name, Status, LinkSpeed, MacAddress, InterfaceDescription -AutoSize |
        Out-String | Set-Content -LiteralPath (Join-Path $directory 'adapters.txt')
    Get-NetIPConfiguration | Format-List | Out-String |
        Set-Content -LiteralPath (Join-Path $directory 'ip-configuration.txt')
    Get-NetRoute -AddressFamily IPv4 | Sort-Object DestinationPrefix, RouteMetric |
        Format-Table -AutoSize | Out-String | Set-Content -LiteralPath (Join-Path $directory 'routes.txt')
    arp.exe -a | Out-String | Set-Content -LiteralPath (Join-Path $directory 'arp.txt')

    $summary = [pscustomobject]@{
        Timestamp = (Get-Date).ToString('o')
        Sequence = $Sequence
        RadioIp = $RadioIp
        RadioPing = [bool]$ping.Success
        RadioMac = $mac
        EthernetAdapter = $Adapter
        EthernetStatus = [string]$adapterInfo.Status
        EthernetLinkSpeed = [string]$adapterInfo.LinkSpeed
        PcAddressPresent = $hasPcIp
        PcAddress = "$PcIp/$PrefixLength"
        Tcp22 = $ports['22']
        Tcp23 = $ports['23']
        Tcp80 = $ports['80']
        Tcp443 = $ports['443']
        Tcp830 = $ports['830']
        HttpCurlExit = $httpExit
        HttpsCurlExit = $httpsExit
        ChudReachable = $chudReachable
        ChudDeviceCount = if ($chudReachable) { @($chudDevices.devices).Count } else { -1 }
        ResultsDirectory = $directory
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $directory 'summary.json')
    $ping.Output | Set-Content -LiteralPath (Join-Path $directory 'ping.txt')

    Write-Section "Radio $Sequence result"
    $color = if ($summary.RadioPing) { 'Green' } else { 'Red' }
    Write-Host "Ping $RadioIp`: $($summary.RadioPing)" -ForegroundColor $color
    Write-Host "MAC identity: $(if ($mac) { $mac } else { 'not learned' })"
    Write-Host "Ethernet: $Adapter $($summary.EthernetStatus) $($summary.EthernetLinkSpeed)"
    Write-Host "PC address: $($summary.PcAddress) present=$($summary.PcAddressPresent)"
    Write-Host "Ports: SSH/22=$($ports['22']) Telnet/23=$($ports['23']) HTTP/80=$($ports['80']) HTTPS/443=$($ports['443']) NETCONF/830=$($ports['830'])"
    Write-Host "CHUD: reachable=$chudReachable devices=$($summary.ChudDeviceCount)"
    if ($chudError) { Write-Host "CHUD detail: $chudError" -ForegroundColor Yellow }
    Write-Host "Results saved to: $directory" -ForegroundColor Green

    if ($ports['443'] -or $ports['80']) {
        $url = if ($ports['443']) { "https://$RadioIp/" } else { "http://$RadioIp/" }
        $open = Read-Host "Open $url in Google Chrome? [Y/N]"
        if ($open -match '^(?i)y(es)?$') {
            $chromeCandidates = @(
                (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
                (Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe'),
                (Join-Path $env:LOCALAPPDATA 'Google\Chrome\Application\chrome.exe')
            )
            $chrome = $chromeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
            if ($chrome) { Start-Process -FilePath $chrome -ArgumentList @('--new-window', $url) }
            else { Start-Process $url }
        }
    } else {
        Write-Host 'The radio answered no tested web-management port, so no configuration page was opened.' -ForegroundColor Yellow
    }

    return $summary
}

Write-Section 'AVIAN automated radio access test'
Write-Host 'This test is read-only for the radio. It checks Ethernet, ICMP, ARP identity, management ports, web responses, and CHUD.'
Write-Host 'Because both known radios currently use 10.1.0.2, test them one at a time.' -ForegroundColor Yellow

Get-NetAdapter -Physical | Format-Table Name, Status, LinkSpeed, MacAddress, InterfaceDescription -AutoSize
$EthernetAdapter = Select-EthernetAdapter -Requested $EthernetAdapter
New-Item -ItemType Directory -Force -Path $ResultsRoot | Out-Null

$allResults = @()
$sequence = 1
do {
    $allResults += Invoke-RadioTest -Sequence $sequence -Adapter $EthernetAdapter
    $again = Read-Host 'Disconnect this radio. Type A to test another radio, or press Enter to finish'
    $sequence++
} while ($again -match '^(?i)a$')

$combined = Join-Path $ResultsRoot "combined-$(Get-Date -Format 'yyyyMMdd-HHmmss').json"
$allResults | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $combined
Write-Section 'Testing complete'
Write-Host "Combined report: $combined" -ForegroundColor Green
Write-Host 'Leave this window open and send the report path or contents back to Codex.'
