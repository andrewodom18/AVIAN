#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$EthernetAdapter = 'Ethernet 2',
    [string]$ChudUrl = 'http://127.0.0.1:8443',
    [string]$ArcUrl = 'http://127.0.0.1:9101',
    [string]$UiUrl = 'https://localhost:3000/home/devices',
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\tw950-live'),
    [ValidateRange(15,180)][int]$ObserveSeconds = 30,
    [switch]$PromptForChudApiKey,
    [switch]$PreflightOnly,
    [switch]$UiRegression,
    [switch]$LibraryOnly
)

$ErrorActionPreference = 'Stop'
function Field($Object, [string]$Name) {
    if ($null -ne $Object -and $null -ne $Object.PSObject.Properties[$Name]) { return $Object.$Name }
    return $null
}
function Normalize-Mac([string]$Value) {
    if ($Value -notmatch '^(?:[0-9a-fA-F]{12}|[0-9a-fA-F]{2}(?:[:-][0-9a-fA-F]{2}){5})$') { return '' }
    $v = ($Value -replace '[:-]', '').ToLowerInvariant()
    if ($v -in @('000000000000','ffffffffffff')) { return '' }
    return $v
}
function Answer-Status([string]$Answer) {
    switch -Regex ($Answer.Trim()) {
        '^(?i)y(es)?$' { return 'PASS' }
        '^(?i)n(o)?$' { return 'FAIL' }
        '^(?i)u(nknown)?$' { return 'BLOCKED' }
        '^(?i)s(kip)?$' { return 'SKIPPED' }
        '^(?i)q(uit)?$' { return 'QUIT' }
        default { return '' }
    }
}
function Verdict($Steps, [bool]$Completed) {
    if (@($Steps | Where-Object status -eq FAIL).Count) { return 'FAIL' }
    if (@($Steps | Where-Object status -eq BLOCKED).Count) { return 'BLOCKED' }
    if (-not $Completed -or @($Steps | Where-Object status -eq SKIPPED).Count) { return 'INCOMPLETE' }
    return 'PASS'
}
function Assert-Loopback([string]$Url) {
    $uri = [uri]$Url
    if (-not $uri.IsAbsoluteUri -or $uri.Scheme -notin @('http','https') -or
        $uri.Host -notin @('localhost','127.0.0.1','[::1]') -or $uri.UserInfo -or $uri.Query -or $uri.Fragment) {
        throw 'Only credential-free loopback service URLs are permitted.'
    }
}
function Redact($Value) {
    if ($null -eq $Value) { return $null }
    if ($Value -is [string] -or $Value -is [ValueType]) { return $Value }
    if ($Value -is [System.Collections.IDictionary]) {
        $out = [ordered]@{}
        foreach ($key in $Value.Keys) {
            $out[$key] = if ($key -match '(?i)password|secret|token|certificate|private.?key|api.?key|credential|^config$|^desired$|^delta$') { '[REDACTED]' } else { Redact $Value[$key] }
        }
        return $out
    }
    if ($Value -is [System.Collections.IEnumerable]) { return ,@($Value | ForEach-Object { Redact $_ }) }
    $out = [ordered]@{}
    foreach ($p in $Value.PSObject.Properties) {
        $out[$p.Name] = if ($p.Name -match '(?i)password|secret|token|certificate|private.?key|api.?key|credential|^config$|^desired$|^delta$') { '[REDACTED]' } else { Redact $p.Value }
    }
    return $out
}
function Save([string]$Name, $Value) {
    Redact $Value | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath (Join-Path $script:RunRoot $Name) -Encoding UTF8
}
function Persist {
    $script:Report.verdict = Verdict $script:Steps $script:Report.completed
    $script:Report.steps = @($script:Steps.ToArray())
    $script:Report.updated_utc = [DateTime]::UtcNow.ToString('o')
    Save 'report.json' $script:Report
    $script:Steps | Export-Csv -NoTypeInformation -LiteralPath (Join-Path $script:RunRoot 'steps.csv')
    @("TW-950 live validation: $($script:Report.id)", "Verdict: $($script:Report.verdict)",
      'READ-ONLY. No radio settings, certificates, host networking or app configuration changed.',
      'PASS applies only to listed checks; configuration writes and flight are NOT TESTED.',
      "Evidence: $script:RunRoot", '') + @($script:Steps | ForEach-Object { "[$($_.status)] $($_.id): $($_.detail)" }) |
        Set-Content -LiteralPath (Join-Path $script:RunRoot 'summary.txt') -Encoding UTF8
}
function Result([string]$Id, [string]$Status, [string]$Detail, [string]$Source = 'automatic') {
    $script:Steps.Add([pscustomobject]@{id=$Id;status=$Status;detail=$Detail;source=$Source;utc=[DateTime]::UtcNow.ToString('o')})
    Persist
    Write-Host "[$Status] $Id - $Detail" -ForegroundColor $(if ($Status -eq 'PASS') {'Green'} elseif ($Status -eq 'FAIL') {'Red'} else {'Yellow'})
}
function Ask([string]$Id, [string]$Question) {
    do { $answer = Answer-Status (Read-Host "$Question [Y/N/U=unknown/S=skip/Q=quit]") } until ($answer)
    if ($answer -eq 'QUIT') { throw 'OPERATOR_QUIT' }
    $note = if ($answer -eq 'PASS') { 'Operator confirmed.' } else { Read-Host 'What happened? Do not enter passwords or secrets' }
    Result $Id $answer $note 'operator'
    return ($answer -eq 'PASS')
}
function Confirm([string]$Text, [string]$Word) {
    do {
        $answer = (Read-Host "$Text Type $Word (or Q to quit)").Trim()
        if ($answer -match '^(?i)q(uit)?$') { throw 'OPERATOR_QUIT' }
    } until ($answer.Equals($Word,[StringComparison]::OrdinalIgnoreCase))
}
function Get-Api([string]$Url, [switch]$Chud) {
    $headers = @{}
    if ($Chud -and $script:ApiKey) { $headers['X-API-Key'] = $script:ApiKey }
    try {
        $r = Invoke-WebRequest -UseBasicParsing -Uri $Url -Method Get -Headers $headers -TimeoutSec 5 -MaximumRedirection 0
        return [pscustomobject]@{status=[int]$r.StatusCode;body=($r.Content | ConvertFrom-Json);utc=[DateTime]::UtcNow.ToString('o')}
    } catch {
        $status = 0
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        # Never persist raw response/error text: it may include configuration secrets.
        return [pscustomobject]@{status=$status;body=$null;utc=[DateTime]::UtcNow.ToString('o')}
    }
}
function Device-Matches($Device, [string]$Mac) {
    foreach ($key in @('mac','mac_address')) { if ((Normalize-Mac ([string](Field $Device $key))) -eq $Mac -and $Mac) { return $true } }
    return $false
}
function Eligible($Devices, [string]$Mac) {
    $matches = @($Devices | Where-Object { Device-Matches $_ $Mac })
    return ($matches.Count -eq 1 -and (Field $matches[0] 'driver_available') -eq $true -and
        (Field $matches[0] 'state') -in @('confirmed','connected','managed'))
}
function Fleet-Ids($Response) {
    return @((Field (Field $Response 'body') 'records') | ForEach-Object { [string](Field $_ 'record_id') } | Sort-Object -Unique)
}
function Valid-Fleet($Response) {
    $body = Field $Response 'body'
    if ((Field $Response 'status') -ne 200 -or $null -eq $body -or $null -eq $body.PSObject.Properties['records']) { return $false }
    foreach ($record in @($body.records)) { if (-not (Field $record 'record_id')) { return $false } }
    return $true
}
function Has-Simulation($Value) {
    if ($null -eq $Value -or $Value -is [string] -or $Value -is [ValueType]) { return $false }
    if ($Value -is [System.Collections.IEnumerable] -and $Value -isnot [System.Collections.IDictionary]) {
        foreach ($item in $Value) { if (Has-Simulation $item) { return $true } }
        return $false
    }
    foreach ($p in $Value.PSObject.Properties) {
        if ($p.Name -in @('simulated','simulation','mock','demo') -and $p.Value -eq $true) { return $true }
        if ($p.Name -eq 'capability_hints' -and 'mock' -in @($p.Value)) { return $true }
        if ($p.Name -in @('source','source_authority') -and $p.Value -in @('simulation','simulated','mock','demo')) { return $true }
        if (Has-Simulation $p.Value) { return $true }
    }
    return $false
}
function Capture([string]$Phase, [int]$Seconds = 0) {
    $samples = [System.Collections.Generic.List[object]]::new()
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        $diagnostic = $null
        $diagPath = Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\live-discovery\latest-discovery.json'
        if (Test-Path -LiteralPath $diagPath) {
            try { $diagnostic = @{file_updated_utc=(Get-Item -LiteralPath $diagPath).LastWriteTimeUtc.ToString('o'); observations=(Get-Content -LiteralPath $diagPath -Raw | ConvertFrom-Json)} } catch {}
        }
        $sample = [pscustomobject]@{
            utc=[DateTime]::UtcNow.ToString('o'); chud=(Get-Api "$ChudUrl/api/radio/devices" -Chud)
            mesh=(Get-Api "$ArcUrl/api/radio/mesh"); fleet=(Get-Api "$ArcUrl/api/fleet"); diagnostic=$diagnostic
        }
        $samples.Add($sample)
        Save "$Phase.json" @($samples.ToArray())
        if ((Has-Simulation $sample.chud.body) -or (Has-Simulation $sample.mesh.body) -or (Has-Simulation $sample.fleet.body)) {
            throw 'Simulation provenance appeared in a live API snapshot. Stop hardware testing and inspect the saved evidence.'
        }
        if ([DateTime]::UtcNow -ge $deadline) { break }
        Start-Sleep -Seconds 3
    } while ($true)
    return $sample
}
function Read-RadioMac([int]$Number) {
    do {
        $value = (Read-Host "Radio $Number label Ethernet MAC (not serial number; S if unreadable, Q to quit)").Trim()
        if ($value -eq 'q') { throw 'OPERATOR_QUIT' }
        if ($value -eq 's') { return '' }
        $mac = Normalize-Mac $value
    } until ($mac)
    return $mac
}
function Read-RadioIp {
    do {
        $value = (Read-Host 'Management IPv4 address [Enter = 10.1.0.2]').Trim()
        if (-not $value) { $value = '10.1.0.2' }
        if ($value -eq 'q') { throw 'OPERATOR_QUIT' }
        $ip = $null
    } until ([System.Net.IPAddress]::TryParse($value,[ref]$ip) -and $ip.AddressFamily -eq 'InterNetwork' -and -not [System.Net.IPAddress]::IsLoopback($ip))
    return $value
}
function Physical-Probe([string]$Ip, [string]$Mac) {
    $route = Find-NetRoute -RemoteIPAddress $Ip -ErrorAction SilentlyContinue | Where-Object { Field $_ 'IPAddress' } | Select-Object -First 1
    $routeOk = $route -and $route.InterfaceIndex -eq $script:Adapter.ifIndex
    $reply = $false
    if ($routeOk) {
        $p = [System.Net.NetworkInformation.Ping]::new()
        try { $reply = $p.Send($Ip,1500).Status -eq 'Success' } catch {} finally { $p.Dispose() }
    }
    $neighbors = @(Get-NetNeighbor -InterfaceIndex $script:Adapter.ifIndex -IPAddress $Ip -ErrorAction SilentlyContinue |
        Select-Object IPAddress,LinkLayerAddress,State,InterfaceIndex)
    $matched = @($neighbors | Where-Object { (Normalize-Mac $_.LinkLayerAddress) -eq $Mac -and $_.State -eq 'Reachable' })
    return [pscustomobject]@{utc=[DateTime]::UtcNow.ToString('o');ip=$Ip;expected_mac=$Mac;route_on_selected_adapter=[bool]$routeOk;icmp_reply=$reply;neighbors=$neighbors;verified=([bool]$Mac -and $reply -and $routeOk -and $matched.Count -eq 1)}
}

if ($LibraryOnly) { return }
foreach ($url in @($ChudUrl,$ArcUrl,$UiUrl)) { Assert-Loopback $url }
$ChudUrl=$ChudUrl.TrimEnd('/'); $ArcUrl=$ArcUrl.TrimEnd('/')
$id = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0,6)
$script:RunRoot = Join-Path $ResultsRoot $id
New-Item -ItemType Directory -Path $script:RunRoot -Force | Out-Null
$script:Steps = [System.Collections.Generic.List[object]]::new()
$script:ApiKey = $null
$script:Report = [ordered]@{id=$id;started_utc=[DateTime]::UtcNow.ToString('o');completed=$false;simulation=$false;radio_writes='NOT TESTED';flight='NOT TESTED';verdict='INCOMPLETE';steps=@();updated_utc='';scope='Live two-TW950 discovery, CHUD read authority, ARC display and gated RF-peer observation';script_sha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash}
Persist
try {
    Write-Host "LIVE TW-950 TEST - no simulator. Results: $script:RunRoot" -ForegroundColor Cyan
    Write-Host 'This test will not start/stop apps, open a browser, change networking, install certificates, or write radios.'
    Write-Host 'Keep radios OFF for preflight. Start the real bench stack separately if requested. Internet is not needed after startup.'
    try { $dockerText = & docker ps --format '{{.Names}}' 2>$null }
    catch { throw 'Docker is not ready. Rerun the Desktop guide with -StartApps and keep both radios OFF. It starts the real bench stack, not the simulator.' }
    if ($LASTEXITCODE -ne 0) { throw 'Docker is not ready. Start Docker Desktop, then the real bench stack with Start-RadioBenchTest.ps1 -SkipBrowser -SkipConnectionMonitor. Rerun this guide.' }
    $required = @('arc-avian-local-comms-1','arc-avian-local-dev-bridge-1','arc-avian-real-link-manager','chud-local')
    if (@($required | Where-Object { $_ -notin @($dockerText) }).Count) { throw 'Required real bench containers are missing. Run Start-RadioBenchTest.ps1 -SkipBrowser -SkipConnectionMonitor with radios OFF, then rerun.' }
    $runtime = @(& docker inspect @required | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect the live stack.' }
    $runtimeEvidence = @($runtime | ForEach-Object {
        $cmd = @($_.Config.Entrypoint) + @($_.Config.Cmd)
        $demoEnv = @($_.Config.Env | Where-Object { $_ -match '(?i)(DEMO|SIMULAT|MOCK)[^=]*=(true|1|yes)$' }).Count -gt 0
        [pscustomobject]@{name=$_.Name;image_id=$_.Image;started=$_.State.StartedAt;mock_or_demo=($demoEnv -or (($cmd -join ' ') -match '(?i)(--mock\b|--demo\b|simulate-radio|local-sim)'));radio_transport=(($cmd -join ' ') -match '--radio-control-zenoh-endpoint');fleet_injection=(($cmd -join ' ') -match '--zenoh-endpoint\b')}
    })
    Save 'runtime.json' $runtimeEvidence
    if (@($dockerText | Where-Object { $_ -match '(?i)local-sim|simulate-radio|local-radio-plugin' }).Count -or @($runtimeEvidence | Where-Object mock_or_demo).Count) { throw 'Simulation detected in the bench stack. Do not connect hardware.' }
    $bridge = $runtimeEvidence | Where-Object name -eq '/arc-avian-local-dev-bridge-1'
    if (-not $bridge.radio_transport -or $bridge.fleet_injection) { throw 'ARC is not using the real-hardware radio-only transport override.' }
    Result 'real-runtime' PASS 'Required containers run without detected demo/mock flags; ARC uses radio-only transport. Image IDs saved.'
    $script:Adapter = Get-NetAdapter -Name $EthernetAdapter
    Save 'adapter.json' @{adapter=($script:Adapter | Select-Object Name,ifIndex,Status,InterfaceDescription);addresses=@(Get-NetIPAddress -InterfaceIndex $script:Adapter.ifIndex -AddressFamily IPv4 | Select-Object IPAddress,PrefixLength);routes=@(Get-NetRoute -InterfaceIndex $script:Adapter.ifIndex -AddressFamily IPv4 | Select-Object DestinationPrefix,NextHop,RouteMetric)}
    if ($PromptForChudApiKey) {
        $secret = Read-Host 'CHUD API key (hidden; not your radio certificate password)' -AsSecureString
        $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secret)
        try { $script:ApiKey = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr); $secret.Dispose() }
    }
    $pre = Capture '00-preflight'
    foreach ($service in @('chud','mesh','fleet')) {
        $code = $pre.$service.status
        if ($code -ne 200) { Result "api-$service" BLOCKED "HTTP $code (0 = connection/TLS/JSON failure). 401/403 = API authentication, NOT radio-certificate failure. For CHUD authentication rerun with -PromptForChudApiKey." }
        else { Result "api-$service" PASS 'API returned JSON; device authority is checked separately.' }
    }
    if ($pre.fleet.status -ne 200 -or $pre.mesh.status -ne 200) { throw 'ARC APIs are unavailable. Repair/start the real bench stack before hardware testing.' }
    if (-not (Valid-Fleet $pre.fleet)) { throw 'ARC Fleet returned an unsupported identity schema. Do not infer no-drone-creation from missing fields.' }
    if (($pre.fleet.body | ConvertTo-Json -Depth 30 -Compress) -match '(?i)SIMULATION-|"mock"|"source"\s*:\s*"simulation"') { throw 'ARC Fleet contains mock/simulation evidence. Do not connect radios to this runtime.' }
    $health = Get-Api "$ArcUrl/api/radio/streamcaster/health"
    Save 'coordination-health.json' $health
    if ($health.status -ne 200 -or (Field $health.body 'coordination_reachable') -ne $true) { throw 'ARC radio coordination is unavailable. This is an application transport blocker, not a radio discovery failure.' }
    if ($PreflightOnly) { Result 'hardware-phases' SKIPPED 'Preflight only; no hardware contact or operator checks performed.'; return }
    if (-not (Ask 'safety' 'Approved antennas/loads installed, bench RF operation authorized, no propellers/flight devices attached, both radios OFF?')) { throw 'Safety confirmation not satisfied.' }
    Write-Host "Use your existing browser at $UiUrl. Do not open CHUD or a vendor app; ARC is the only operator UI." -ForegroundColor Cyan
    Confirm 'Both radios powered OFF and Ethernet unplugged.' 'DISCONNECTED'
    $baseline = Capture '01-disconnected' $ObserveSeconds
    [void](Ask 'baseline-radio' 'After Refresh Live Data, does RADIO NETWORK show zero online radios?')
    [void](Ask 'baseline-fleet' 'Are saved drones absent or clearly offline, with no green Ready placeholder?')
    if($UiRegression){
        [void](Ask 'ui-no-disconnected-toggle' 'Open RADIO NETWORK. Is the Show disconnected peers checkbox gone?')
        Write-Host 'For the next steps, do NOT click Refresh Live Data, Refresh CHUD discovery, or browser refresh. We are checking automatic updates.' -ForegroundColor Cyan
    }
    $identities = @()
    for ($n=1; $n -le 2; $n++) {
        Write-Host "`nRADIO $n ONLY - keep the other radio OFF." -ForegroundColor Cyan
        $mac = Read-RadioMac $n
        $ip = Read-RadioIp
        if ($mac -and $mac -in @($identities.mac)) { throw 'Both radio labels have the same MAC entry. Correct the identity entry before continuing.' }
        $identities += [pscustomobject]@{radio=$n;mac=$mac;ip=$ip}
        Save 'radio-identities.json' $identities
        Confirm "Power Radio $n, connect only it to $EthernetAdapter, and wait at least 30 seconds." 'CONNECTED'
        $physical = Physical-Probe $ip $mac
        Save "radio-$n-windows.json" $physical
        if ($physical.verified) { Result "radio-$n-windows" PASS 'Selected-interface route, ICMP response and reachable neighbor match the physical label MAC.' }
        else { Result "radio-$n-windows" BLOCKED 'Physical identity not proven. See route/ICMP/neighbor evidence; a stale cache or blocked ping is not proof of a live radio.' }
        $live = Capture "radio-$n-connected" $ObserveSeconds
        $devices = @(Field $live.chud.body 'devices')
        $eligible = $physical.verified -and $live.chud.status -eq 200 -and (Eligible $devices $mac)
        if ($eligible) {
            Result "radio-$n-chud-inventory" PASS 'Exactly one label-matching CHUD record is confirmed/connected/managed with driver_available=true. Authenticated read still required.'
            $wireMac = ([regex]::Matches($mac,'..') | ForEach-Object Value) -join ':'
            $snapshot = Get-Api "$ChudUrl/api/radio/snapshot?mac=$([uri]::EscapeDataString($wireMac))" -Chud
            # Radio config values are deliberately excluded from files. Metadata remains available.
            Save "radio-$n-chud-snapshot.json" $snapshot
            if (Has-Simulation $snapshot.body) { throw 'CHUD returned a simulated snapshot. No live configuration test is permitted.' }
            if ($snapshot.status -eq 200 -and $null -ne (Field $snapshot.body 'config') -and $null -ne (Field $snapshot.body 'meta')) {
                Result "radio-$n-chud-read" PASS 'CHUD snapshot returned configuration and metadata through its real API. No writes performed.'
                [void](Ask "radio-$n-arc-controls" "Select Radio $n in ARC and open Configure Selected Radio. Does the same MAC load actual settings without radio-not-found/auth errors? DO NOT click Apply or Save.")
            } else { Result "radio-$n-chud-read" BLOCKED "CHUD snapshot HTTP $($snapshot.status). 401/403 may be API auth; 501 may be an unimplemented driver. No radio write allowed." }
        } else {
            Result "radio-$n-chud-authority" BLOCKED 'No verified physical identity plus unique eligible CHUD record. Windows observation is not CHUD management authority.'
            [void](Ask "radio-$n-fail-closed" 'Is Configure Selected Radio disabled or explicitly blocked, rather than claiming a successful configuration?')
        }
        [void](Ask "radio-$n-display" "Does ARC RADIO NETWORK show the correct Radio $n identity and honest status (observed/auth-required/managed), not an invented drone?")
        if($UiRegression){
            [void](Ask "radio-$n-auto-appearance" 'Did the radio appear without clicking any refresh button? UI polling is every second; physical discovery can take longer.')
            [void](Ask "radio-$n-minimal-details" 'Open Live radio details. Are Management IP, MAC, and Interface the ONLY three fields, with the correct values?')
            if(-not $eligible){[void](Ask "radio-$n-pending-label" 'For this discovery-only radio, does the row say CHUD pending instead of certificate required, while configuration stays blocked?')}
            [void](Ask "radio-$n-ui-stability" 'Leave the panel open for 10 seconds, then collapse/reopen it. Does it stay responsive without constant loading flashes or Failed to fetch?')
        }
        if ((Valid-Fleet $baseline.fleet) -and (Valid-Fleet $live.fleet)) {
            $same = ((Fleet-Ids $baseline.fleet) -join '|') -ceq ((Fleet-Ids $live.fleet) -join '|')
            if ($same) { Result "radio-$n-fleet" PASS 'Fleet record identities stayed unchanged during radio-only discovery.' }
            else { Result "radio-$n-fleet" FAIL 'Fleet record identities changed during a radio-only test. Review snapshots; record counts alone are insufficient.' }
        } else { Result "radio-$n-fleet" BLOCKED 'Fleet API unavailable; cannot compare identity sets.' }
        Confirm "Power OFF and unplug Radio $n before touching the other radio." 'DISCONNECTED'
        $off = Capture "radio-$n-disconnected" $ObserveSeconds
        $refreshInstruction=if($UiRegression){'without clicking refresh'}else{'and Refresh Live Data'}
        [void](Ask "radio-$n-expiry" "After $ObserveSeconds seconds $refreshInstruction, has Radio $n stopped showing online? Saved entries may remain clearly stale/offline.")
    }
    if (@($identities | Where-Object { $_.mac }).Count -eq 2) { Result 'sequential-identity' PASS 'Two different physical label MACs recorded. This does NOT prove concurrent duplicate-IP support.' }
    else { Result 'sequential-identity' BLOCKED 'One or more physical label identities could not be recorded.' }
    Write-Host '`nTWO-RADIO OVER-AIR CHECK - no configuration is changed by this guide.' -ForegroundColor Cyan
    $meshReady = Ask 'mesh-prerequisites' 'Are both radios ALREADY on an approved matching RF network, with distinct management IPs OR vendor-verified factory-IP isolation? If unknown choose U. Do not power both merely to try it.'
    if ($meshReady) {
        Confirm 'Power both radios with approved antennas/loads. Connect Ethernet ONLY to Radio 1; Radio 2 has power only, no USB/Ethernet data cable.' 'MESH READY'
        $meshOn = Capture 'mesh-peer-on' $ObserveSeconds
        [void](Ask 'mesh-peer-identity' 'Does ARC identify Radio 2 by its real identity as an over-air peer, with explicit CHUD/vendor RF topology evidence, not just a remembered inventory row or logical PEAT path?')
        Confirm 'Keep Radio 1 connected and powered. Power OFF Radio 2 only.' 'PEER OFF'
        $meshOff = Capture 'mesh-peer-off' $ObserveSeconds
        [void](Ask 'mesh-peer-loss' 'Does Radio 2 become stale/offline and its measured RF edge disappear while Radio 1 remains available?')
        Confirm 'Power Radio 2 back ON with no data cable; wait 30 seconds.' 'PEER ON'
        $meshReturn = Capture 'mesh-peer-return' $ObserveSeconds
        [void](Ask 'mesh-peer-return' 'Does the same Radio 2 identity and measured RF edge return without duplicate nodes?')
        Result 'mesh-evidence-scope' INFO 'Three API timelines captured. RF-edge assertions are operator observations; inspect source/freshness in evidence before claiming CHUD mesh support.'
    } else { Result 'mesh-sequence' SKIPPED 'Both-radio phase not attempted without an already-compatible, collision-safe configuration.' }
    Confirm 'Power OFF both radios and unplug Ethernet. Existing applications are left running.' 'DISCONNECTED'
    $final = Capture '99-final' $ObserveSeconds
    [void](Ask 'final-disconnected' 'Does ARC show no live radio or ready drone originating from these powered-off radios?')
    Result 'configuration-write-scope' INFO 'Apply/persist/rollback, certificate installation, RF settings and flight NOT TESTED. A separate approved write test requires successful CHUD snapshot and ARC control gates.'
    $script:Report.completed = $true
} catch {
    if ($_.Exception.Message -eq 'OPERATOR_QUIT') { Result 'operator-stop' SKIPPED 'Operator quit; partial evidence retained.' }
    else { Result 'test-blocker' BLOCKED $_.Exception.Message }
} finally {
    $script:ApiKey = $null
    Persist
    Write-Host "`n$($script:Report.verdict) - $script:RunRoot" -ForegroundColor Cyan
    Write-Host 'If you stopped early, power off/disconnect both radios when safe. No services or networking were changed by this guide.'
}
