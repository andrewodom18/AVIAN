[CmdletBinding()]
param(
    [ValidateRange(1, 10)]
    [int]$RadioCount = 2,
    [string]$RadioIp = '10.1.0.2',
    [string]$EthernetAdapter = 'Ethernet 2',
    [string]$UiUrl = 'https://localhost:3000/home/devices',
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\chud-authority-validation'),
    [switch]$PreflightOnly,
    [switch]$AllowManualConfigurationTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$runId = Get-Date -Format 'yyyyMMdd-HHmmss'
$runRoot = Join-Path $ResultsRoot $runId
$checkpointPath = Join-Path $runRoot 'checkpoints.json'
$summaryPath = Join-Path $runRoot 'summary.txt'
$checkpoints = [System.Collections.Generic.List[object]]::new()

function Write-Section([string]$Message) {
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function Add-Checkpoint(
    [string]$Phase,
    [string]$Check,
    [ValidateSet('pass', 'fail', 'blocked', 'info')] [string]$Result,
    [string]$Detail
) {
    $checkpoints.Add([pscustomobject]@{
        timestamp = (Get-Date).ToString('o')
        phase = $Phase
        check = $Check
        result = $Result
        detail = $Detail
    })
    $checkpoints | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $checkpointPath
    $color = if ($Result -eq 'pass') { 'Green' } elseif ($Result -eq 'fail') { 'Red' } else { 'Yellow' }
    Write-Host "[$($Result.ToUpperInvariant())] $Check - $Detail" -ForegroundColor $color
}

function Read-OperatorResult([string]$Question) {
    while ($true) {
        $answer = (Read-Host "$Question [Y/N/U]").Trim()
        if ($answer -match '^(?i)y(es)?$') { return 'pass' }
        if ($answer -match '^(?i)n(o)?$') { return 'fail' }
        if ($answer -match '^(?i)u(nknown|navailable)?$') { return 'blocked' }
        Write-Host 'Enter Y, N, or U when the result cannot be observed.' -ForegroundColor Yellow
    }
}

function Confirm-Exact([string]$Question, [string]$Expected) {
    $answer = (Read-Host "$Question Type $Expected to continue").Trim()
    if (-not $answer.Equals($Expected, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Confirmation did not match '$Expected'."
    }
}

function Get-LocalJson([string]$Uri) {
    Invoke-RestMethod -Uri $Uri -TimeoutSec 5
}

function Save-Json([object]$Value, [string]$Path) {
    $Value | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $Path
}

function Test-TtlReply([string]$Address) {
    (& ping.exe -n 1 -w 900 $Address 2>&1 | Out-String) -match '(?i)TTL[= ]\d+'
}

function Get-LiveNeighborMac([string]$Address) {
    $neighbor = Get-NetNeighbor -AddressFamily IPv4 -IPAddress $Address -ErrorAction SilentlyContinue |
        Where-Object State -NotIn @('Unreachable', 'Incomplete') |
        Select-Object -First 1
    if ($neighbor) { return [string]$neighbor.LinkLayerAddress }
    return ''
}

function Normalize-Mac([string]$Value) {
    ($Value -replace '[^0-9A-Fa-f]', '').ToLowerInvariant()
}

function Get-LatestDiagnostic {
    $path = Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\live-discovery\latest-discovery.json'
    if (-not (Test-Path -LiteralPath $path)) { return @() }
    try { return @(Get-Content -Raw -LiteralPath $path | ConvertFrom-Json) } catch { return @() }
}

function Get-ChudDevices {
    $response = Get-LocalJson 'http://127.0.0.1:8443/api/radio/devices'
    return @($response.devices)
}

function Get-ArcMesh {
    Get-LocalJson 'http://127.0.0.1:9101/api/radio/mesh'
}

function Test-MeshContainsMac([object]$Mesh, [string]$Mac) {
    $compact = Normalize-Mac $Mac
    foreach ($node in @($Mesh.nodes)) {
        if ((Normalize-Mac ($node | ConvertTo-Json -Depth 15 -Compress)) -match $compact) {
            return $true
        }
    }
    return $false
}

function Test-ChudEligibility([object[]]$Devices, [string]$Mac) {
    $compact = Normalize-Mac $Mac
    $pairs = for ($index = 0; $index -lt $compact.Length; $index += 2) {
        [regex]::Escape($compact.Substring($index, 2))
    }
    $macPattern = $pairs -join '[:-]?'
    foreach ($device in $Devices) {
        $encoded = $device | ConvertTo-Json -Depth 15 -Compress
        if ($encoded -notmatch $macPattern) { continue }
        $stateProperty = $device.PSObject.Properties['state']
        $driverProperty = $device.PSObject.Properties['driver_available']
        $managementDriverProperty = $device.PSObject.Properties['management_driver_available']
        $state = if ($stateProperty) { [string]$stateProperty.Value } else { '' }
        $driver = ($driverProperty -and $driverProperty.Value -eq $true) -or
                  ($managementDriverProperty -and $managementDriverProperty.Value -eq $true)
        if ($driver -and $state -match '^(?i)(confirmed|connected|managed)$') { return $true }
    }
    return $false
}

function Ask-UiCheck([string]$Phase, [string]$Check, [string]$Question) {
    $result = Read-OperatorResult $Question
    if ($result -eq 'pass') {
        Add-Checkpoint $Phase $Check pass 'Operator confirmed expected behavior.'
    } elseif ($result -eq 'blocked') {
        $detail = (Read-Host 'Briefly describe why this could not be observed (optional)').Trim()
        Add-Checkpoint $Phase $Check blocked $(if ($detail) { $detail } else { 'Operator could not observe this result.' })
    } else {
        $detail = (Read-Host 'Briefly describe what appeared instead').Trim()
        Add-Checkpoint $Phase $Check fail $(if ($detail) { $detail } else { 'Operator reported unexpected behavior.' })
    }
}

function Write-Summary {
    $failures = @($checkpoints | Where-Object result -eq 'fail')
    $blocked = @($checkpoints | Where-Object result -eq 'blocked')
    $verdict = if ($failures.Count) { 'FAIL' } elseif ($blocked.Count) { 'PARTIAL' } else { 'PASS' }
    @(
        "CHUD authority validation: $runId",
        "Verdict: $verdict",
        "Evidence: $runRoot",
        '',
        "Passed: $(@($checkpoints | Where-Object result -eq 'pass').Count)",
        "Failed: $($failures.Count)",
        "Blocked: $($blocked.Count)",
        '',
        'No host networking, certificates, firmware, or radio settings were changed automatically.'
    ) | Set-Content -LiteralPath $summaryPath
    Get-Content -LiteralPath $summaryPath | Write-Host
}

New-Item -ItemType Directory -Force -Path $runRoot | Out-Null

try {
    Write-Section 'Application preflight'
    $simulators = @(docker ps -a --format '{{.Names}}|{{.Command}}' | Select-String 'simulate-radio|local-sim|arc-avian-local-radio-plugin')
    if ($simulators.Count) { throw "Simulator containers are present: $($simulators -join ', ')" }
    Add-Checkpoint preflight 'Simulator exclusion' pass 'No simulated radio or local-sim container is present.'

    $requiredContainers = @('arc-avian-local-comms-1', 'arc-avian-local-dev-bridge-1', 'arc-avian-real-link-manager', 'chud-local')
    foreach ($container in $requiredContainers) {
        if ((docker inspect --format '{{.State.Status}}' $container) -ne 'running') { throw "$container is not running." }
    }
    Add-Checkpoint preflight 'Required services' pass 'ARC comms, dev-bridge, link manager, and CHUD are running.'

    & curl.exe --silent --fail --insecure $UiUrl *> $null
    if ($LASTEXITCODE -ne 0) { throw "ARC UI is not reachable at $UiUrl" }
    $chudBaseline = @(Get-ChudDevices)
    $fleetBaseline = Get-LocalJson 'http://127.0.0.1:9101/api/fleet'
    $meshBaseline = Get-ArcMesh
    $radioHealth = Get-LocalJson 'http://127.0.0.1:9101/api/radio/streamcaster/health'
    Save-Json $chudBaseline (Join-Path $runRoot 'baseline-chud.json')
    Save-Json $fleetBaseline (Join-Path $runRoot 'baseline-fleet.json')
    Save-Json $meshBaseline (Join-Path $runRoot 'baseline-mesh.json')
    Save-Json $radioHealth (Join-Path $runRoot 'baseline-radio-health.json')
    Add-Checkpoint preflight 'Local APIs' pass "UI, CHUD, Fleet, and radio-mesh APIs responded. CHUD devices=$($chudBaseline.Count)."
    if (-not $radioHealth.coordination_reachable) {
        throw 'ARC radio coordination is not reachable through dev-bridge. Restart the bench stack before connecting hardware.'
    }
    Add-Checkpoint preflight 'ARC radio transport' pass 'Dev-bridge can query the ARC link-manager coordination path.'

    $adapter = Get-NetAdapter -Name $EthernetAdapter -ErrorAction Stop
    $radioAddress = Get-NetIPAddress -InterfaceAlias $EthernetAdapter -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object IPAddress -Like '10.1.0.*' | Select-Object -First 1
    if (-not $radioAddress) { throw "$EthernetAdapter has no existing 10.1.0.x address. This suite will not alter networking." }
    Add-Checkpoint preflight 'Radio Ethernet' pass "$EthernetAdapter already has $($radioAddress.IPAddress)/$($radioAddress.PrefixLength)."
    if ($PreflightOnly) {
        Add-Checkpoint preflight 'Hardware contact' info 'Preflight-only mode did not request a radio connection.'
        Write-Summary
        return
    }

    Write-Section 'Disconnected baseline'
    Write-Host 'Disconnect and power off both radios. The apps remain running.' -ForegroundColor Yellow
    Confirm-Exact 'Confirm both radios are disconnected.' 'DISCONNECTED'
    Start-Sleep -Seconds 12
    $disconnectedChud = @(Get-ChudDevices)
    $disconnectedMesh = Get-ArcMesh
    Save-Json $disconnectedChud (Join-Path $runRoot 'disconnected-chud.json')
    Save-Json $disconnectedMesh (Join-Path $runRoot 'disconnected-mesh.json')
    Ask-UiCheck disconnected 'No live radio' 'Does RADIO NETWORK show zero online radios after Refresh Live Data?'
    Ask-UiCheck disconnected 'Fleet persistence truth' 'Are saved Fleet drones either absent or clearly disconnected rather than shown as live?'

    $learnedMacs = [System.Collections.Generic.List[string]]::new()
    for ($sequence = 1; $sequence -le $RadioCount; $sequence++) {
        $phase = "radio-$sequence"
        $radioRoot = Join-Path $runRoot $phase
        New-Item -ItemType Directory -Force -Path $radioRoot | Out-Null
        Write-Section "Radio $sequence discovery and authority"
        Write-Host "Power and Ethernet-connect only Radio $sequence. Keep every other factory-address radio off." -ForegroundColor Yellow
        Read-Host 'Press Enter after the radio has been powered for at least 30 seconds' | Out-Null

        $deadline = (Get-Date).AddSeconds(60)
        $mac = ''
        do {
            $ping = Test-TtlReply $RadioIp
            $mac = Get-LiveNeighborMac $RadioIp
            Start-Sleep -Seconds 2
        } until (($ping -and $mac) -or (Get-Date) -ge $deadline)
        if (-not $ping -or -not $mac) { throw "Radio $sequence did not satisfy ping plus MAC discovery." }
        $normalizedMac = Normalize-Mac $mac
        $learnedMacs.Add($normalizedMac)
        Add-Checkpoint $phase 'Stable physical identity' pass "$RadioIp resolved to MAC $mac."

        $diagnosticDeadline = (Get-Date).AddSeconds(20)
        do {
            $diagnostics = Get-LatestDiagnostic
            $matchingDiagnostic = $diagnostics | Where-Object { (Normalize-Mac ([string]$_.mac_address)) -eq $normalizedMac } | Select-Object -First 1
            if (-not $matchingDiagnostic) { Start-Sleep -Seconds 2 }
        } until ($matchingDiagnostic -or (Get-Date) -ge $diagnosticDeadline)
        Save-Json $diagnostics (Join-Path $radioRoot 'avian-diagnostic.json')
        if (-not $matchingDiagnostic) {
            Add-Checkpoint $phase 'AVIAN diagnostic observation' fail 'The watcher did not publish the learned MAC.'
        } else {
            $schemaProperty = $matchingDiagnostic.PSObject.Properties['schema_version']
            $authorityProperty = $matchingDiagnostic.PSObject.Properties['source_authority']
            $driverProperty = $matchingDiagnostic.PSObject.Properties['management_driver_available']
            $diagnosticSafe = $schemaProperty -and $schemaProperty.Value -eq 2 -and
                $authorityProperty -and $authorityProperty.Value -eq 'avian_diagnostic' -and
                $driverProperty -and $driverProperty.Value -eq $false
        }
        if ($matchingDiagnostic -and $diagnosticSafe) {
            Add-Checkpoint $phase 'AVIAN diagnostic observation' pass 'Schema v2 correctly reports diagnostic, non-authoritative provenance.'
        } elseif ($matchingDiagnostic) {
            Add-Checkpoint $phase 'AVIAN diagnostic observation' fail 'The observation did not carry the expected v2 diagnostic safety metadata.'
        }

        $chud = @(Get-ChudDevices)
        $meshDeadline = (Get-Date).AddSeconds(20)
        do {
            $mesh = Get-ArcMesh
            if (-not (Test-MeshContainsMac $mesh $mac)) { Start-Sleep -Seconds 2 }
        } until ((Test-MeshContainsMac $mesh $mac) -or (Get-Date) -ge $meshDeadline)
        Save-Json $chud (Join-Path $radioRoot 'chud-devices.json')
        Save-Json $mesh (Join-Path $radioRoot 'arc-radio-mesh.json')
        $arcTransported = Test-MeshContainsMac $mesh $mac
        if ($arcTransported) {
            Add-Checkpoint $phase 'ARC discovery transport' pass 'The MAC-identifiable AVIAN diagnostic reached the ARC radio-mesh API.'
        } else {
            Add-Checkpoint $phase 'ARC discovery transport' fail 'AVIAN discovered the radio, but ARC radio-mesh did not receive the MAC within 20 seconds.'
        }
        $eligible = Test-ChudEligibility $chud $mac
        if ($eligible) {
            Add-Checkpoint $phase 'CHUD configuration authority' pass 'CHUD reports the MAC confirmed/connected with an available driver.'
        } else {
            Add-Checkpoint $phase 'CHUD configuration authority' blocked 'CHUD has not supplied a confirmed/connected device with an available driver.'
        }

        Write-Host "Open $UiUrl in your existing browser workspace." -ForegroundColor Cyan
        if ($arcTransported) {
            Ask-UiCheck $phase 'Real MAC in Radio Network' "Does RADIO NETWORK show Radio $sequence as $mac without a simulated label?"
        } else {
            Add-Checkpoint $phase 'Real MAC in Radio Network' blocked 'UI validation was skipped because the ARC discovery transport did not contain this MAC.'
        }
        $fleetAfterDiscovery = Get-LocalJson 'http://127.0.0.1:9101/api/fleet'
        Save-Json $fleetAfterDiscovery (Join-Path $radioRoot 'fleet-after-discovery.json')
        if (@($fleetAfterDiscovery.records).Count -eq @($fleetBaseline.records).Count) {
            Add-Checkpoint $phase 'No automatic Fleet drone' pass 'Radio discovery did not create a Fleet drone record.'
        } else {
            Add-Checkpoint $phase 'No automatic Fleet drone' fail 'Fleet record count changed during radio-only discovery.'
        }
        if ($eligible) {
            Ask-UiCheck $phase 'CHUD-enabled control' 'Is configuration available only through the CHUD-backed workflow?'
        } else {
            Ask-UiCheck $phase 'Fail-closed control' 'Is Configure Selected Radio disabled or explicitly blocked by CHUD state?'
        }

        if ($AllowManualConfigurationTest -and $eligible) {
            Write-Section "Optional manual CHUD transaction for Radio $sequence"
            Write-Host 'Use only an approved reversible value. Confirm the baseline snapshot, apply through CHUD, verify readback, then restore the baseline.' -ForegroundColor Yellow
            Confirm-Exact 'Confirm recovery access, approved antennas/loads, and the recorded baseline snapshot.' 'LIVE WRITE READY'
            Ask-UiCheck $phase 'CHUD apply result' 'Did the CHUD transaction complete without ARC contacting the radio directly?'
            Ask-UiCheck $phase 'Deferred readback' 'Did CHUD readback exactly match the requested value?'
            Ask-UiCheck $phase 'Baseline restoration' 'Was the original baseline restored and verified?'
        } elseif ($AllowManualConfigurationTest) {
            Add-Checkpoint $phase 'Manual configuration test' blocked 'Not offered because the CHUD authority gate was not satisfied.'
        }

        Write-Host "Power off and disconnect Radio $sequence." -ForegroundColor Yellow
        Confirm-Exact "Confirm Radio $sequence is disconnected." 'DISCONNECTED'
        Start-Sleep -Seconds 12
        $afterDiagnostics = Get-LatestDiagnostic
        if (@($afterDiagnostics | Where-Object { (Normalize-Mac ([string]$_.mac_address)) -eq $normalizedMac }).Count -eq 0) {
            Add-Checkpoint $phase 'Diagnostic expiry' pass 'The disconnected MAC left the live AVIAN diagnostic inventory.'
        } else {
            Add-Checkpoint $phase 'Diagnostic expiry' fail 'The disconnected MAC remained in the live diagnostic inventory after 12 seconds.'
        }
        Ask-UiCheck $phase 'Disconnect truthfulness' 'Did the radio stop showing as online without deleting a separately saved Fleet drone?'
    }

    if (@($learnedMacs | Select-Object -Unique).Count -eq $RadioCount) {
        Add-Checkpoint identity 'Same-IP identity separation' pass "$RadioCount distinct MACs were learned sequentially at $RadioIp."
    } else {
        Add-Checkpoint identity 'Same-IP identity separation' fail 'The sequential runs did not produce distinct MAC identities.'
    }

    Write-Section 'Two-radio topology truth check'
    Write-Host 'Power both radios with approved antennas/loads. Ethernet-connect only one radio.' -ForegroundColor Yellow
    Confirm-Exact 'Confirm both radios are powered and only one has Ethernet.' 'MESH READY'
    Start-Sleep -Seconds 15
    $meshEvidence = Get-ArcMesh
    Save-Json $meshEvidence (Join-Path $runRoot 'two-radio-mesh.json')
    Ask-UiCheck mesh 'No invented RF edge' 'If CHUD supplies no RF-peer evidence, does ARC avoid drawing a measured solid RF link?'
    Ask-UiCheck mesh 'Logical overlay separation' 'Are any PEAT/logical paths visibly distinct from measured RF links?'

    Write-Host 'Power off and disconnect both radios.' -ForegroundColor Yellow
    Confirm-Exact 'Confirm both radios are powered off and disconnected.' 'DISCONNECTED'
    Add-Checkpoint cleanup 'Bench shutdown' pass 'Operator confirmed both radios were powered off and disconnected.'

    Write-Summary
} catch {
    Add-Checkpoint walkthrough 'Unexpected stop' fail $_.Exception.Message
    Write-Summary
    throw
} finally {
    Write-Host "`nEvidence directory: $runRoot" -ForegroundColor Green
}
