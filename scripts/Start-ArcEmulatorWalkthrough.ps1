#Requires -Version 5.1
<#
.SYNOPSIS
Attended ARC/AVIAN simulator walkthrough and checkpoint recorder.
.DESCRIPTION
Starts its own loopback emulator and mock Fleet bridge on dedicated ports.
Never attaches to an existing service. Stops with BLOCKED if mock radio control
is unavailable. Results are observational evidence, not an automated UI gate.
#>
[CmdletBinding()]
param(
    [string]$ArcRoot = (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'Desktop\arc-uas-main-20260827'),
    [string]$OutputRoot = (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'Desktop\Radio Test Results\arc-emulator'),
    [ValidateRange(1024,65535)][int]$BridgePort = 19101,
    [ValidateRange(1024,65535)][int]$CommandPort = 19100,
    [ValidateRange(1024,65535)][int]$EmulatorPort = 13212,
    [ValidateRange(1024,65535)][int]$UiPort = 13000,
    [switch]$PreflightOnly,
    [switch]$LibraryOnly
)

$ErrorActionPreference = 'Stop'
$script:Run = $null
$script:Owned = New-Object System.Collections.ArrayList
$script:TestToken = ''
$script:AvianRoot = Split-Path -Parent $PSScriptRoot

function Protect-Text([string]$Text) {
    if ($script:TestToken) { $Text = $Text.Replace($script:TestToken, '[REDACTED]') }
    return $Text
}
function Save-Json([string]$Path, $Value) {
    $json = Protect-Text (ConvertTo-Json -InputObject $Value -Depth 60)
    $temporary = $Path + '.pending'
    [IO.File]::WriteAllText($temporary, $json, (New-Object Text.UTF8Encoding($false)))
    # Windows PowerShell 5.1 coerces a null string argument to an empty path.
    # A named backup also preserves the previous complete checkpoint.
    if (Test-Path -LiteralPath $Path) { [IO.File]::Replace($temporary, $Path, ($Path + '.previous')) }
    else { [IO.File]::Move($temporary, $Path) }
}
function Get-Verdict($Steps, [bool]$Completed) {
    $states = @($Steps | ForEach-Object { $_.status })
    if ($states -contains 'FAIL') { return 'FAIL' }
    if ($states -contains 'BLOCKED') { return 'BLOCKED' }
    if (-not $Completed -or $states -contains 'SKIPPED' -or $states.Count -eq 0) { return 'INCOMPLETE' }
    return 'PASS'
}
function Save-Report {
    $script:Run.updated_utc = [DateTime]::UtcNow.ToString('o')
    $script:Run.verdict = Get-Verdict $script:Run.steps $script:Run.completed
    Save-Json (Join-Path $script:Run.directory 'report.json') $script:Run
    $script:Run.steps | Export-Csv -NoTypeInformation -Encoding UTF8 -LiteralPath (Join-Path $script:Run.directory 'steps.csv')
    $lines = @('ARC / AVIAN SIMULATION WALKTHROUGH', ('Run: ' + $script:Run.id), ('Verdict: ' + $script:Run.verdict),
        'Scope: attended process/API and UI observations; no physical radio or RF validation.', '')
    foreach ($step in $script:Run.steps) { $lines += '[{0}] {1}: {2}' -f $step.status,$step.id,$step.detail }
    [IO.File]::WriteAllLines((Join-Path $script:Run.directory 'summary.txt'), $lines, (New-Object Text.UTF8Encoding($false)))
}
function Add-Result([string]$Id, [ValidateSet('PASS','FAIL','BLOCKED','SKIPPED')][string]$Status, [string]$Detail, [string]$Source = 'automatic') {
    $record = [pscustomobject]@{ id=$Id; status=$Status; detail=(Protect-Text $Detail); source=$Source; utc=[DateTime]::UtcNow.ToString('o') }
    [void]$script:Run.steps.Add($record)
    Save-Report
    $color = @{PASS='Green';FAIL='Red';BLOCKED='Yellow';SKIPPED='DarkYellow'}[$Status]
    Write-Host ('[{0}] {1} - {2}' -f $Status,$Id,$record.detail) -ForegroundColor $color
}
function Convert-Answer([string]$Answer) {
    switch ($Answer.Trim().ToLowerInvariant()) {
        { $_ -in @('y','yes') } { return 'PASS' }
        { $_ -in @('n','no') } { return 'FAIL' }
        { $_ -in @('u','unknown') } { return 'BLOCKED' }
        { $_ -in @('s','skip') } { return 'SKIPPED' }
        { $_ -in @('q','quit') } { return 'QUIT' }
        default { return $null }
    }
}
function Ask([string]$Id, [string]$Instruction, [string]$Question) {
    Write-Host "`n$Instruction" -ForegroundColor Cyan
    do { $answer = Convert-Answer (Read-Host "$Question [Y/N/U=unknown/S=skip/Q=quit]") } while (-not $answer)
    if ($answer -eq 'QUIT') { throw [OperationCanceledException]::new('Operator ended the walkthrough. Saved checkpoints remain available.') }
    $note = if ($answer -eq 'PASS') { 'Operator confirmed expected behavior.' } else { Read-Host 'What happened, or why could this not be checked? (Do not enter credentials)' }
    Add-Result $Id $answer $note 'operator'
    return ($answer -eq 'PASS')
}
function Assert-ExternalOutput([string]$Output, [string[]]$Repositories) {
    $full = [IO.Path]::GetFullPath($Output).TrimEnd('\','/')
    foreach ($repository in $Repositories) {
        $repo = [IO.Path]::GetFullPath($repository).TrimEnd('\','/')
        if ($full.Equals($repo,[StringComparison]::OrdinalIgnoreCase) -or $full.StartsWith($repo + '\',[StringComparison]::OrdinalIgnoreCase)) {
            throw 'Evidence output must be outside the source repositories.'
        }
    }
    return $full
}
function Get-Provenance([string]$Path) {
    $commit = & git -C $Path rev-parse HEAD 2>$null
    if ($LASTEXITCODE -ne 0) { throw "Not a Git checkout: $Path" }
    $branch = & git -C $Path branch --show-current
    $status = @(& git -C $Path status --porcelain)
    return @{ path=$Path; commit=($commit -join ''); branch=($branch -join ''); status=$status }
}
function Invoke-LocalJson([string]$Url, [string]$Method = 'GET', $Body = $null, [hashtable]$Headers = @{}) {
    $uri = [Uri]$Url
    if ($uri.Scheme -ne 'http' -or $uri.Host -ne '127.0.0.1' -or $uri.Port -notin @($BridgePort,$EmulatorPort,$UiPort)) {
        throw 'The recorder permits only its configured loopback HTTP ports.'
    }
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object Net.Http.HttpClientHandler
    $handler.AllowAutoRedirect = $false
    $handler.UseProxy = $false
    $client = New-Object Net.Http.HttpClient($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(8)
    $request = New-Object Net.Http.HttpRequestMessage((New-Object Net.Http.HttpMethod($Method)), $uri)
    try {
        foreach ($key in $Headers.Keys) { [void]$request.Headers.TryAddWithoutValidation($key,[string]$Headers[$key]) }
        if ($null -ne $Body) { $request.Content = New-Object Net.Http.StringContent((ConvertTo-Json $Body -Depth 40 -Compress),[Text.Encoding]::UTF8,'application/json') }
        $response = $client.SendAsync($request).GetAwaiter().GetResult()
        try {
            $raw = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            $parsed = $null
            try { $parsed = $raw | ConvertFrom-Json } catch { }
            return [pscustomobject]@{ status=[int]$response.StatusCode; body=$parsed; text=(Protect-Text $raw); url=$Url; utc=[DateTime]::UtcNow.ToString('o') }
        } finally { $response.Dispose() }
    } finally { $request.Dispose(); $client.Dispose(); $handler.Dispose() }
}
function Start-Owned([string]$Name, [string]$Exe, [string[]]$Arguments, [string]$WorkingDirectory, [hashtable]$Environment) {
    $previous = @{}
    # Do not inherit production management credentials, discovery settings, or UI overrides.
    $keys = @('RADIO_MANAGEMENT_API_KEY_FILE','RADIO_MANAGEMENT_API_TOKEN','ARC_DEV_BRIDGE_TEST_CONTEXT_PATH','VITE_ARC_WS_URL','ZENOH_CONFIG') + @($Environment.Keys)
    try {
        foreach ($key in ($keys | Select-Object -Unique)) {
            $previous[$key] = [Environment]::GetEnvironmentVariable($key,'Process')
            [Environment]::SetEnvironmentVariable($key,$null,'Process')
        }
        foreach ($key in $Environment.Keys) { [Environment]::SetEnvironmentVariable($key,[string]$Environment[$key],'Process') }
        $process = Start-Process -FilePath $Exe -ArgumentList $Arguments -WorkingDirectory $WorkingDirectory -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $script:Run.directory "$Name.stdout.log") -RedirectStandardError (Join-Path $script:Run.directory "$Name.stderr.log")
        [void]$script:Owned.Add([pscustomobject]@{ name=$Name; process=$process; started=$process.StartTime.ToUniversalTime().Ticks })
        return $process
    } finally {
        foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key,$previous[$key],'Process') }
    }
}
function Stop-Owned([string]$Name = '') {
    foreach ($entry in @($script:Owned)) {
        if ($Name -and $entry.name -ne $Name) { continue }
        try {
            $current = Get-Process -Id $entry.process.Id -ErrorAction Stop
            if ($current.StartTime.ToUniversalTime().Ticks -eq $entry.started) { Stop-Process -Id $current.Id -ErrorAction Stop; $current.WaitForExit(5000) | Out-Null }
        } catch { if (-not $entry.process.HasExited) { throw } }
    }
}
function Wait-Endpoint([string]$Url, [hashtable]$Headers = @{}) {
    $until = [DateTime]::UtcNow.AddSeconds(45)
    do {
        try { $r = Invoke-LocalJson $Url 'GET' $null $Headers; if ($r.status -eq 200) { return $r } } catch { }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $until)
    throw "Endpoint did not become ready within 45 seconds: $Url"
}
function Get-Ledger {
    $r = Invoke-LocalJson "$script:EmulatorUrl/__sim/ledger" 'GET' $null $script:Auth
    if ($r.status -ne 200 -or $r.body.simulated -ne $true -or $r.body.hardware_write -ne $false -or $null -eq $r.body.events) {
        throw 'Missing or invalid simulator ledger; no inference of zero writes is permitted.'
    }
    return $r.body
}
function Assert-LedgerUnchanged($Before,$After,[string]$Id) {
    if ($Before.digest -eq $After.digest -and @($Before.events).Count -eq @($After.events).Count) {
        Add-Result $Id 'PASS' 'Emulator mutation ledger unchanged.'
    } else { Add-Result $Id 'FAIL' 'Emulator mutation ledger changed during an action expected to perform no apply/confirm.' }
}
function Checkpoint([string]$Name) {
    $capture = [ordered]@{}
    foreach ($path in @('/api/health','/api/fleet','/api/radio/mesh','/api/radio/networks','/api/fleet/radio-bindings')) {
        try { $capture[$path] = Invoke-LocalJson ($script:BridgeUrl+$path) } catch { $capture[$path] = @{error=(Protect-Text $_.Exception.Message)} }
    }
    $capture['ledger'] = Get-Ledger
    $capture['devices'] = Invoke-LocalJson "$script:EmulatorUrl/api/radio/devices" 'GET' $null $script:Auth
    $capture['operations'] = Invoke-LocalJson "$script:EmulatorUrl/api/radio/operations" 'GET' $null $script:Auth
    $capture['snapshots'] = @()
    foreach ($device in @($capture['devices'].body.devices)) {
        $encodedMac = [Uri]::EscapeDataString([string]$device.mac)
        $capture['snapshots'] += Invoke-LocalJson "$script:EmulatorUrl/api/radio/snapshot?mac=$encodedMac" 'GET' $null $script:Auth
    }
    Save-Json (Join-Path $script:Run.directory ($Name+'.json')) $capture
    return $capture
}
function Set-Fault($Fault) {
    $r = Invoke-LocalJson "$script:EmulatorUrl/__sim/control" 'POST' @{fault=$Fault} $script:Auth
    if ($r.status -ne 200 -or $r.body.simulated -ne $true -or $r.body.hardware_write -ne $false) { throw 'Simulator fault control rejected; stopping.' }
}
function Start-Bridge {
    $script:BridgeProcess = Start-Owned 'bridge' $script:BridgeExe @('--mock','--device-id','SIMULATION-GUIDE','--port',"$CommandPort",'--http-port',"$BridgePort",'--tcp-bind','127.0.0.1','--http-bind','127.0.0.1','--radio-management-api-url',$script:EmulatorUrl) $ArcRoot @{
        ARC_DEV_BRIDGE_STATE_DIR=(Join-Path $script:Run.directory 'state'); RADIO_MANAGEMENT_API_TOKEN=$script:TestToken;
        ARC_RADIO_MUTATIONS_ENABLED='true'; ARC_UI_ALLOWED_ORIGINS=$script:UiUrl; RUST_LOG='warn'
    }
    [void](Wait-Endpoint "$script:BridgeUrl/api/health")
}
function Invoke-Walkthrough {
    $uiRoot = Join-Path $ArcRoot 'services\arc-ui'
    $output = Assert-ExternalOutput $OutputRoot @($script:AvianRoot,$ArcRoot,$uiRoot)
    $id = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,6)
    $directory = Join-Path $output $id
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $script:Run = [ordered]@{schema_version=1;id=$id;directory=$directory;started_utc=[DateTime]::UtcNow.ToString('o');updated_utc='';verdict='INCOMPLETE';completed=$false;simulation_only=$true;hardware_validation=$false;steps=(New-Object System.Collections.ArrayList);repositories=@();processes=@();limitations=@('Attended UI evidence does not replace automated Playwright coverage.','HTTP 504 injection is not a real transport timeout.','Reboot flag injection does not simulate a full device reboot.','The current emulator has global faults and no plug/unplug control.','Current ARC mock Fleet mode lacks radio-control initialization; preflight should report BLOCKED until that prerequisite is implemented.')}
    $script:BridgeUrl = "http://127.0.0.1:$BridgePort"
    $script:EmulatorUrl = "http://127.0.0.1:$EmulatorPort"
    $script:UiUrl = "http://127.0.0.1:$UiPort"
    $script:TestToken = [Guid]::NewGuid().ToString('N')
    $script:Auth = @{Authorization="Bearer $script:TestToken"}
    Save-Report
    try {
        Write-Host "`nSIMULATION // ARC + AVIAN guided validation" -ForegroundColor Cyan
        Write-Host "Evidence: $directory"
        Write-Host 'Y=yes, N=no, U=unknown, S=skip, Q=quit. Answers are saved after every prompt.'
        if (-not $PreflightOnly) {
            if (-not (Ask 'isolation' 'Leave physical radios disconnected. This run uses fresh simulated state and dedicated local ports.' 'Are the physical radios disconnected?')) { Add-Result 'preflight' 'BLOCKED' 'Isolation was not confirmed.'; return }
        }
        $script:Run.repositories = @((Get-Provenance $script:AvianRoot),(Get-Provenance $ArcRoot),(Get-Provenance $uiRoot))
        Save-Report
        $ports = @($BridgePort,$CommandPort,$EmulatorPort,$UiPort)
        if (@($ports | Select-Object -Unique).Count -ne 4) { throw 'All four ports must be distinct.' }
        foreach ($port in $ports) {
            if (Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue) { throw "Port $port is already occupied. The guide will not stop or reuse that service." }
        }
        $node = (Get-Command node -ErrorAction Stop).Source
        $script:BridgeExe = Join-Path $ArcRoot 'services\dev-bridge\target\debug\dev-bridge.exe'
        $emulator = Join-Path $script:AvianRoot 'simulators\mesh-operations\chud-emulator\server.mjs'
        $vite = Join-Path $uiRoot 'node_modules\vite\bin\vite.js'
        foreach ($file in @($script:BridgeExe,$emulator,$vite)) { if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Required local dependency is missing: $file" } }
        $script:Run.binary_sha256 = (Get-FileHash -LiteralPath $script:BridgeExe -Algorithm SHA256).Hash
        $script:Run.script_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
        [void](Start-Owned 'emulator' $node @(('"'+$emulator+'"')) $script:AvianRoot @{AVIAN_CHUD_EMULATOR_PORT="$EmulatorPort";AVIAN_CHUD_EMULATOR_TOKEN=$script:TestToken})
        $devices = Wait-Endpoint "$script:EmulatorUrl/api/radio/devices" $script:Auth
        if ($devices.body.simulated -ne $true -or $devices.body.hardware_write -ne $false) { throw 'Emulator provenance check failed.' }
        Add-Result 'emulator' 'PASS' 'Authenticated local emulator reports simulated=true and hardware_write=false.'
        Start-Bridge
        $script:Run.processes = @($script:Owned | ForEach-Object { @{name=$_.name;pid=$_.process.Id;started_ticks=$_.started} })
        $networks = Invoke-LocalJson "$script:BridgeUrl/api/radio/networks"
        Save-Json (Join-Path $directory 'radio-control-preflight.json') $networks
        if ($networks.status -ne 200 -or $null -eq $networks.body.networks) {
            Save-Json (Join-Path $directory 'preflight-emulator-ledger.json') (Get-Ledger)
            Add-Result 'ARC-radio-control' 'BLOCKED' 'ARC mock Fleet mode has no usable radio-control API. Its run_mock path must initialize an isolated CHUD client before this walkthrough can test onboarding. No fallback to live discovery was attempted.'
            return
        }
        $baseline = Checkpoint '00-baseline'
        if (@($baseline.ledger.events).Count -ne 0) { Add-Result 'empty-ledger' 'FAIL' 'Unexpected mutation before operator action.'; return }
        $identities = @($devices.body.devices.mac | Select-Object -Unique)
        $addresses = @($devices.body.devices.reach_addr | Select-Object -Unique)
        if ($identities.Count -ne 2 -or $addresses.Count -ne 1) { Add-Result 'same-ip-fixture' 'BLOCKED' 'Expected two distinct MACs sharing one simulated management IP.'; return }
        Add-Result 'same-ip-fixture' 'PASS' 'Emulator reports two distinct MACs sharing one management IP.'
        foreach ($rawPath in @('/api/radio/management/apply','/api/radio/management/confirm')) {
            $rawResponse = Invoke-LocalJson ($script:BridgeUrl+$rawPath) 'POST' @{} @{'x-arc-radio-control'='explicit-operator-action'}
            Save-Json (Join-Path $directory (('raw-route-'+($rawPath.Split('/')[-1]))+'.json')) $rawResponse
            if ($rawResponse.status -eq 404) { Add-Result $rawPath 'PASS' 'Raw mutation route is absent.' }
            else { Add-Result $rawPath 'FAIL' 'Former raw mutation route did not return 404.'; return }
        }
        Assert-LedgerUnchanged $baseline.ledger (Get-Ledger) 'raw-routes-no-write'
        Add-Result 'ARC-radio-control' 'PASS' 'Mock Fleet bridge exposes the radio network API.'
        if ($PreflightOnly) { Add-Result 'walkthrough' 'SKIPPED' 'Preflight-only run; interactive workflow not tested.'; return }
        [void](Start-Owned 'frontend' $node @(('"'+$vite+'"'),'--host','127.0.0.1','--port',"$UiPort",'--strictPort') $uiRoot @{VITE_DEV_HTTP='1';VITE_BRIDGE_URL=$script:BridgeUrl;VITE_ARC_WS_URL="ws://127.0.0.1:$BridgePort/ws";VITE_DEMO_MODE='1';BROWSER='none'})
        [void](Wait-Endpoint $script:UiUrl)
        $proxy = Invoke-LocalJson "$script:UiUrl/api/fleet"
        $fleetIds = @($proxy.body.records | ForEach-Object { $_.drone_id })
        if ($proxy.status -ne 200 -or $fleetIds -notcontains 'SIMULATION-GUIDE') { throw 'UI proxy does not reach this run mock Fleet. Check Vite configuration.' }
        Write-Host "Open $script:UiUrl/home/devices in your browser. No browser window is opened automatically." -ForegroundColor Cyan
        if (-not (Ask 'ui-ready' 'Check that Fleet contains SIMULATION-GUIDE and the network builder opens.' 'Does this page show the correct simulation and working Fleet controls?')) { return }
        $before = Get-Ledger
        [void](Ask 'discovery' 'Open Configure fleet network. Name it GUIDE-HAPPY; expected drone radios=1 (plus 1 GCS). Continue. Inspect both radios: different MACs, same factory IP. Do not configure yet.' 'Are both distinct MACs available without an automatic write?')
        Assert-LedgerUnchanged $before (Get-Ledger) 'discovery-no-write'
        [void](Checkpoint '01-discovery')
        [void](Ask 'happy-first' 'Select radio 1. Change an ordinary field and one field CHUD marks reboot-required. Review and approve the plan; confirm each transaction. Assign to SIMULATION-GUIDE. Do not add the second radio yet.' 'Did separate transactions pass final readback, with Finish blocked until radio 2?')
        [void](Checkpoint '02-first-radio')
        [void](Ask 'happy-second' 'Add another radio, select its different MAC, approve any TrellisWare safety acknowledgment, complete configuration/readback, and assign Ground control. Finish the network.' 'Does it finish as configured, with topology still unverified?')
        $happy = Checkpoint '03-finished'
        $finished = @($happy['/api/radio/networks'].body.networks | Where-Object { $_.name -eq 'GUIDE-HAPPY' -and $_.finished -eq $true -and $_.state -eq 'configured_unverified' -and @($_.nodes).Count -eq 2 -and @($_.nodes | Where-Object { $_.state -ne 'verified' -or $null -eq $_.asset_binding }).Count -eq 0 })
        if ($finished.Count -eq 1 -and @($finished[0].nodes.mac | Select-Object -Unique).Count -eq 2) { Add-Result 'happy-api' 'PASS' 'Two distinct verified and assigned MACs; finished as configured_unverified.' }
        else { Add-Result 'happy-api' 'FAIL' 'API evidence does not satisfy the two-radio happy path.'; return }
        foreach ($fault in @('stale_device','authentication_failed','apply_error','apply_timeout','missing_operation_id','operation_expired','readback_mismatch','reboot_required')) {
            Set-Fault $null
            if (-not (Ask "prepare-$fault" "Create a new draft named GUIDE-$fault. Select an eligible radio and prepare a non-empty delta, but do not click configure/apply. For readback_mismatch, stage only an ordinary network_id change." 'Is the new draft ready before applying?')) { continue }
            $beforeFault = Get-Ledger
            Set-Fault $fault
            try {
                [void](Ask "fault-$fault" "Fault active: $fault. Attempt the prepared UI action once. Inspect errors, operation ID, readback, and recovery. Expected: stale/auth reject; apply errors/timeouts fail; missing ID requires recovery; expiry reports rollback; mismatch cannot verify; reboot needs sequential confirmation. Do not repeatedly retry." 'Does the UI report the expected outcome without falsely claiming completion?')
                [void](Checkpoint ("fault-$fault"))
                if ($fault -in @('stale_device','authentication_failed','apply_error','apply_timeout')) { Assert-LedgerUnchanged $beforeFault (Get-Ledger) "fault-$fault-no-write" }
            } finally { Set-Fault $null }
        }
        foreach ($phase in @('applying','awaiting_confirmation')) {
            if (-not (Ask "prepare-restart-$phase" "Use a new draft named GUIDE-RESTART-$phase. Start a non-empty transaction and leave the UI at $phase. The current emulator completes apply immediately; use U if applying cannot be held." "Is the node visibly in $phase now?")) { continue }
            $pre = Checkpoint "restart-$phase-before"
            $pending = @($pre['/api/radio/networks'].body.networks | Where-Object { $_.name -eq "GUIDE-RESTART-$phase" } | ForEach-Object { $_.nodes } | Where-Object { $_.state -eq $phase })
            if ($pending.Count -ne 1) { Add-Result "restart-$phase" 'BLOCKED' 'API did not confirm exactly one node in the requested state; no process was interrupted.'; continue }
            $beforeRestart = Get-Ledger
            Stop-Owned 'bridge'
            # Preserve previous startup logs before reusing the same log filenames.
            foreach ($suffix in @('stdout','stderr')) { Copy-Item -LiteralPath (Join-Path $directory "bridge.$suffix.log") -Destination (Join-Path $directory "restart-$phase-bridge.$suffix.log") }
            Start-Bridge
            $post = Checkpoint "restart-$phase-after"
            Assert-LedgerUnchanged $beforeRestart (Get-Ledger) "restart-$phase-no-replay"
            $recovered = @($post['/api/radio/networks'].body.networks | Where-Object { $_.name -eq "GUIDE-RESTART-$phase" } | ForEach-Object { $_.nodes } | Where-Object { $_.state -eq 'recovery_required' })
            if ($recovered.Count -eq 1) { Add-Result "restart-$phase-api" 'PASS' 'Persisted node changed to recovery_required.' } else { Add-Result "restart-$phase-api" 'FAIL' 'Recovery-required state missing after restart.' }
            [void](Ask "restart-$phase-ui" 'Reload ARC. Inspect recovery status, then explicitly reconcile the retained operation and current readback.' 'Does recovery work without an unrequested apply?')
            [void](Checkpoint "restart-$phase-reconciled")
        }
        foreach ($action in @('delete-draft','remove-node','archive-network')) {
            $beforeDelete = Get-Ledger
            [void](Ask $action "In ARC perform $action on a test definition. Check the confirmation text and verify that only the definition is removed or archived. Use S if the UI has no such action." 'Did the definition change correctly without claiming a radio reset?')
            Assert-LedgerUnchanged $beforeDelete (Get-Ledger) "$action-no-write"
            [void](Checkpoint $action)
        }
        [void](Ask 'ui-usability' 'Check narrow and desktop widths, dark/light themes, keyboard navigation, Escape, and focus restoration. Inspect the browser console and network panel.' 'Are these screens usable, accurately labeled, and free of unexpected loading/errors?')
        Add-Result 'extended-matrix' 'BLOCKED' 'True socket timeout, plug/unplug lifecycle, durable emulator restart, cross-MAC operation substitution, all journal crash windows, secret-redaction checks, and automated Playwright coverage require the integration harness extensions. This guide cannot close all of #43.'
        [void](Checkpoint '99-final')
        $script:Run.completed = $true
    } catch [OperationCanceledException] {
        Add-Result 'operator-stop' 'SKIPPED' $_.Exception.Message 'operator'
    } catch {
        Add-Result 'unexpected-stop' 'BLOCKED' $_.Exception.Message
    } finally {
        try { Stop-Owned } catch { Add-Result 'cleanup' 'BLOCKED' $_.Exception.Message }
        Save-Report
        Write-Host "`nVerdict: $($script:Run.verdict). Evidence: $($script:Run.directory)" -ForegroundColor Cyan
        Write-Host 'Only processes started by this guide were stopped. Reports and test state are retained.'
        $script:TestToken = ''
    }
}

if (-not $LibraryOnly) { Invoke-Walkthrough }
