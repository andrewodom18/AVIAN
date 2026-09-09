#Requires -Version 5.1
[CmdletBinding()]
param([switch]$PreflightOnly,[switch]$PromptForChudApiKey,[string]$EthernetAdapter='Ethernet 2',[int]$ObserveSeconds=30)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'TestSessionLifecycle.ps1')
$arc=Join-Path $env:USERPROFILE 'Desktop\arc-uas-main-20260827'
$avian=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$ui=Join-Path $arc 'services\arc-ui'
if($PreflightOnly){& "$PSScriptRoot\Start-Tw950LiveValidation.ps1" -PreflightOnly -PromptForChudApiKey:$PromptForChudApiKey -EthernetAdapter $EthernetAdapter;return}
Write-Host 'Fresh REAL bench build -> guided live test -> stop services and remove this run''s builds.' -ForegroundColor Cyan
Write-Host 'Keep both TW950s powered OFF until testing prompts. Source, certs, Fleet data and results are preserved.'
$answer=(Read-Host 'Type BUILD to start (internet needed while building)').Trim()
if($answer -ine 'BUILD'){return}
$id='test-'+(Get-Date -Format 'yyyyMMdd-HHmmss')+'-'+[guid]::NewGuid().ToString('N').Substring(0,6)
$parent=Join-Path $env:LOCALAPPDATA 'AVIAN-Test-Builds'
$build=Assert-SessionBuildPath (Join-Path $parent $id) $parent
$evidence=Join-Path $env:USERPROFILE "Desktop\Radio Test Results\test-sessions\$id"
New-Item -ItemType Directory -Force -Path $evidence,$build | Out-Null
$manifestPath=Join-Path $evidence 'session.json'
$m=[pscustomobject]@{id=$id;buildParent=$parent;buildRoot=$build;status='preparing';failure='';builder='';images=@();processes=@();startedChud=$false;cleanup_errors=@()}
Save-TestSession $m $manifestPath
$lock=$null
function Run-Build([string]$Name,[scriptblock]$Command){
    $old=$ErrorActionPreference
    try{$ErrorActionPreference='Continue';& $Command *> (Join-Path $evidence "$Name.log");$code=$LASTEXITCODE}finally{$ErrorActionPreference=$old}
    if($code -ne 0){throw "$Name failed (exit $code). See $evidence\$Name.log"}
}
function Start-Owned([string]$Name,[string]$Exe,[string[]]$Arguments,[string]$WorkingDirectory){
    $p=Start-Process -FilePath $Exe -ArgumentList $Arguments -WorkingDirectory $WorkingDirectory -WindowStyle Hidden -PassThru -RedirectStandardOutput "$evidence\$Name.stdout.log" -RedirectStandardError "$evidence\$Name.stderr.log"
    $m.processes+= [pscustomobject]@{pid=$p.Id;path=[IO.Path]::GetFullPath($Exe);startTicks=$p.StartTime.ToUniversalTime().Ticks.ToString()}
    Save-TestSession $m $manifestPath
}
$envNames=@('BROWSER','VITE_BRIDGE_URL','VITE_ARC_WS_URL','VITE_DEMO_MODE','VITE_DEV_HTTP')
$oldEnv=@{};foreach($key in $envNames){$oldEnv[$key]=[Environment]::GetEnvironmentVariable($key,'Process')}
try{
    $lock=[IO.File]::Open((Join-Path $parent 'live-session.lock'),'OpenOrCreate','ReadWrite','None')
    $old=$ErrorActionPreference;try{$ErrorActionPreference='Continue';docker info *> $null;$ready=$LASTEXITCODE -eq 0}finally{$ErrorActionPreference=$old}
    if(-not $ready){
        Start-Process -FilePath (Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe') -WindowStyle Hidden
        $deadline=[DateTime]::UtcNow.AddMinutes(2)
        do {Start-Sleep -Seconds 3;$old=$ErrorActionPreference;try{$ErrorActionPreference='Continue';docker info *> $null;$ready=$LASTEXITCODE -eq 0}finally{$ErrorActionPreference=$old}}until($ready -or [DateTime]::UtcNow -gt $deadline)
        if(-not $ready){throw 'Docker did not become ready.'}
    }
    if(@(docker ps -aq --filter 'label=com.docker.compose.project=arc-avian-local').Count -or @(docker ps -aq --filter 'name=^/arc-avian-real-link-manager$').Count){throw 'An existing bench stack is present. Clean that session first; this run will not adopt or delete it.'}
    foreach($port in @(3000,9100,9101,7447)){if(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue){throw "Port $port is occupied. Close the old test apps, using the same privilege level that launched them."}}
    if(@(Get-Process arc-radio-plugin -ErrorAction SilentlyContinue).Count){throw 'An old AVIAN watcher is running. Close it before building a new session.'}
    $chud=docker inspect chud-local | ConvertFrom-Json
    if($LASTEXITCODE -ne 0){throw 'Existing CHUD installation missing. This test never invents a CHUD installation.'}
    if(@($chud.Mounts | Where-Object {$_.Type -eq 'bind' -and -not(Test-Path -LiteralPath $_.Source)}).Count){throw 'A CHUD mount is missing. Certificates and configuration were not altered.'}
    $m.builder="avian-$id"
    Save-TestSession $m $manifestPath
    docker buildx create --name $m.builder --driver docker-container | Out-Null
    if($LASTEXITCODE -ne 0){throw 'Dedicated Docker builder creation failed.'}
    $services=@('comms','dev-bridge','flight-recorder','landing-advisor')
    $lines=@('services:')
    foreach($service in $services){$image="avian-test/$id/$service`:local";$m.images+=$image;$lines+=@("  $service`:","    image: $image")}
    $lmImage="avian-test/$id/link-manager:local";$m.images+=$lmImage
    $override=Join-Path $build 'session-images.yml';$lines | Set-Content -LiteralPath $override -Encoding UTF8
    Save-TestSession $m $manifestPath
    $compose=@('compose','--project-name','arc-avian-local','--file',"$arc\infra\dev\docker-compose.yml",'--file',"$PSScriptRoot\docker-compose.real-hardware.yml",'--file',$override)
    Write-Host "Building fresh; progress logs: $evidence" -ForegroundColor Cyan
    Run-Build 'arc-containers-build' {docker @compose build --builder $m.builder @services}
    Run-Build 'link-manager-build' {docker buildx build --builder $m.builder --load --tag $lmImage --file "$arc\services\link-manager\Dockerfile" $arc}
    Run-Build 'avian-build' {cargo build --locked --manifest-path "$avian\Cargo.toml" --target-dir "$build\avian-target" -p arc-radio-plugin}
    $env:BROWSER='none';$env:VITE_BRIDGE_URL='http://127.0.0.1:9101';$env:VITE_ARC_WS_URL='wss://localhost:3000/ws';$env:VITE_DEMO_MODE='0';$env:VITE_DEV_HTTP='0'
    $viteConfig=Join-Path $build 'vite-session.config.mjs'
    $sourceConfig=([uri](Join-Path $ui 'vite.config.ts')).AbsoluteUri | ConvertTo-Json -Compress
    $cachePath=(Join-Path $build 'vite-cache') | ConvertTo-Json -Compress
    "import original from $sourceConfig; export default env => ({...original(env), cacheDir: $cachePath});" | Set-Content -LiteralPath $viteConfig -Encoding UTF8
    Push-Location $ui
    try{
        Run-Build 'ui-regression' {npm run test:vitest -- src/components/Devices/RadioNetworkConsole.test.ts src/components/Devices/RadioConfigurationPanel.test.ts}
        Run-Build 'ui-build' {npm run build -- --outDir "$build\ui-dist" --config $viteConfig}
    }finally{Pop-Location}
    docker @compose up --detach --no-build @services | Out-Null
    if($LASTEXITCODE -ne 0){throw 'ARC startup failed.'}
    if(-not $chud.State.Running){$m.startedChud=$true;Save-TestSession $m $manifestPath;docker start chud-local | Out-Null;if($LASTEXITCODE -ne 0){throw 'CHUD startup failed.'}}
    docker run --detach --name arc-avian-real-link-manager --volume 'arc-avian-local_arc-ipc:/run/arc' $lmImage --device-id "$env:COMPUTERNAME-radio-bench" | Out-Null
    if($LASTEXITCODE -ne 0){throw 'Link manager startup failed.'}
    $discovery=Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\live-discovery';New-Item -ItemType Directory -Force -Path $discovery | Out-Null
    Start-Owned 'avian-discovery' "$build\avian-target\debug\arc-radio-plugin.exe" @('trellisware-discover','--probe-ip','10.1.0.2','--watch','--interval-seconds','2','--zenoh-endpoint','tcp/127.0.0.1:7447','--output',"`"$discovery\latest-discovery.json`"") $avian
    Start-Owned 'arc-ui' (Get-Command node).Source @("`"$ui\node_modules\vite\bin\vite.js`"",'preview','--host','127.0.0.1','--port','3000','--strictPort','--outDir',"`"$build\ui-dist`"",'--config',"`"$viteConfig`"") $ui
    $deadline=[DateTime]::UtcNow.AddSeconds(90);$healthy=$false
    do{
        try{$h=Invoke-RestMethod 'http://127.0.0.1:9101/api/radio/streamcaster/health' -TimeoutSec 3;$healthy=$h.coordination_reachable -eq $true}catch{}
        if(-not $healthy){Start-Sleep -Seconds 2}
    }until($healthy -or [DateTime]::UtcNow -gt $deadline)
    if(-not $healthy){throw 'Radio coordination did not become ready.'}
    $m.status='testing';Save-TestSession $m $manifestPath
    Write-Host 'Open https://localhost:3000/home/devices in your existing browser. No browser was opened automatically.'
    & "$PSScriptRoot\Start-Tw950LiveValidation.ps1" -PromptForChudApiKey:$PromptForChudApiKey -EthernetAdapter $EthernetAdapter -ObserveSeconds $ObserveSeconds -UiRegression
}catch{$m.failure=$_.Exception.Message;Save-TestSession $m $manifestPath;Write-Warning $_.Exception.Message}
finally{
    foreach($key in $envNames){[Environment]::SetEnvironmentVariable($key,$oldEnv[$key],'Process')}
    try{Stop-TestSession $manifestPath;Write-Host 'Test services stopped; owned build outputs, images and dedicated cache removed. Results retained.' -ForegroundColor Green}
    catch{Write-Warning "Cleanup needs attention: $($_.Exception.Message). Run Stop-AVIAN-Test-Session.ps1 -ManifestPath '$manifestPath'"}
    if($lock){$lock.Dispose()}
    Write-Host "Session manifest and build logs: $evidence"
}
