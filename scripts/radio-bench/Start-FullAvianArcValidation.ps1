[CmdletBinding()]
param(
    [string]$ArcRoot = (Join-Path $env:USERPROFILE 'Desktop\arc-uas-main-20260827'),
    [string]$AvianRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..')),
    [string]$ClientIdentityPkcs12 = (Join-Path $env:USERPROFILE 'Desktop\Work Docs\Security\OEM Certificates\oemcert-compat.p12'),
    [string]$EthernetAdapter = 'Ethernet 2',
    [string]$RadioIp = '10.1.0.2',
    [string]$PcIp = '10.1.0.20',
    [int]$PrefixLength = 24,
    [string]$ArcUrl = 'https://localhost:3000/home/devices',
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\full-avian-arc-validation'),
    [switch]$PreflightOnly,
    [switch]$SkipAuthenticatedProbe,
    [switch]$SkipApplicationStartup
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:RunId = Get-Date -Format 'yyyyMMdd-HHmmss'
$script:RunRoot = Join-Path $ResultsRoot $script:RunId
$script:CheckpointPath = Join-Path $script:RunRoot 'checkpoints.json'
$script:SummaryPath = Join-Path $script:RunRoot 'summary.txt'
$script:Checkpoints = [System.Collections.Generic.List[object]]::new()

function Write-Section {
    param([string]$Message)
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function Save-Checkpoints {
    $script:Checkpoints | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $script:CheckpointPath
}

function Add-Checkpoint {
    param(
        [string]$Phase,
        [string]$Check,
        [ValidateSet('pass', 'fail', 'blocked', 'skipped', 'info')]
        [string]$Result,
        [string]$Detail
    )
    $script:Checkpoints.Add([pscustomobject]@{
        Timestamp = (Get-Date).ToString('o')
        Phase = $Phase
        Check = $Check
        Result = $Result
        Detail = $Detail
    })
    Save-Checkpoints
    $color = switch ($Result) {
        'pass' { 'Green' }
        'fail' { 'Red' }
        'blocked' { 'Yellow' }
        'skipped' { 'DarkYellow' }
        default { 'Gray' }
    }
    Write-Host "[$($Result.ToUpperInvariant())] $Check - $Detail" -ForegroundColor $color
}

function Read-YesNo {
    param([string]$Question)
    while ($true) {
        $answer = (Read-Host "$Question [Y/N]").Trim()
        if ($answer -match '^(?i)y(es)?$') { return $true }
        if ($answer -match '^(?i)n(o)?$') { return $false }
        Write-Host 'Please enter Y or N.' -ForegroundColor Yellow
    }
}

function Read-RequiredText {
    param([string]$Prompt)
    while ($true) {
        $answer = (Read-Host $Prompt).Trim()
        if ($answer) { return $answer }
        Write-Host 'A short answer is required so the report explains the result.' -ForegroundColor Yellow
    }
}

function Confirm-Exact {
    param([string]$Prompt, [string]$Expected)
    $answer = Read-Host "$Prompt Type $Expected to continue"
    if ($answer -cne $Expected) {
        throw "Confirmation did not match '$Expected'. The validation stopped without changing a radio."
    }
}

function Get-StableSource {
    param([string]$MacAddress)
    $compact = $MacAddress -replace '[^0-9A-Fa-f]', ''
    if ($compact.Length -ne 12) { return $null }
    return "tw-bench-$($compact.ToLowerInvariant())"
}

function Convert-MacToLinkLocal {
    param([string]$MacAddress)
    $compact = $MacAddress -replace '[^0-9A-Fa-f]', ''
    if ($compact.Length -ne 12) { return $null }
    $bytes = for ($index = 0; $index -lt 12; $index += 2) {
        [Convert]::ToByte($compact.Substring($index, 2), 16)
    }
    $bytes[0] = $bytes[0] -bxor 0x02
    $groups = @(
        (([int]$bytes[0] -shl 8) -bor [int]$bytes[1]),
        (([int]$bytes[2] -shl 8) -bor 0xff),
        ((0xfe -shl 8) -bor [int]$bytes[3]),
        (([int]$bytes[4] -shl 8) -bor [int]$bytes[5])
    )
    return 'fe80::{0:x}:{1:x}:{2:x}:{3:x}' -f $groups
}

function Invoke-AuthenticatedRead {
    param(
        [int]$Sequence,
        [pscustomobject]$Access,
        [string]$OutputDirectory,
        [string]$Probe
    )
    $source = Get-StableSource -MacAddress ([string]$Access.RadioMac)
    if ($SkipAuthenticatedProbe) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Authenticated read' -Result 'skipped' -Detail 'Skipped by command-line request.'
        return $null
    }
    if (-not $Access.Tcp443) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Authenticated read' -Result 'blocked' -Detail 'HTTPS port 443 was not reachable.'
        return $null
    }
    if (-not $source) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Authenticated read' -Result 'blocked' -Detail 'A stable MAC was not learned.'
        return $null
    }

    $observationPath = Join-Path $OutputDirectory 'authenticated-observation.json'
    $probeLog = Join-Path $OutputDirectory 'authenticated-probe.log'
    $arguments = @(
        'trellisware-probe',
        '--radio-url', "https://$RadioIp",
        '--source', $source,
        '--client-identity-pkcs12', $ClientIdentityPkcs12,
        '--accept-invalid-server-certificate',
        '--output', $observationPath
    )
    $previousErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $Probe @arguments *> $probeLog
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }
    if ($exitCode -eq 0 -and (Test-Path -LiteralPath $observationPath)) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Authenticated read' -Result 'pass' -Detail "Blank-password-compatible identity read $source."
    } else {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Authenticated read' -Result 'blocked' -Detail "Discovery worked, but the protected read exited $exitCode. See the sanitized probe log."
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        ObservationPath = $observationPath
        LogPath = $probeLog
        Source = $source
    }
}

function Invoke-OneRadioPhase {
    param(
        [int]$Sequence,
        [string]$AccessScript,
        [string]$Probe
    )
    Write-Section "Radio $Sequence individual validation"
    Write-Host "Power and Ethernet-connect only Radio $Sequence. Every other factory-address radio must remain powered off and disconnected." -ForegroundColor Yellow
    Read-Host "Press Enter after Radio $Sequence has been powered for at least 30 seconds" | Out-Null
    $radioRoot = Join-Path $script:RunRoot "radio-$Sequence"
    New-Item -ItemType Directory -Force -Path $radioRoot | Out-Null
    & $AccessScript `
        -RadioIp $RadioIp `
        -EthernetAdapter $EthernetAdapter `
        -PcIp $PcIp `
        -PrefixLength $PrefixLength `
        -ResultsRoot $radioRoot `
        -SequenceStart $Sequence `
        -OneShot `
        -SkipBrowserPrompt `
        -RequireExistingPcAddress | Out-Host

    $summaryFile = Get-ChildItem -LiteralPath $radioRoot -Filter 'summary.json' -File -Recurse |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $summaryFile) { throw "Radio $Sequence did not produce a summary.json file." }
    $access = Get-Content -Raw -LiteralPath $summaryFile.FullName | ConvertFrom-Json
    if ($access.RadioPing) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Direct IPv4 reachability' -Result 'pass' -Detail "$RadioIp replied on $EthernetAdapter."
    } else {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Direct IPv4 reachability' -Result 'fail' -Detail "$RadioIp did not reply."
    }
    if ([string]$access.RadioMac) {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Stable hardware identity' -Result 'pass' -Detail "MAC $($access.RadioMac) was learned."
    } else {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Stable hardware identity' -Result 'fail' -Detail 'No MAC was learned from the neighbor table.'
    }
    if (-not (Read-YesNo -Question "Did the physical result shown above match what you observed for Radio $Sequence?")) {
        $note = Read-RequiredText -Prompt 'What looked wrong?'
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Operator observation' -Result 'fail' -Detail $note
    } else {
        Add-Checkpoint -Phase "radio-$Sequence" -Check 'Operator observation' -Result 'pass' -Detail 'Operator confirmed the displayed evidence.'
    }
    Invoke-AuthenticatedRead -Sequence $Sequence -Access $access -OutputDirectory $radioRoot -Probe $Probe | Out-Null
    if (-not $access.RadioPing -or -not [string]$access.RadioMac) {
        throw "Radio $Sequence failed the minimum direct-discovery gate. Correct the bench before continuing."
    }
    Write-Host "Power off Radio $Sequence and disconnect Ethernet before continuing." -ForegroundColor Yellow
    Confirm-Exact -Prompt "Confirm Radio $Sequence is powered off and disconnected." -Expected 'DISCONNECTED'
    return $access
}

function Ask-UiCheck {
    param([string]$Check, [string]$Question)
    if (Read-YesNo -Question $Question) {
        Add-Checkpoint -Phase 'arc-ui' -Check $Check -Result 'pass' -Detail 'Operator confirmed the expected UI behavior.'
    } else {
        $note = Read-RequiredText -Prompt 'What did you see instead?'
        Add-Checkpoint -Phase 'arc-ui' -Check $Check -Result 'fail' -Detail $note
    }
}

function Write-FinalSummary {
    $counts = $script:Checkpoints | Group-Object Result | ForEach-Object {
        [pscustomobject]@{ Result = $_.Name; Count = $_.Count }
    }
    $failures = @($script:Checkpoints | Where-Object Result -eq 'fail')
    $blocked = @($script:Checkpoints | Where-Object Result -eq 'blocked')
    $verdict = if ($failures.Count -gt 0) { 'FAIL' } elseif ($blocked.Count -gt 0) { 'PARTIAL' } else { 'PASS' }
    @(
        "Full AVIAN/ARC two-radio validation: $script:RunId",
        "Verdict: $verdict",
        "Results: $script:RunRoot",
        '',
        'Counts:',
        ($counts | ForEach-Object { "  $($_.Result): $($_.Count)" }),
        '',
        'Failures:',
        ($(if ($failures.Count) { $failures | ForEach-Object { "  [$($_.Phase)] $($_.Check): $($_.Detail)" } } else { '  none' })),
        '',
        'Blocked:',
        ($(if ($blocked.Count) { $blocked | ForEach-Object { "  [$($_.Phase)] $($_.Check): $($_.Detail)" } } else { '  none' })),
        '',
        'Safety statement:',
        '  This walkthrough is limited to read-only radio access and operator UI checks.',
        '  It did not apply radio configuration, change firmware, or bypass CHUD.'
    ) | Set-Content -LiteralPath $script:SummaryPath
    Write-Section "Validation $verdict"
    Get-Content -LiteralPath $script:SummaryPath | Write-Host
}

New-Item -ItemType Directory -Force -Path $script:RunRoot | Out-Null

try {
    Write-Section 'Full AVIAN + ARC two-radio walkthrough'
    Write-Host 'This guided test is read-only for the TW-950 radios.' -ForegroundColor Green
    Write-Host 'It will not change RF settings, management IPs, firmware, certificates, routes, or host networking.'
    Write-Host 'CHUD remains the only approved physical-radio configuration authority.'

    $requiredPaths = @(
        $ArcRoot,
        $AvianRoot,
        $ClientIdentityPkcs12,
        (Join-Path $PSScriptRoot 'Test-RadioAccess.ps1'),
        (Join-Path $PSScriptRoot 'Monitor-TwoRadioMesh.ps1'),
        (Join-Path $PSScriptRoot 'Start-RadioBenchTest.ps1'),
        (Join-Path $AvianRoot 'Cargo.toml')
    )
    foreach ($required in $requiredPaths) {
        if (-not (Test-Path -LiteralPath $required)) { throw "Required path was not found: $required" }
    }
    $adapter = Get-NetAdapter -Name $EthernetAdapter -ErrorAction Stop
    $pcAddress = Get-NetIPAddress -InterfaceAlias $EthernetAdapter -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -eq $PcIp -and $_.PrefixLength -eq $PrefixLength } |
        Select-Object -First 1
    if (-not $pcAddress) {
        throw "$EthernetAdapter does not have $PcIp/$PrefixLength. Run Enable-RadioEthernet.ps1 separately; this walkthrough will not change networking."
    }
    Add-Checkpoint -Phase 'preflight' -Check 'Dedicated Ethernet address' -Result 'pass' -Detail "$EthernetAdapter has $PcIp/$PrefixLength and status $($adapter.Status)."
    Add-Checkpoint -Phase 'preflight' -Check 'Client identity' -Result 'pass' -Detail 'The blank-password-compatible PKCS#12 bundle is available outside the repository.'
    $knownLinkLocal = Convert-MacToLinkLocal -MacAddress '00:1e:3f:20:9a:10'
    if ($knownLinkLocal -ne 'fe80::21e:3fff:fe20:9a10') {
        throw "MAC-to-link-local derivation returned an unexpected address: $knownLinkLocal"
    }
    Add-Checkpoint -Phase 'preflight' -Check 'Link-local identity derivation' -Result 'pass' -Detail 'Known TrellisWare MAC produced the expected scoped IPv6 address.'

    Write-Section 'Build the locked read-only probe'
    Push-Location $AvianRoot
    try {
        & cargo build --locked -p arc-radio-plugin
        if ($LASTEXITCODE -ne 0) { throw 'The AVIAN read-only radio probe failed to build.' }
    } finally {
        Pop-Location
    }
    $probe = Join-Path $AvianRoot 'target\debug\arc-radio-plugin.exe'
    if (-not (Test-Path -LiteralPath $probe)) { throw "Probe binary was not produced at $probe." }
    Add-Checkpoint -Phase 'preflight' -Check 'AVIAN read-only probe' -Result 'pass' -Detail 'The locked arc-radio-plugin build succeeded.'

    if ($PreflightOnly) {
        Add-Checkpoint -Phase 'preflight' -Check 'Radio contact' -Result 'skipped' -Detail 'Preflight-only mode contacted no radio.'
        Write-FinalSummary
        return
    }

    Write-Section 'Physical safety gate'
    Write-Host 'Before powering either radio:' -ForegroundColor Yellow
    Write-Host '  - Install approved antennas or RF loads on both TW-950s.'
    Write-Host '  - Use approved power supplies and Ethernet cables.'
    Write-Host '  - Keep only one factory-address radio powered during individual discovery.'
    Write-Host '  - Keep the radios on the bench, not on an installed or airborne aircraft.'
    Confirm-Exact -Prompt 'Confirm the bench satisfies all four conditions.' -Expected 'READY'
    Add-Checkpoint -Phase 'safety' -Check 'Bench preparation' -Result 'pass' -Detail 'Operator entered the exact READY confirmation.'

    $accessScript = Join-Path $PSScriptRoot 'Test-RadioAccess.ps1'
    $radio1 = Invoke-OneRadioPhase -Sequence 1 -AccessScript $accessScript -Probe $probe
    $radio2 = Invoke-OneRadioPhase -Sequence 2 -AccessScript $accessScript -Probe $probe
    $mac1 = ([string]$radio1.RadioMac).ToLowerInvariant()
    $mac2 = ([string]$radio2.RadioMac).ToLowerInvariant()
    if ($mac1 -eq $mac2) {
        Add-Checkpoint -Phase 'identity' -Check 'Distinct radio identities' -Result 'fail' -Detail "Both runs learned $mac1."
        throw 'The two physical radios were not distinguished by MAC. Stop before mesh or UI testing.'
    }
    Add-Checkpoint -Phase 'identity' -Check 'Distinct radio identities' -Result 'pass' -Detail "Radio 1=$mac1; Radio 2=$mac2; shared factory IPv4 did not collapse identity."

    Write-Section 'Two-radio RF-path validation'
    $radio1LinkLocal = Convert-MacToLinkLocal -MacAddress $mac1
    $radio2LinkLocal = Convert-MacToLinkLocal -MacAddress $mac2
    Write-Host "Radio 1 remote address: $radio1LinkLocal"
    Write-Host "Radio 2 direct address: $radio2LinkLocal"
    Write-Host 'Power both radios. Connect only Radio 2 to Ethernet. Leave Radio 1 Ethernet-unplugged.' -ForegroundColor Yellow
    Confirm-Exact -Prompt 'Confirm both antennas are installed, both radios are powered, and only Radio 2 has Ethernet.' -Expected 'MESH READY'
    $meshRoot = Join-Path $script:RunRoot 'two-radio-mesh'
    & (Join-Path $PSScriptRoot 'Monitor-TwoRadioMesh.ps1') `
        -EthernetAdapter $EthernetAdapter `
        -DirectRadioIPv6 $radio2LinkLocal `
        -RemoteRadioIPv6 $radio1LinkLocal `
        -ResultsRoot $meshRoot
    if (Read-YesNo -Question 'Did PowerShell show Radio 2 direct reachability?') {
        Add-Checkpoint -Phase 'mesh' -Check 'Direct radio reachability' -Result 'pass' -Detail 'Operator observed the directly attached radio.'
    } else {
        Add-Checkpoint -Phase 'mesh' -Check 'Direct radio reachability' -Result 'fail' -Detail (Read-RequiredText -Prompt 'What failed?')
    }
    if (Read-YesNo -Question 'Did PowerShell show Radio 1 reachable while its Ethernet remained disconnected?') {
        Add-Checkpoint -Phase 'mesh' -Check 'Remote RF-path reachability' -Result 'pass' -Detail 'Operator observed the remote radio through the directly attached radio.'
    } else {
        Add-Checkpoint -Phase 'mesh' -Check 'Remote RF-path reachability' -Result 'fail' -Detail (Read-RequiredText -Prompt 'What failed?')
    }

    if ($SkipApplicationStartup) {
        Add-Checkpoint -Phase 'applications' -Check 'ARC/CHUD startup' -Result 'skipped' -Detail 'Skipped by command-line request.'
    } elseif (Read-YesNo -Question 'Start the real ARC/CHUD stack and open the ARC Devices page in Edge now?') {
        try {
            & (Join-Path $PSScriptRoot 'Start-RadioBenchTest.ps1') `
                -ArcRoot $ArcRoot `
                -AvianRoot $AvianRoot `
                -ArcUrl $ArcUrl `
                -SkipStartupPrompt `
                -SkipConnectionMonitor
            Add-Checkpoint -Phase 'applications' -Check 'ARC/CHUD startup' -Result 'pass' -Detail 'Real-hardware-safe services and ArcUI started without a simulator.'
        } catch {
            Add-Checkpoint -Phase 'applications' -Check 'ARC/CHUD startup' -Result 'blocked' -Detail $_.Exception.Message
        }
    } else {
        Add-Checkpoint -Phase 'applications' -Check 'ARC/CHUD startup' -Result 'skipped' -Detail 'Operator declined application startup.'
    }

    if (@($script:Checkpoints | Where-Object { $_.Phase -eq 'applications' -and $_.Result -eq 'pass' }).Count -gt 0) {
        Write-Section 'ARC Devices visual checks'
        Write-Host 'Use the ARC Devices page opened in Edge. Do not click any button that applies physical radio settings.' -ForegroundColor Yellow
        Ask-UiCheck -Check 'Page health' -Question 'Did the Devices page load without Fail to fetch?'
        Ask-UiCheck -Check 'Radio Network presentation' -Question 'Is RADIO NETWORK visible and collapsed initially?'
        Ask-UiCheck -Check 'Physical inventory' -Question 'After expanding RADIO NETWORK, are the physical radios identified by their real MACs with no simulated nodes?'
        Ask-UiCheck -Check 'Fleet cards' -Question 'Does Fleet avoid creating extra drone cards just because radios were discovered?'
        Ask-UiCheck -Check 'Truthful association state' -Question 'Does any operator assignment remain labeled attachment not verified rather than falsely verified?'
        Ask-UiCheck -Check 'Configuration safety gate' -Question 'Does + Configure fleet network keep Finish disabled until every radio is CHUD-verified and assigned?'

        Write-Section 'Disconnect and recovery check'
        Write-Host 'Unplug only Radio 2 Ethernet. Leave both radios powered and wait for ARC/CHUD state to update.' -ForegroundColor Yellow
        Read-Host 'Press Enter after the cable is unplugged' | Out-Null
        Start-Sleep -Seconds 12
        Ask-UiCheck -Check 'Disconnect truthfulness' -Question 'Did ARC stop presenting the directly attached radio as normally connected without deleting the Fleet drone?'
        Write-Host 'Reconnect Radio 2 Ethernet and wait up to 30 seconds.' -ForegroundColor Yellow
        Read-Host 'Press Enter after the cable is reconnected' | Out-Null
        Start-Sleep -Seconds 12
        Ask-UiCheck -Check 'Identity-preserving recovery' -Question 'Did the same MAC recover without creating a duplicate radio or drone?'
    } else {
        Add-Checkpoint -Phase 'arc-ui' -Check 'Visual integration checks' -Result 'blocked' -Detail 'ARC/CHUD applications were not started successfully.'
    }

    Add-Checkpoint -Phase 'scope' -Check 'Physical configuration writes' -Result 'blocked' -Detail 'Deliberately not run: live CHUD apply/readback/rollback requires the configuration-path contract and recovery procedure to be reconciled first.'
    Add-Checkpoint -Phase 'scope' -Check 'Authenticated ARC session attestation' -Result 'blocked' -Detail 'ARC main does not yet expose the authenticated Fleet/PEAT assertion consumer; operator assignment must remain unverified.'
    Add-Checkpoint -Phase 'scope' -Check 'Two-hundred-node claim' -Result 'info' -Detail 'Two physical radios were exercised; the 200-drone result remains simulation evidence only.'
    Write-FinalSummary
} catch {
    Add-Checkpoint -Phase 'walkthrough' -Check 'Unexpected stop' -Result 'fail' -Detail $_.Exception.Message
    Write-FinalSummary
    throw
} finally {
    Write-Host "`nEvidence directory: $script:RunRoot" -ForegroundColor Green
    Write-Host 'No physical radio configuration was applied by this walkthrough.' -ForegroundColor Green
}
