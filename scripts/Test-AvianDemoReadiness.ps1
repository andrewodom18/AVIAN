#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\Start-AvianDemoReadiness.ps1" -LibraryOnly
$script:passed = 0
function Check($Value, [bool]$Expected, [string]$Name) {
    if ($Value -ne $Expected) { throw "FAIL: $Name (got $Value)" }
    $script:passed++
}
$fleet = @{status=200;body=@{records=@(
    @{drone_id='SIMULATION-ALPHA';authorization_status='authorized';connection_status='connected';endpoints=@(@{host='127.0.0.1';capability_hints=@('mock','video')})},
    @{drone_id='SIMULATION-ALPHA-2';authorization_status='authorized';connection_status='connected';endpoints=@(@{host='127.0.0.1';capability_hints=@('mock','video')})},
    @{drone_id='SIMULATION-ALPHA-unauthorized';authorization_status='unauthorized';connection_status='discovered';endpoints=@(@{host='127.0.0.1';capability_hints=@('mock','video')})}
)}}
Check (Test-DemoFleetResponse $fleet) $true 'isolated mock fleet accepted'
$fleet.body.records[1].drone_id = 'LIVE-DRONE'
Check (Test-DemoFleetResponse $fleet) $false 'unowned fleet rejected'
$fleet.body.records[1].drone_id = 'SIMULATION-ALPHA-2'
$fleet.body.records[1].endpoints[0].capability_hints = @('video')
Check (Test-DemoFleetResponse $fleet) $false 'missing mock marker rejected'
$fleet.body.records[1].endpoints[0].capability_hints = @('mock','video')
$fleet.body.records[2].authorization_status = 'authorized'
Check (Test-DemoFleetResponse $fleet) $false 'unauthorized fixture cannot become ready'
$fleet.status = 503
Check (Test-DemoFleetResponse $fleet) $false 'unavailable fleet rejected'

function New-TraceFixture {
    $steps = foreach ($phase in @('maximum-formation-online','maximum-formation-rerouting','maximum-formation-mission')) {
        $loss = $phase -eq 'maximum-formation-rerouting'
        $nodes = @(1..200 | ForEach-Object { @{id="aircraft-$_";role='aircraft';status=$(if($loss -and $_ -le 20){'offline'}else{'online'})} })
        $nodes += @{id='gcs';role='ground';status=$(if($loss){'offline'}else{'online'})}
        @{id=$phase;nodes=$nodes;metrics=@{online_nodes=$(if($loss){180}else{201});active_links=$(if($loss){639}else{801});mission_synced_nodes=$(if($loss){180}else{201})}}
    }
    return @{schema_version=1;steps=@($steps)}
}
$trace = New-TraceFixture
Check (Test-ScaleTrace $trace) $true 'complete scale trace'
$trace.steps[1].nodes[20].status = 'degraded'
$trace.steps[1].nodes[21].status = 'degraded'
Check (Test-ScaleTrace $trace) $true 'degraded survivors remain online in trace metrics'
$trace.steps[1].nodes[20].status = 'unknown'
Check (Test-ScaleTrace $trace) $false 'unknown node state rejected'
$trace = New-TraceFixture
$trace.steps[1].metrics.online_nodes = 201
Check (Test-ScaleTrace $trace) $false 'loss metrics must match node state'
$trace = New-TraceFixture
$trace.steps[2].nodes[1].id = $trace.steps[2].nodes[0].id
Check (Test-ScaleTrace $trace) $false 'duplicate identities rejected'
$trace = New-TraceFixture
$trace.steps[2].nodes[1].id = 'different-aircraft'
Check (Test-ScaleTrace $trace) $false 'recovery cannot substitute identities'
$trace = New-TraceFixture
$trace.steps[2].nodes[200].role = 'unknown'
Check (Test-ScaleTrace $trace) $false 'ground station required'
$trace = New-TraceFixture
$trace.steps[1].metrics.active_links = 801
Check (Test-ScaleTrace $trace) $false 'loss must affect active links'
$trace = New-TraceFixture
$trace.steps[2].metrics.mission_synced_nodes = 199
Check (Test-ScaleTrace $trace) $false 'full recovery convergence required'
$trace = New-TraceFixture
$trace.steps = @($trace.steps[0],$trace.steps[1])
Check (Test-ScaleTrace $trace) $false 'missing recovery rejected'

$before = @{status=200;body=@{simulated=$true;hardware_write=$false;configuration=@{generation=1;network_id='TEST';band='S-BAND';center_frequency_mhz=2500;bandwidth_mhz=10;transmit_power_dbm=18;routing_beacon_period_ms=500;encryption_required=$true}}}
$after = $before | ConvertTo-Json -Depth 10 | ConvertFrom-Json
Check (Same-SimConfiguration $before $after) $true 'unchanged config'
$after.body.configuration.generation = 2
Check (Same-SimConfiguration $before $after) $false 'generation change detected'
$after.body.configuration.generation = 1
$after.body.configuration.center_frequency_mhz = 2440
Check (Same-SimConfiguration $before $after) $false 'field change detected without generation change'
$before.body.hardware_write = $true
Check (Test-SimConfiguration $before) $false 'hardware-writing endpoint rejected'
$before.body.hardware_write = $false
$before.body.simulated = $false
Check (Test-SimConfiguration $before) $false 'non-simulated endpoint rejected'

$dir = Join-Path ([IO.Path]::GetTempPath()) ('avian-readiness-selftest-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $dir | Out-Null
$script:Run = [ordered]@{id='unit';directory=$dir;updated_utc='';completed=$true;verdict='';integration_status='BLOCKED - not part of demo';steps=(New-Object System.Collections.ArrayList)}
Add-Result 'demo-check' 'PASS' 'Fixture'
Check ((Get-Content -LiteralPath (Join-Path $dir 'summary.txt') -Raw).Contains('Integration: BLOCKED')) $true 'demo pass retains integration limitation'
Check ($script:Run.verdict -eq 'PASS') $true 'separate demo verdict'
Write-Host "$script:passed readiness checks passed. Evidence: $dir"
