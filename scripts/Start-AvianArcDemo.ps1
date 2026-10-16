# Standalone, local-only demo launcher. Compatible with Windows PowerShell 5.1.
[CmdletBinding()]
param(
    [string]$ArcRoot = "$env:USERPROFILE\Desktop\arc-uas-main-20260827",
    [string]$StateRoot = "$env:USERPROFILE\Desktop\AVIAN Demo Runtime",
    [ValidateRange(1024,65535)][int]$UiPort = 13000,
    [ValidateRange(1024,65535)][int]$BridgePort = 19101,
    [ValidateRange(1024,65535)][int]$TcpPort = 19100,
    [ValidateRange(1024,65535)][int]$VisualizerPort = 13211,
    [switch]$OpenBrowser,
    [switch]$GuidedSession,
    [switch]$CheckOnly,
    [switch]$LibraryOnly
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-DemoProcess($Record) {
    $candidate = Get-Process -Id $Record.pid -ErrorAction SilentlyContinue
    if (-not $candidate) { return $false }
    try {
        return ($candidate.StartTime.ToUniversalTime().Ticks.ToString() -eq $Record.startTicks -and
            $candidate.Path -eq $Record.path)
    } catch { return $false }
}

function Stop-DemoProcesses($Records) {
    $reverse = @($Records)
    [array]::Reverse($reverse)
    foreach ($record in $reverse) {
        if ($null -ne $record -and (Test-DemoProcess $record)) {
            Stop-Process -Id $record.pid -ErrorAction Stop
            Write-Host "Stopped $($record.name) (PID $($record.pid))."
        }
    }
}

function Save-DemoManifest($Value, [string]$Path) {
    $temporary = "$Path.pending"
    $Value | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $temporary -Encoding UTF8
    if (Test-Path -LiteralPath $Path) {
        [IO.File]::Replace($temporary, $Path, "$Path.previous")
    } else { [IO.File]::Move($temporary, $Path) }
}

function Remove-DemoBuild($Manifest) {
    if(-not $Manifest.PSObject.Properties['buildRoot']){return}
    $parent=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'AVIAN-Test-Builds')).TrimEnd('\')
    $path=[IO.Path]::GetFullPath($Manifest.buildRoot).TrimEnd('\')
    if(-not $path.StartsWith($parent+'\',[StringComparison]::OrdinalIgnoreCase) -or (Split-Path $path -Leaf) -notmatch '^test-[0-9]{8}-[0-9]{6}-[a-f0-9]{6}$'){throw 'Invalid demo build ownership.'}
    $walk=$path
    while($walk -and $walk.Length -ge $parent.Length){
        if(Test-Path -LiteralPath $walk){if((Get-Item -LiteralPath $walk -Force).Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Demo build path contains a reparse point.'}}
        $walk=Split-Path $walk -Parent
    }
    if(Test-Path -LiteralPath $path){
        if(@(Get-ChildItem -LiteralPath $path -Recurse -Force | Where-Object {$_.Attributes -band [IO.FileAttributes]::ReparsePoint}).Count){throw 'Demo build contains a reparse point.'}
        Remove-Item -LiteralPath $path -Recurse -Force
    }
}

function Get-DemoJson([string]$Url) {
    # Explicitly bypass corporate proxies for these loopback-only requests.
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object System.Net.Http.HttpClientHandler
    $handler.UseProxy = $false
    $handler.AllowAutoRedirect = $false
    $client = New-Object System.Net.Http.HttpClient($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(3)
    try {
        $response = $client.GetAsync($Url).GetAwaiter().GetResult()
        try {
            $response.EnsureSuccessStatusCode() | Out-Null
            return ($response.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json)
        } finally { $response.Dispose() }
    } finally { $client.Dispose(); $handler.Dispose() }
}

function Wait-DemoJson([string]$Url, $ProcessRecord, [scriptblock]$Accept) {
    $deadline = [DateTime]::UtcNow.AddSeconds(100)
    do {
        if (-not (Test-DemoProcess $ProcessRecord)) { throw "$($ProcessRecord.name) exited. See $runDirectory." }
        try {
            $value = Get-DemoJson $Url
            if (& $Accept $value) { return }
        } catch { }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Not ready: $Url. See $runDirectory."
}

function Start-DemoProcess([string]$Name, [string]$Executable, [string[]]$Arguments, [string]$Directory, [hashtable]$Environment) {
    $previous = @{}
    try {
        foreach ($key in $Environment.Keys) {
            $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, $Environment[$key], 'Process')
        }
        $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -WorkingDirectory $Directory `
            -WindowStyle Hidden -PassThru -RedirectStandardOutput "$runDirectory\$Name.stdout.log" `
            -RedirectStandardError "$runDirectory\$Name.stderr.log"
        # Start-Process may expose a null Path before a newly spawned process settles.
        $record = [pscustomobject]@{ name=$Name; pid=$process.Id; path=[IO.Path]::GetFullPath($Executable);
            startTicks=$process.StartTime.ToUniversalTime().Ticks.ToString() }
        $script:demoManifest.processes = @($script:demoManifest.processes) + $record
        Save-DemoManifest $script:demoManifest $manifestPath
        return $record
    } finally {
        foreach ($key in $previous.Keys) { [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process') }
    }
}

if ($LibraryOnly) { return }
$avianRoot = Split-Path -Parent $PSScriptRoot
$uiRoot = Join-Path $ArcRoot 'services\arc-ui'
$bridgeManifest = Join-Path $ArcRoot 'services\dev-bridge\Cargo.toml'
$vite = Join-Path $uiRoot 'node_modules\vite\bin\vite.js'
$visualizer = Join-Path $avianRoot 'simulators\mesh-operations\visualizer\server.mjs'
$manifestPath = Join-Path $StateRoot 'active.json'
$ports = @($UiPort,$BridgePort,$TcpPort,$VisualizerPort)
if (@($ports | Select-Object -Unique).Count -ne 4) { throw 'Choose four different ports.' }
foreach ($file in @($bridgeManifest,$vite,$visualizer)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing local dependency: $file. No downloads were attempted." }
}
$node = (Get-Command node -ErrorAction Stop).Source
$cargo = (Get-Command cargo -ErrorAction Stop).Source
if (Test-Path -LiteralPath $manifestPath) {
    $existing = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if (@($existing.processes | Where-Object { Test-DemoProcess $_ }).Count -gt 0) {
        throw "Demo processes are already running. Use the Desktop Stop-AVIAN-ARC-Demo.ps1 first. Details: $manifestPath"
    }
}
foreach ($port in $ports) {
    $listener = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Loopback, $port)
    try { $listener.Start() } catch { throw "Port $port is occupied. Nothing was stopped; use different demo ports." }
    finally { $listener.Stop() }
}
Write-Host 'Local prerequisites OK. No Docker or chat connection is required.' -ForegroundColor Green
if ($CheckOnly) { return }
New-Item -ItemType Directory -Force -Path $StateRoot | Out-Null
# Prevent concurrent launch/stop operations from racing over the ownership manifest.
$lock = [IO.File]::Open((Join-Path $StateRoot 'launcher.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
try {
    if (Test-Path -LiteralPath $manifestPath) {
        $existing = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if (@($existing.processes | Where-Object { Test-DemoProcess $_ }).Count -gt 0) { throw 'A demo launch is already active.' }
    }
    $runDirectory = Join-Path $StateRoot ((Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0,6))
    New-Item -ItemType Directory -Path $runDirectory | Out-Null
    $buildRoot=Join-Path $env:LOCALAPPDATA ('AVIAN-Test-Builds\test-'+(Split-Path $runDirectory -Leaf))
    if(Test-Path -LiteralPath $buildRoot){throw 'Fresh demo build directory already exists.'}
    New-Item -ItemType Directory -Path $buildRoot -Force | Out-Null
    $bridge=Join-Path $buildRoot 'arc-target\debug\dev-bridge.exe'
    $sessionViteConfig=Join-Path $buildRoot 'vite-session.config.mjs'
    $configSource=([uri](Join-Path $uiRoot 'vite.config.ts')).AbsoluteUri | ConvertTo-Json -Compress
    $cachePath=(Join-Path $buildRoot 'vite-cache') | ConvertTo-Json -Compress
    "import original from $configSource; export default env => ({...original(env), cacheDir: $cachePath});" | Set-Content -LiteralPath $sessionViteConfig -Encoding UTF8
    $uiUrl = "http://127.0.0.1:$UiPort"
    $bridgeUrl = "http://127.0.0.1:$BridgePort"
    $networkUrl = "http://127.0.0.1:$VisualizerPort"
    $script:demoManifest = [pscustomobject]@{ schema=1; status='starting'; startedUtc=[DateTime]::UtcNow.ToString('o');
        arcUrl="$uiUrl/home/devices"; avianUrl=$networkUrl; logs=$runDirectory; buildRoot=$buildRoot; processes=@() }
    Save-DemoManifest $script:demoManifest $manifestPath
    try {
        Write-Host 'Preparing AVIAN simulation from local dependencies (offline)...'
        Push-Location $avianRoot
        try {
            # PS 5.1 treats native stderr as ErrorRecord even on successful builds.
            $buildPreference = $ErrorActionPreference
            try {
                $ErrorActionPreference = 'Continue'
                & $cargo build --offline --locked --target-dir "$buildRoot\avian-target" -p mesh-sim *> "$runDirectory\simulator-build.log"
            } finally { $ErrorActionPreference = $buildPreference }
            if ($LASTEXITCODE -ne 0) { throw "Offline simulator build failed. See $runDirectory\simulator-build.log" }
        } finally { Pop-Location }
        Write-Host 'Building ARC from current source into the disposable session directory...'
        $buildPreference=$ErrorActionPreference
        try{$ErrorActionPreference='Continue';& $cargo build --offline --locked --manifest-path $bridgeManifest --target-dir "$buildRoot\arc-target" *> "$runDirectory\arc-build.log"}
        finally{$ErrorActionPreference=$buildPreference}
        if($LASTEXITCODE -ne 0){throw "ARC build failed. See $runDirectory\arc-build.log"}
        $backend = Start-DemoProcess 'arc-backend' $bridge @('--mock','--mock-detections','--video','--device-id','SIMULATION-ALPHA',
            '--port',"$TcpPort",'--http-port',"$BridgePort",'--tcp-bind','127.0.0.1','--http-bind','127.0.0.1') $ArcRoot @{
                ARC_DEV_BRIDGE_STATE_DIR="$runDirectory\fleet-state"; ARC_DEV_BRIDGE_TEST_CONTEXT_PATH=$null;
                ARC_RADIO_MUTATIONS_ENABLED='false'; RADIO_MANAGEMENT_API_TOKEN=$null; RADIO_MANAGEMENT_API_KEY_FILE=$null;
                ZENOH_CONFIG=$null; RUST_LOG='warn'; ARC_UI_ALLOWED_ORIGINS=$uiUrl
            }
        Wait-DemoJson "$bridgeUrl/api/health" $backend { param($h) $h.bridge_protocol -eq 'fleet-v2' -and $h.fleet.ok }
        $network = Start-DemoProcess 'avian-visualizer' $node @("`"$visualizer`"") $avianRoot @{
            AVIAN_VISUALIZER_PORT="$VisualizerPort"; CARGO_NET_OFFLINE='true'; CARGO_TARGET_DIR="$buildRoot\avian-target"
        }
        Wait-DemoJson "$networkUrl/api/health" $network { param($h) $h.ok -eq $true }
        $frontend = Start-DemoProcess 'arc-frontend' $node @("`"$vite`"",'--host','127.0.0.1','--port',"$UiPort",'--strictPort','--config',"`"$sessionViteConfig`"") $uiRoot @{
            VITE_DEV_HTTP='1'; VITE_BRIDGE_URL=$bridgeUrl; VITE_ARC_WS_URL="ws://127.0.0.1:$BridgePort/ws";
            VITE_DEMO_MODE='1'; BROWSER='none'
        }
        Wait-DemoJson "$uiUrl/api/health" $frontend { param($h) $h.bridge_protocol -eq 'fleet-v2' -and $h.fleet.ok }
        $script:demoManifest.status = 'ready'
        Save-DemoManifest $script:demoManifest $manifestPath
    } catch {
        $failure = $_
        if (@($script:demoManifest.processes).Count -gt 0) { Stop-DemoProcesses $script:demoManifest.processes }
        Remove-DemoBuild $script:demoManifest
        $script:demoManifest.status = 'failed'
        Save-DemoManifest $script:demoManifest $manifestPath
        throw $failure
    }
    Write-Host "`nREADY - SIMULATION // Dev Environment" -ForegroundColor Green
    Write-Host "ARC Fleet:       $($script:demoManifest.arcUrl)"
    Write-Host "AVIAN network:   $networkUrl"
    Write-Host "Logs:            $runDirectory"
    Write-Host 'These are separate demos. ARC mock radio configuration is not integrated; real radios are not controlled.' -ForegroundColor Yellow
    Write-Host 'Voice/AI features require additional model assets. They are not included in this launch.'
    if ($GuidedSession) {
        Write-Host 'Keep this PowerShell window open and follow the testing prompts. The guide handles its own cleanup.'
    } else {
        Write-Host 'You can close this PowerShell window. Use Stop-AVIAN-ARC-Demo.ps1 to stop these services.'
    }
    if ($OpenBrowser) {
        Start-Process $script:demoManifest.arcUrl
        Start-Process $networkUrl
    }
} finally { $lock.Dispose() }
