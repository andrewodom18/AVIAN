[CmdletBinding()]
param(
    [ValidateRange(1, 10)]
    [int]$RadioCount = 2,
    [string]$RadioIp = '10.1.0.2',
    [string]$PcIp = '10.1.0.20',
    [int]$PrefixLength = 24,
    [string]$EthernetAdapter,
    [string]$AvianRoot = (Join-Path $env:USERPROFILE 'Desktop\AVIAN'),
    [string]$ClientIdentityPkcs12 = (Join-Path $env:USERPROFILE 'Desktop\oemcert-compat.p12'),
    [string]$CaCertificatePem,
    [string]$ResultsRoot = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\guided-validation'),
    [switch]$SkipAuthenticatedProbe,
    [switch]$PreflightOnly,
    [switch]$StartArcStack
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Section {
    param([string]$Message)
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

function Confirm-Exact {
    param([string]$Prompt, [string]$Expected)
    $answer = Read-Host "$Prompt Type $Expected to continue"
    if ($answer -cne $Expected) {
        throw "Confirmation did not match '$Expected'. No radio configuration was changed."
    }
}

function Select-RadioEthernetAdapter {
    param([string]$Requested)

    $physical = @(Get-NetAdapter -Physical -ErrorAction Stop | Sort-Object Name)
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
            Where-Object IPAddress -eq $PcIp).Count -gt 0
    } | Select-Object -First 1
    if (-not $suggested) { $suggested = $candidates | Where-Object Name -eq 'Ethernet 2' | Select-Object -First 1 }
    if (-not $suggested) { $suggested = $candidates | Select-Object -First 1 }
    if (-not $suggested) { throw 'No physical Ethernet adapter was found.' }

    $answer = Read-Host "Ethernet adapter to use [$($suggested.Name)]"
    if ([string]::IsNullOrWhiteSpace($answer)) { return $suggested.Name }
    $chosen = $physical | Where-Object Name -eq $answer | Select-Object -First 1
    if (-not $chosen) { throw "Ethernet adapter '$answer' was not found." }
    return $chosen.Name
}

function Get-StableRadioSource {
    param([string]$MacAddress)
    $compact = $MacAddress -replace '[^0-9A-Fa-f]', ''
    if ($compact.Length -ne 12) { return $null }
    return "tw-bench-$($compact.ToLowerInvariant())"
}

Write-Section 'AVIAN guided real-radio validation'
Write-Host 'This workflow is read-only. It does not change radio configuration, firmware, host networking, routes, or certificates.' -ForegroundColor Green
Write-Host 'It tests one factory-address radio at a time, then optionally starts the existing ARC/CHUD evidence stack.'

$accessScript = Join-Path $PSScriptRoot 'Test-RadioAccess.ps1'
$stackScript = Join-Path $PSScriptRoot 'Start-RadioBenchTest.ps1'
$manifest = Join-Path $AvianRoot 'Cargo.toml'
foreach ($required in @($accessScript, $stackScript, $manifest)) {
    if (-not (Test-Path -LiteralPath $required)) { throw "Required file was not found: $required" }
}
if (-not $SkipAuthenticatedProbe -and -not (Test-Path -LiteralPath $ClientIdentityPkcs12)) {
    throw 'The PKCS#12 client identity was not found. Supply -ClientIdentityPkcs12 or use -SkipAuthenticatedProbe.'
}
if ($CaCertificatePem -and -not (Test-Path -LiteralPath $CaCertificatePem)) {
    throw 'The supplied radio CA certificate was not found.'
}

$EthernetAdapter = Select-RadioEthernetAdapter -Requested $EthernetAdapter
$pcAddressPresent = @(
    Get-NetIPAddress -InterfaceAlias $EthernetAdapter -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object IPAddress -eq $PcIp
).Count -gt 0
if (-not $pcAddressPresent) {
    throw "$EthernetAdapter does not have $PcIp/$PrefixLength. Run Enable-RadioEthernet.ps1 separately so a recovery snapshot is created."
}

Write-Section 'Build the read-only AVIAN probe'
Push-Location $AvianRoot
try {
    & cargo build --locked -p arc-radio-plugin
    if ($LASTEXITCODE -ne 0) { throw 'The AVIAN radio probe failed to build.' }
} finally {
    Pop-Location
}
$probe = Join-Path $AvianRoot 'target\debug\arc-radio-plugin.exe'
if (-not (Test-Path -LiteralPath $probe)) { throw "Probe binary was not produced at '$probe'." }
if ($PreflightOnly) {
    Write-Section 'Preflight complete'
    Write-Host "AVIAN probe: ready" -ForegroundColor Green
    Write-Host "Ethernet adapter: $EthernetAdapter" -ForegroundColor Green
    Write-Host "Bench address: $PcIp/$PrefixLength" -ForegroundColor Green
    Write-Host 'Client identity: available' -ForegroundColor Green
    Write-Host 'No radio was contacted and no setting was changed.' -ForegroundColor Green
    return
}

$runId = Get-Date -Format 'yyyyMMdd-HHmmss'
$runRoot = Join-Path $ResultsRoot $runId
New-Item -ItemType Directory -Force -Path $runRoot | Out-Null

Write-Section 'Physical safety gate'
Write-Host 'Before powering a radio:' -ForegroundColor Yellow
Write-Host '  - Install the approved antennas or RF loads.'
Write-Host '  - Use the approved power supply and Ethernet cable.'
Write-Host '  - Keep every other factory-address radio powered off and disconnected.'
Write-Host '  - Do not perform this test on an installed or airborne aircraft.'
Confirm-Exact -Prompt 'Confirm the bench is prepared.' -Expected 'READY'

$results = @()
for ($sequence = 1; $sequence -le $RadioCount; $sequence++) {
    $radioRoot = Join-Path $runRoot "radio-$sequence"
    New-Item -ItemType Directory -Force -Path $radioRoot | Out-Null

    Write-Section "Radio $sequence of $RadioCount"
    & $accessScript `
        -RadioIp $RadioIp `
        -EthernetAdapter $EthernetAdapter `
        -PcIp $PcIp `
        -PrefixLength $PrefixLength `
        -ResultsRoot $radioRoot `
        -SequenceStart $sequence `
        -OneShot `
        -SkipBrowserPrompt

    $accessSummaryPath = Get-ChildItem -LiteralPath $radioRoot -Filter 'summary.json' -File -Recurse |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1 -ExpandProperty FullName
    if (-not $accessSummaryPath) { throw "Radio $sequence access summary was not produced." }
    $access = Get-Content -Raw -LiteralPath $accessSummaryPath | ConvertFrom-Json
    $source = Get-StableRadioSource -MacAddress ([string]$access.RadioMac)
    $probeExit = $null
    $observationPath = Join-Path $radioRoot 'authenticated-observation.json'
    $probeLog = Join-Path $radioRoot 'authenticated-probe.log'

    if ($SkipAuthenticatedProbe) {
        Write-Host 'Authenticated probe skipped by request.' -ForegroundColor Yellow
    } elseif (-not $access.Tcp443) {
        Write-Host 'HTTPS/443 was not reachable; authenticated probe was skipped.' -ForegroundColor Yellow
    } elseif (-not $source) {
        Write-Host 'A stable MAC identity was not learned; authenticated probe was skipped.' -ForegroundColor Yellow
    } else {
        Write-Section "Read authenticated state from Radio $sequence"
        $arguments = @(
            'trellisware-probe',
            '--radio-url', "https://$RadioIp",
            '--source', $source,
            '--client-identity-pkcs12', $ClientIdentityPkcs12,
            '--output', $observationPath
        )
        if ($CaCertificatePem) {
            $arguments += @('--ca-certificate-pem', $CaCertificatePem)
        } else {
            Write-Host 'No CA file supplied; using the explicit lab-only self-signed-certificate override.' -ForegroundColor Yellow
            $arguments += '--accept-invalid-server-certificate'
        }

        $previousErrorAction = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & $probe @arguments *> $probeLog
            $probeExit = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorAction
        }
        if ($probeExit -eq 0) {
            Write-Host "Authenticated read succeeded for $source." -ForegroundColor Green
        } else {
            Write-Host "Authenticated read failed for $source. The generic diagnostic is saved in the run folder." -ForegroundColor Yellow
        }
    }

    $results += [pscustomobject]@{
        Sequence = $sequence
        RadioIp = $RadioIp
        RadioMac = [string]$access.RadioMac
        Source = $source
        Ping = [bool]$access.RadioPing
        Tcp443 = [bool]$access.Tcp443
        AuthenticatedProbeAttempted = ($null -ne $probeExit)
        AuthenticatedProbeExitCode = $probeExit
        AuthenticatedObservationProduced = (Test-Path -LiteralPath $observationPath)
        AccessSummary = $accessSummaryPath
        EvidenceDirectory = $radioRoot
    }

    Write-Section "Disconnect Radio $sequence"
    Write-Host 'Disconnect Ethernet and power from this radio before continuing. Leave the antennas or RF loads installed.' -ForegroundColor Yellow
    Confirm-Exact -Prompt 'Confirm this radio is powered off and disconnected.' -Expected 'DISCONNECTED'
}

$summaryPath = Join-Path $runRoot 'guided-validation-summary.json'
$results | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath

Write-Section 'Individual-radio validation complete'
$results | Format-Table Sequence, RadioMac, Ping, Tcp443, AuthenticatedProbeAttempted, AuthenticatedProbeExitCode -AutoSize
Write-Host "Summary: $summaryPath" -ForegroundColor Green
Write-Host 'No radio configuration was changed.' -ForegroundColor Green

$startStackNow = $StartArcStack
if (-not $startStackNow) {
    $answer = Read-Host 'Start the full ARC/CHUD one-radio evidence workflow now? [Y/N]'
    $startStackNow = $answer -match '^(?i)y(es)?$'
}
if ($startStackNow) {
    Write-Section 'Start ARC/CHUD evidence workflow'
    $previousErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & docker info *> $null
        $dockerExit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }
    if ($dockerExit -ne 0) {
        Write-Host 'Docker Desktop is not running. Start it, then run Start-RadioBenchTest.ps1.' -ForegroundColor Yellow
    } else {
        & $stackScript -AvianRoot $AvianRoot
    }
} else {
    Write-Host 'Full-stack startup skipped. Run Start-RadioBenchTest.ps1 when Docker Desktop is ready.' -ForegroundColor Yellow
}
