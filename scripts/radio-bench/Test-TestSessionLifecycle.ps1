#Requires -Version 5.1
$ErrorActionPreference='Stop'
. "$PSScriptRoot\TestSessionLifecycle.ps1"
$checks=0
function Check([bool]$Value,[string]$Name){if(-not $Value){throw "FAIL: $Name"};$script:checks++;Write-Host "PASS: $Name"}
$parent=Join-Path $env:LOCALAPPDATA 'AVIAN-Test-Builds'
$id='test-20000101-000000-'+[guid]::NewGuid().ToString('N').Substring(0,6)
$path=Join-Path $parent $id
Check ((Assert-SessionBuildPath $path $parent) -eq $path) 'Exact child build path allowed'
foreach($bad in @($parent,(Join-Path $parent '..\outside'),(Join-Path $parent 'source'))){
    $rejected=$false;try{Assert-SessionBuildPath $bad $parent | Out-Null}catch{$rejected=$true}
    Check $rejected 'Broad, escaping, or unowned path rejected'
}
$evidence=Join-Path ([IO.Path]::GetTempPath()) ('avian-lifecycle-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $path,$evidence -Force | Out-Null
'test-build' | Set-Content -LiteralPath (Join-Path $path 'fixture.bin')
'retain-me' | Set-Content -LiteralPath (Join-Path $evidence 'results.txt')
$manifest=Join-Path $evidence 'session.json'
$m=[pscustomobject]@{id=$id;buildParent=$parent;buildRoot=$path;status='starting';builder='';images=@();processes=@();startedChud=$false;cleanup_errors=@()}
Save-TestSession $m $manifest
Stop-TestSession $manifest
Check (-not(Test-Path -LiteralPath $path)) 'Failure before startup removes owned build output'
Check ((Get-Content -LiteralPath (Join-Path $evidence 'results.txt')) -eq 'retain-me') 'Evidence preserved outside build output'
Check ((Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).status -eq 'cleaned') 'Cleanup recorded'
Stop-TestSession $manifest
Check (-not(Test-Path -LiteralPath $path)) 'Cleanup idempotent without builder'
$m.buildParent='C:\';Save-TestSession $m $manifest
$rejected=$false;try{Stop-TestSession $manifest}catch{$rejected=$true}
Check $rejected 'Forged cleanup manifest rejected'
$current=Get-Process -Id $PID
$rejected=$false;try{Stop-OwnedTestProcess ([pscustomobject]@{pid=$PID;path=$current.Path;startTicks='0'})}catch{$rejected=$true}
Check $rejected 'Reused PID never terminated'
# Exercise image/container/cache ownership with a Docker fake, not live Docker.
$m.buildParent=$parent;$m.builder="avian-$id";$m.images=@("avian-test/$id/dev-bridge:local");$m.startedChud=$false
$script:dockerCalls=[Collections.Generic.List[string]]::new()
$script:fixtureSessionId=$id
function docker {
    $line=$args -join ' ';$script:dockerCalls.Add($line);$global:LASTEXITCODE=0
    if($line -like 'ps -aq --filter label=*'){return @('owned','unrelated')}
    if($line -like 'ps -aq*'){return}
    if($line -eq 'inspect owned'){return ('{"Config":{"Image":"avian-test/'+$script:fixtureSessionId+'/dev-bridge:local"}}')}
    if($line -eq 'inspect unrelated'){return '{"Config":{"Image":"someone-else/dev:local"}}'}
    if($line -like 'image ls*'){return 'image-id'}
    if($line -like 'buildx ls*'){return "avian-$script:fixtureSessionId"}
}
New-Item -ItemType Directory -Force -Path $path | Out-Null
Save-TestSession $m $manifest
Stop-TestSession $manifest
Check ($script:dockerCalls.Contains('rm -f owned')) 'Owned container removed'
Check (-not $script:dockerCalls.Contains('rm -f unrelated')) 'Unrelated container preserved'
Check ($script:dockerCalls.Contains("image rm avian-test/$id/dev-bridge:local")) 'Only owned image tag removed'
Check ($script:dockerCalls.Contains("buildx rm --force avian-$id")) 'Dedicated builder removed'
Check (-not @($script:dockerCalls | Where-Object {$_ -match 'prune|volume rm|stop chud'}).Count) 'No global cache/volume prune or independent CHUD stop'
Remove-Item function:docker
foreach($name in @('Invoke-Tw950TestSession.ps1','TestSessionLifecycle.ps1','Clear-AvianTestBuilds.ps1','Start-Tw950LiveValidation.ps1')){
    $tokens=$null;$errors=$null
    [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $name),[ref]$tokens,[ref]$errors)
    Check ($errors.Count -eq 0) "$name syntax"
}
Write-Host "$checks checks passed. No live services or radios contacted. Test evidence: $evidence"
