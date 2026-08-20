[CmdletBinding()]
param(
    [string]$ArcRoot = (Join-Path $env:USERPROFILE 'Desktop\Work Docs\arc-edge\arc-uas-avian-radio'),
    [string]$AvianRoot = (Join-Path $env:USERPROFILE 'Desktop\AVIAN'),
    [string]$ArcUrl = 'https://localhost:3000/home/devices'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Section {
    param([string]$Message)
    Write-Host "`n=== $Message ===" -ForegroundColor Cyan
}

Write-Section 'AVIAN radio bench-test launcher'
Write-Host 'This launcher prepares a REAL-HARDWARE test only.' -ForegroundColor Green
Write-Host 'It restarts ARC comms, the ARC bridge, recorder/advisor services, CHUD, and the ARC UI.'
Write-Host 'It starts AVIAN real-radio discovery and the real ARC Link Manager.'
Write-Host 'It does not start a simulated radio, simulated node, or MAVLink simulator.'
Write-Host 'After startup, the monitor will explain and record each connection milestone.'
Read-Host 'Leave the radio Ethernet cable unplugged and press Enter to restart the application' | Out-Null

if (-not (Test-Path -LiteralPath $ArcRoot)) {
    throw "ARC repository was not found at '$ArcRoot'."
}
if (-not (Test-Path -LiteralPath $AvianRoot)) { throw "AVIAN repository was not found at '$AvianRoot'." }

$composeFile = Join-Path $ArcRoot 'infra\dev\docker-compose.yml'
$uiRoot = Join-Path $ArcRoot 'services\arc-ui'
if (-not (Test-Path -LiteralPath $composeFile)) { throw "Compose file was not found at '$composeFile'." }
if (-not (Test-Path -LiteralPath $uiRoot)) { throw "ARC UI was not found at '$uiRoot'." }

Write-Section 'Check Docker and simulator state'
& docker info *> $null
if ($LASTEXITCODE -ne 0) { throw 'Docker Desktop is not running.' }

$simContainers = @(
    & docker ps -a --format '{{.Names}}|{{.Command}}' |
        Select-String -Pattern 'simulate-radio|local-sim|arc-avian-local-radio-plugin'
)
if ($simContainers.Count -gt 0) {
    throw "A simulator container is present. Remove it before real testing: $($simContainers -join ', ')"
}
Write-Host 'No simulated radio or local-sim container is present.' -ForegroundColor Green

Write-Section 'Restart the real-hardware-safe services'
Push-Location $ArcRoot
try {
    & docker compose --project-name arc-avian-local --file $composeFile build comms dev-bridge flight-recorder landing-advisor
    if ($LASTEXITCODE -ne 0) { throw 'ARC backend build failed.' }
    & docker compose --project-name arc-avian-local --file $composeFile up --detach comms dev-bridge flight-recorder landing-advisor
    if ($LASTEXITCODE -ne 0) { throw 'ARC backend startup failed.' }
} finally {
    Pop-Location
}

$chudExists = & docker ps -a --format '{{.Names}}' | Where-Object { $_ -eq 'chud-local' }
if (-not $chudExists) { throw "The required external CHUD container 'chud-local' does not exist." }
& docker restart chud-local *> $null
if ($LASTEXITCODE -ne 0) { throw 'CHUD restart failed.' }

Write-Section 'Start the real AVIAN-to-ARC discovery path'
$linkManagerName = 'arc-avian-real-link-manager'
$linkManagerExists = & docker ps -a --filter "name=^/$linkManagerName$" --format '{{.Names}}'
if ($linkManagerExists) {
    throw "Container '$linkManagerName' already exists. Run Stop-RadioBenchTest.ps1 before starting a new evidence run."
} else {
    & docker run --detach `
        --name $linkManagerName `
        --rm `
        --volume 'arc-avian-local_arc-ipc:/run/arc' `
        'arc-link-manager:dev' `
        --device-id "$env:COMPUTERNAME-radio-bench" *> $null
}
if ($LASTEXITCODE -ne 0) { throw 'The real ARC Link Manager failed to start.' }

$existingPlugins = @(Get-CimInstance Win32_Process -Filter "Name = 'arc-radio-plugin.exe'")
if ($existingPlugins.Count -gt 0) { throw 'An AVIAN radio plugin is already running. Stop it before starting an evidence run.' }
Push-Location $AvianRoot
try {
    & cargo build --locked -p arc-radio-plugin
    if ($LASTEXITCODE -ne 0) { throw 'AVIAN radio plugin build failed.' }
} finally {
    Pop-Location
}
$avianPlugin = Join-Path $AvianRoot 'target\debug\arc-radio-plugin.exe'
if (-not (Test-Path -LiteralPath $avianPlugin)) { throw "The AVIAN radio plugin was not produced at '$avianPlugin'." }
$discoveryDirectory = Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\live-discovery'
New-Item -ItemType Directory -Force -Path $discoveryDirectory | Out-Null
$discoveryStamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$discoveryOutput = Join-Path $discoveryDirectory 'latest-discovery.json'
$discoveryStdout = Join-Path $discoveryDirectory "avian-discovery-$discoveryStamp.out.log"
$discoveryStderr = Join-Path $discoveryDirectory "avian-discovery-$discoveryStamp.err.log"
$avianProcess = Start-Process -FilePath $avianPlugin `
    -ArgumentList @(
        'trellisware-discover', '--probe-ip', '10.1.0.2', '--watch',
        '--interval-seconds', '2', '--zenoh-endpoint', 'tcp/127.0.0.1:7447',
        '--output', "`"$discoveryOutput`""
    ) `
    -WindowStyle Hidden `
    -RedirectStandardOutput $discoveryStdout `
    -RedirectStandardError $discoveryStderr `
    -PassThru
$arcCommit = (& git -C $ArcRoot rev-parse HEAD).Trim()
$avianCommit = (& git -C $AvianRoot rev-parse HEAD).Trim()
$linkManagerImage = (& docker image inspect arc-link-manager:dev --format '{{.Id}}').Trim()
[pscustomobject]@{
    CapturedAt = (Get-Date).ToString('o')
    ArcCommit = $arcCommit
    AvianCommit = $avianCommit
    AvianPluginSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $avianPlugin).Hash
    AvianPluginProcessId = $avianProcess.Id
    LinkManagerImageId = $linkManagerImage
    ComposeFileSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $composeFile).Hash
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $discoveryDirectory "bench-manifest-$discoveryStamp.json")
Write-Host 'AVIAN is watching the Windows neighbor table and publishing real TW-950 discoveries to ARC.' -ForegroundColor Green

Write-Section 'Restart the ARC UI'
$listener = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
if ($listener) {
    $uiProcess = Get-CimInstance Win32_Process -Filter "ProcessId = $($listener.OwningProcess)"
    if ($uiProcess.CommandLine -notlike "*$uiRoot*") {
        throw "Port 3000 is owned by an unexpected process: $($uiProcess.CommandLine)"
    }
    Stop-Process -Id $listener.OwningProcess -Force
    Wait-Process -Id $listener.OwningProcess -ErrorAction SilentlyContinue
}

$logDirectory = Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\application-logs'
New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$outLog = Join-Path $logDirectory "arc-ui-$stamp.out.log"
$errLog = Join-Path $logDirectory "arc-ui-$stamp.err.log"
Start-Process -FilePath 'npm.cmd' `
    -ArgumentList @('run', 'dev', '--', '--host', '0.0.0.0', '--port', '3000') `
    -WorkingDirectory $uiRoot `
    -WindowStyle Hidden `
    -RedirectStandardOutput $outLog `
    -RedirectStandardError $errLog

$deadline = (Get-Date).AddSeconds(60)
do {
    Start-Sleep -Milliseconds 500
    $listener = Get-NetTCPConnection -LocalPort 3000 -State Listen -ErrorAction SilentlyContinue
} until ($listener -or (Get-Date) -gt $deadline)
if (-not $listener) {
    throw "ARC UI did not start. Inspect '$outLog' and '$errLog'."
}
Write-Host "ARC UI is listening at $ArcUrl" -ForegroundColor Green

Write-Section 'Open the device page in Google Chrome'
$chromeCandidates = @(
    (Join-Path $env:ProgramFiles 'Google\Chrome\Application\chrome.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Google\Chrome\Application\chrome.exe'),
    (Join-Path $env:LOCALAPPDATA 'Google\Chrome\Application\chrome.exe')
)
$chrome = $chromeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $chrome) { throw 'Google Chrome is not installed in a standard location.' }
Start-Process -FilePath $chrome -ArgumentList @('--new-window', $ArcUrl)
Write-Host 'Chrome was opened to the ARC Devices page.' -ForegroundColor Green
Write-Host 'If Chrome displays a local-certificate page, choose Advanced and continue to localhost.' -ForegroundColor Yellow

Write-Section 'Start the connection monitor'
$monitor = Join-Path $PSScriptRoot 'Monitor-RadioConnection.ps1'
& $monitor
