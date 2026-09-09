#Requires -Version 5.1
<#
.SYNOPSIS
Today's attended demo checks, separate from blocked CHUD integration testing.
#>
[CmdletBinding()]
param(
    [string]$ArcRoot = "$env:USERPROFILE\Desktop\arc-uas-main-20260827",
    [string]$OutputRoot = "$env:USERPROFILE\Desktop\Radio Test Results\demo-readiness",
    [switch]$PreflightOnly,
    [switch]$KeepRunning,
    [switch]$LibraryOnly
)
$runReadiness = -not $LibraryOnly
$keepDemoRunning = $KeepRunning
$readinessScriptPath = $PSCommandPath
# Reuse the tested answer classifier, recorder, provenance, and restricted HTTP client.
# The permitted "EmulatorPort" is this guide's own AVIAN visualizer, not live CHUD.
. "$PSScriptRoot\Start-ArcEmulatorWalkthrough.ps1" -LibraryOnly -ArcRoot $ArcRoot -OutputRoot $OutputRoot `
    -BridgePort 19111 -CommandPort 19110 -UiPort 13001 -EmulatorPort 13221 -PreflightOnly:$PreflightOnly

function Test-DemoFleetResponse($Response) {
    if ($Response.status -ne 200) { return $false }
    $records = @($Response.body.records)
    if ($records.Count -ne 3) { return $false }
    $ids = @($records | ForEach-Object { $_.drone_id } | Select-Object -Unique)
    if ($ids.Count -ne 3 -or $ids -notcontains 'SIMULATION-ALPHA' -or $ids -notcontains 'SIMULATION-ALPHA-2' -or
        $ids -notcontains 'SIMULATION-ALPHA-unauthorized') { return $false }
    foreach ($record in $records) {
        if (@($record.endpoints).Count -eq 0) { return $false }
        foreach ($endpoint in $record.endpoints) {
            if (@($endpoint.capability_hints) -notcontains 'mock' -or $endpoint.host -ne '127.0.0.1') { return $false }
        }
        if ($record.drone_id -eq 'SIMULATION-ALPHA-unauthorized') {
            if ($record.authorization_status -ne 'unauthorized' -or $record.connection_status -ne 'discovered') { return $false }
        } elseif ($record.authorization_status -ne 'authorized' -or $record.connection_status -ne 'connected') { return $false }
    }
    return $true
}

function Test-ScaleTrace($Trace) {
    if ($Trace.schema_version -ne 1) { return $false }
    $initial = @($Trace.steps | Where-Object { $_.id -eq 'maximum-formation-online' })
    $lost = @($Trace.steps | Where-Object { $_.id -eq 'maximum-formation-rerouting' })
    $restored = @($Trace.steps | Where-Object { $_.id -eq 'maximum-formation-mission' })
    if ($initial.Count -ne 1 -or $lost.Count -ne 1 -or $restored.Count -ne 1) { return $false }
    $expectedIds = @($initial[0].nodes.id | Sort-Object)
    foreach ($step in @($initial[0],$lost[0],$restored[0])) {
        if (@($step.nodes).Count -ne 201 -or @($step.nodes.id | Select-Object -Unique).Count -ne 201 -or
            @($step.nodes | Where-Object { $_.role -eq 'aircraft' }).Count -ne 200 -or
            @($step.nodes | Where-Object { $_.role -eq 'ground' }).Count -ne 1 -or
            @($step.nodes | Where-Object { $_.status -notin @('online','degraded','offline') }).Count -ne 0 -or
            @($step.nodes | Where-Object { $_.status -in @('online','degraded') }).Count -ne $step.metrics.online_nodes -or
            ((@($step.nodes.id | Sort-Object) -join ',') -cne ($expectedIds -join ','))) { return $false }
    }
    return ($initial[0].metrics.online_nodes -eq 201 -and $lost[0].metrics.online_nodes -eq 180 -and
        $restored[0].metrics.online_nodes -eq 201 -and $restored[0].metrics.mission_synced_nodes -eq 201 -and
        $lost[0].metrics.active_links -lt $initial[0].metrics.active_links)
}

function Test-SimConfiguration($Response) {
    return ($Response.status -eq 200 -and $Response.body.simulated -eq $true -and
        $Response.body.hardware_write -eq $false -and $null -ne $Response.body.configuration)
}

function Same-SimConfiguration($Before, $After) {
    if (-not (Test-SimConfiguration $Before) -or -not (Test-SimConfiguration $After)) { return $false }
    foreach ($key in @('generation','network_id','band','center_frequency_mhz','bandwidth_mhz','transmit_power_dbm','routing_beacon_period_ms','encryption_required')) {
        if ($Before.body.configuration.$key -cne $After.body.configuration.$key) { return $false }
    }
    return $true
}

function Capture-Demo([string]$Name) {
    $capture = [ordered]@{}
    foreach ($entry in @(
        @{name='arc_health';url="http://127.0.0.1:$BridgePort/api/health"},
        @{name='arc_fleet';url="http://127.0.0.1:$UiPort/api/fleet"},
        @{name='arc_radio_control';url="http://127.0.0.1:$BridgePort/api/radio/networks"},
        @{name='avian_health';url="http://127.0.0.1:$EmulatorPort/api/health"},
        @{name='avian_configuration';url="http://127.0.0.1:$EmulatorPort/api/radio/configuration"}
    )) { $capture[$entry.name] = Invoke-LocalJson $entry.url }
    Save-Json (Join-Path $script:Run.directory "$Name.json") $capture
    return $capture
}

function Invoke-DemoReadiness {
    $output = Assert-ExternalOutput $OutputRoot @($script:AvianRoot,$ArcRoot)
    $id = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0,6)
    $directory = Join-Path $output $id
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $runtime = Join-Path $directory 'services'
    $script:Run = [ordered]@{
        schema_version=1; id=$id; directory=$directory; started_utc=[DateTime]::UtcNow.ToString('o'); updated_utc='';
        verdict='INCOMPLETE'; completed=$false; simulation_only=$true; hardware_validation=$false;
        scope='Separate ARC mock Fleet and AVIAN network demo readiness'; integration_status='BLOCKED - not tested by this demo guide';
        chud_assessment_date='2026-09-08'; steps=(New-Object System.Collections.ArrayList); repositories=@();
        limitations=@('A demo PASS is not an end-to-end ARC/CHUD integration PASS.',
            'Windows discovery/intake, authoritative promotion, duplicate-IP onboarding and TrellisWare mesh peers remain CHUD blockers.',
            'ARC has two mock Fleet devices. AVIAN separately executes an in-memory 200-aircraft plus one-GCS trace.',
            'Trace checks inspect recorded states, not independent radio processes, RF propagation or hardware.',
            'ARC and AVIAN are freshly built into a disposable session directory; build logs and source provenance are retained.',
            'Voice/AI assets and physical flight are outside this demo.');
        arc_url="http://127.0.0.1:$UiPort/home/devices"; avian_url="http://127.0.0.1:$EmulatorPort";
        runtime_directory=$runtime
    }
    Save-Report
    $started = $false
    try {
        Write-Host "`nTODAY'S DEMO CHECK - SIMULATION ONLY" -ForegroundColor Cyan
        Write-Host 'ARC Fleet and AVIAN networking are separate demos. Physical-radio onboarding remains blocked.'
        Write-Host "Answers: Y/N/U=unknown/S=skip/Q=quit. Evidence: $directory"
        if (-not $PreflightOnly -and -not (Ask 'isolation' 'Leave physical radios disconnected. No network, certificate or hardware settings will be changed.' 'Ready to test simulation only?')) { return }
        $script:Run.repositories = @((Get-Provenance $script:AvianRoot),(Get-Provenance $ArcRoot),(Get-Provenance (Join-Path $ArcRoot 'services\arc-ui')))
        $script:Run.script_sha256 = (Get-FileHash -LiteralPath $readinessScriptPath -Algorithm SHA256).Hash
        Save-Report
        # Use dedicated ports and a fresh process manifest, never someone else's running demo.
        & "$PSScriptRoot\Start-AvianArcDemo.ps1" -ArcRoot $ArcRoot -StateRoot $runtime -UiPort $UiPort `
            -BridgePort $BridgePort -TcpPort $CommandPort -VisualizerPort $EmulatorPort -GuidedSession
        $started = $true
        $manifest=Get-Content -LiteralPath (Join-Path $runtime 'active.json') -Raw | ConvertFrom-Json
        $script:Run.binary_sha256=(Get-FileHash -LiteralPath (Join-Path $manifest.buildRoot 'arc-target\debug\dev-bridge.exe') -Algorithm SHA256).Hash
        $baseline = Capture-Demo '00-baseline'
        if (-not (Test-DemoFleetResponse $baseline.arc_fleet)) { Add-Result 'mock-fleet' 'FAIL' 'Expected two connected authorized mocks plus one unauthorized discovery fixture, all with loopback mock endpoints.'; return }
        Add-Result 'mock-fleet' 'PASS' 'UI proxy reaches two connected authorized mocks and one unauthorized discovery fixture, all loopback and mock-marked.'
        if (-not (Test-SimConfiguration $baseline.avian_configuration)) { Add-Result 'simulation-provenance' 'FAIL' 'AVIAN simulation/no-hardware-write markers are missing.'; return }
        Add-Result 'simulation-provenance' 'PASS' 'AVIAN configuration endpoint reports simulated=true and hardware_write=false.'
        $scenario = Invoke-LocalJson "$($script:Run.avian_url)/api/scenario"
        Save-Json (Join-Path $directory 'scenario.json') $scenario
        if ($scenario.status -ne 200 -or -not (Test-ScaleTrace $scenario.body)) { Add-Result 'scale-trace' 'FAIL' 'Expected 201-node identities, loss/rerouting and recovery trace are missing or inconsistent.'; return }
        Add-Result 'scale-trace' 'PASS' 'Trace contains 200 aircraft plus one GCS; online count changes 201 -> 180 -> 201 with reduced active links during loss.'
        Write-Host 'Radio integration is not a pass/fail prerequisite for the separate demo; its actual API response is recorded in each checkpoint.' -ForegroundColor Yellow
        if ($PreflightOnly) { Add-Result 'operator-walkthrough' 'SKIPPED' 'Automated preflight only. Visual presentation and operator actions have not been tested.'; return }
        Write-Host "`nOpen these exact test URLs in your browser; existing demo tabs use different ports."
        Write-Host $script:Run.arc_url
        Write-Host $script:Run.avian_url
        if (Ask 'arc-page' 'Open the ARC test URL. Check Fleet for SIMULATION-ALPHA and SIMULATION-ALPHA-2. Do not configure physical radios.' 'Does Fleet load without Failed to fetch or permanent loading cards?') {
            [void](Ask 'arc-selection' 'Select each mock Fleet device in turn. Check that the active-device indication follows the selection. Do not send flight commands.' 'Do both mock devices select correctly?')
            [void](Ask 'arc-unauthorized' 'A third fixture named SIMULATION-ALPHA-unauthorized may appear in discovery. Do not authorize or pair it.' 'Is it kept separate from the two connected, ready Fleet devices rather than falsely shown as ready?')
        } else { Add-Result 'arc-selection' 'SKIPPED' 'ARC page readiness was not confirmed; dependent interaction was not attempted.' }
        [void](Capture-Demo '01-arc-fleet')
        $avianReady = Ask 'avian-page' 'Open the AVIAN test URL. Confirm dark presentation and a readable SIMULATION // Dev Environment label that does not cover the logo.' 'Does the network demo load clearly?'
        if ($avianReady) {
            [void](Ask 'discovery-sequence' 'Reset, then use STEP through radio connected, nodes discovered, snapshot, configuration and readback. These are modeled events, not hardware discovery.' 'Are the early steps understandable and are the relevant controls/highlights visible?')
            $before = Invoke-LocalJson "$($script:Run.avian_url)/api/radio/configuration"
            Save-Json (Join-Path $directory '02-before-config.json') $before
            $testName = 'DEMO-' + (Get-Date -Format 'HHmmss')
            if (Ask 'validate-plan' "Enter Network ID $testName, S-BAND, 2500 MHz, 10 MHz channel width, 18 dBm, 500 ms beacon, encryption checked. Click VALIDATE PLAN only." 'Does validation succeed without claiming a completed apply?') {
                $validated = Invoke-LocalJson "$($script:Run.avian_url)/api/radio/configuration"
                Save-Json (Join-Path $directory '03-after-validate.json') $validated
                if (Same-SimConfiguration $before $validated) { Add-Result 'validate-no-apply' 'PASS' 'Server configuration and generation remained unchanged.' }
                else { Add-Result 'validate-no-apply' 'FAIL' 'Configuration changed during validation-only action.' }
                if (Ask 'simulated-apply' 'Click APPLY THROUGH CHUD in this AVIAN simulator only. This button uses the local CHUD model, not the real CHUD service.' 'Does it report simulated apply and readback confirmation?') {
                    $after = Invoke-LocalJson "$($script:Run.avian_url)/api/radio/configuration"
                    Save-Json (Join-Path $directory '04-after-apply.json') $after
                    $config = $after.body.configuration
                    if ((Test-SimConfiguration $after) -and $config.network_id -ceq $testName -and $config.band -ceq 'S-BAND' -and
                        $config.center_frequency_mhz -eq 2500 -and $config.bandwidth_mhz -eq 10 -and $config.transmit_power_dbm -eq 18 -and
                        $config.routing_beacon_period_ms -eq 500 -and $config.encryption_required -eq $true -and $config.generation -gt $before.body.configuration.generation) {
                        Add-Result 'simulated-readback' 'PASS' 'Server readback matches every requested simulator setting and generation advanced.'
                    } else { Add-Result 'simulated-readback' 'FAIL' 'Server readback did not match the requested simulator settings.' }
                } else { Add-Result 'simulated-readback' 'SKIPPED' 'Apply success was not confirmed; dependent comparison was not asserted.' }
            } else { Add-Result 'simulated-apply' 'SKIPPED' 'Plan validation was not confirmed; dependent apply was not requested.' }
            $beforeInvalid = Invoke-LocalJson "$($script:Run.avian_url)/api/radio/configuration"
            if (Ask 'invalid-frequency' 'Keep S-BAND selected, enter 3000 MHz, and try VALIDATE PLAN/APPLY. It must reject the out-of-band value. Then restore the input to 2500 without applying.' 'Was the invalid frequency rejected?') {
                $afterInvalid = Invoke-LocalJson "$($script:Run.avian_url)/api/radio/configuration"
                Save-Json (Join-Path $directory '05-invalid-plan.json') @{before=$beforeInvalid;after=$afterInvalid}
                if (Same-SimConfiguration $beforeInvalid $afterInvalid) { Add-Result 'invalid-no-apply' 'PASS' 'Rejected plan did not change server configuration.' }
                else { Add-Result 'invalid-no-apply' 'FAIL' 'Server configuration changed during invalid-plan testing.' }
            }
            [void](Ask 'scale-visual' 'Select the final three timeline steps: 200-aircraft mesh online; 20 aircraft leave and paths reroute; 200-aircraft simulation completed. Pause and inspect each.' 'Does the view fit all nodes, show changed active paths during loss, then show recovery?')
            [void](Ask 'about-truth' 'Open ABOUT. Read what is simulated and what is not yet validated. Return to SIMULATION.' 'Does it clearly distinguish in-memory simulation from real CHUD, RF and hardware testing?')
        } else { Add-Result 'avian-interactions' 'SKIPPED' 'AVIAN page readiness was not confirmed; configuration and visual checks were not requested.' }
        [void](Capture-Demo '99-final')
        [void](Ask 'presentation-readiness' 'Review both test tabs at presentation resolution. Do not demonstrate unavailable voice/AI or claim the two demos are integrated.' 'Are labels, text, controls and pacing ready for the demo?')
        $script:Run.completed = $true
    } catch [OperationCanceledException] { Add-Result 'operator-stop' 'SKIPPED' $_.Exception.Message 'operator' }
    catch { Add-Result 'startup-or-capture' 'BLOCKED' $_.Exception.Message }
    finally {
        if (Test-Path -LiteralPath (Join-Path $runtime 'active.json')) {
            try { & "$PSScriptRoot\Stop-AvianArcDemo.ps1" -StateRoot $runtime }
            catch { Add-Result 'cleanup' 'BLOCKED' $_.Exception.Message }
        }
        Save-Report
        Write-Host "`nDemo verdict: $($script:Run.verdict). Integration/hardware: NOT VALIDATED." -ForegroundColor Cyan
        Write-Host "Results: $directory"
        if ($keepDemoRunning) { Write-Host 'KeepRunning is superseded by automatic post-test cleanup. Logs and results remain saved.' }
    }
}

if ($runReadiness) { Invoke-DemoReadiness }
