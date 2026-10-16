# Shared, ownership-scoped helpers. Dot-source only.
function Assert-SessionBuildPath([string]$Path,[string]$Parent) {
    $full=[IO.Path]::GetFullPath($Path).TrimEnd('\')
    $base=[IO.Path]::GetFullPath($Parent).TrimEnd('\')
    if(-not $full.StartsWith($base+'\',[StringComparison]::OrdinalIgnoreCase) -or
       (Split-Path $full -Leaf) -notmatch '^test-[0-9]{8}-[0-9]{6}-[a-f0-9]{6}$'){throw 'Invalid owned test-build directory.'}
    $walk=$full
    while($walk -and $walk.Length -ge $base.Length){
        if(Test-Path -LiteralPath $walk){if((Get-Item -LiteralPath $walk -Force).Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Session path contains a reparse point.'}}
        $walk=Split-Path $walk -Parent
    }
    return $full
}
function Save-TestSession($Manifest,[string]$Path){
    $Manifest | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $Path -Encoding UTF8
}
function Remove-TestSessionBuild([string]$Path,[string]$Parent){
    $full=Assert-SessionBuildPath $Path $Parent
    if(Test-Path -LiteralPath $full){
        $links=@(Get-ChildItem -LiteralPath $full -Recurse -Force | Where-Object {$_.Attributes -band [IO.FileAttributes]::ReparsePoint})
        if($links.Count){throw 'Session contains a reparse point; build retained for review.'}
        Remove-Item -LiteralPath $full -Recurse -Force -ErrorAction Stop
    }
}
function Stop-OwnedTestProcess($Record){
    $p=Get-Process -Id $Record.pid -ErrorAction SilentlyContinue
    if(-not $p){return}
    if(-not $p.Path){throw "Cannot verify PID $($Record.pid); rerun cleanup from the same privilege level as the test."}
    if($p.Path -ne $Record.path -or $p.StartTime.ToUniversalTime().Ticks.ToString() -ne $Record.startTicks){throw 'PID was reused; refusing to stop another process.'}
    Stop-Process -Id $p.Id -ErrorAction Stop
    Wait-Process -Id $p.Id -Timeout 15 -ErrorAction SilentlyContinue
}
function Stop-TestSession([string]$ManifestPath){
    $m=Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    $expectedParent=Join-Path $env:LOCALAPPDATA 'AVIAN-Test-Builds'
    if($m.id -notmatch '^test-[0-9]{8}-[0-9]{6}-[a-f0-9]{6}$' -or
       [IO.Path]::GetFullPath($m.buildParent) -ne [IO.Path]::GetFullPath($expectedParent) -or
       [IO.Path]::GetFullPath($m.buildRoot) -ne (Join-Path $expectedParent $m.id) -or
       ($m.builder -and $m.builder -ne "avian-$($m.id)")){throw 'Manifest ownership validation failed.'}
    $problems=[Collections.Generic.List[string]]::new()
    foreach($p in @($m.processes)) {try{Stop-OwnedTestProcess $p}catch{$problems.Add($_.Exception.Message)}}
    if($m.builder){
      try {
        $names=@(& docker ps -aq --filter 'label=com.docker.compose.project=arc-avian-local')
        $extra=@(& docker ps -aq --filter 'name=^/arc-avian-real-link-manager$')
        foreach($id in @($names+$extra | Select-Object -Unique)){
            if(-not $id){continue}
            $c=& docker inspect $id | ConvertFrom-Json
            if($c.Config.Image.StartsWith("avian-test/$($m.id)/")){
                & docker rm -f $id | Out-Null
                if($LASTEXITCODE -ne 0){$problems.Add("Could not remove owned container $id")}
            }
        }
        if($m.startedChud){
            # Preserve its container, certificate mounts and image. It is not our build.
            & docker stop chud-local | Out-Null
            if($LASTEXITCODE -ne 0){$problems.Add('Could not stop CHUD.')}
        }
        foreach($image in @($m.images)){
            if(-not $image.StartsWith("avian-test/$($m.id)/")){$problems.Add('Image ownership mismatch.');continue}
            $existing=@(& docker image ls -q $image)
            if($existing.Count){& docker image rm $image | Out-Null;if($LASTEXITCODE -ne 0){$problems.Add("Could not remove $image")}}
        }
        $builders=@(& docker buildx ls --format '{{.Name}}')
        if($LASTEXITCODE -ne 0){$problems.Add('Could not list dedicated builders.')}
        elseif($m.builder -in $builders){
            & docker buildx rm --force $m.builder | Out-Null
            if($LASTEXITCODE -ne 0){$problems.Add('Dedicated builder cleanup failed; retry cleanup command.')}
        }
      } catch { $problems.Add("Docker cleanup could not complete: $($_.Exception.Message)") }
    }
    if(-not $problems.Count){try{Remove-TestSessionBuild $m.buildRoot $m.buildParent}catch{$problems.Add($_.Exception.Message)}}
    $m.cleanup_errors=@($problems.ToArray())
    $m.status=if($problems.Count){'cleanup-required'}else{'cleaned'}
    Save-TestSession $m $ManifestPath
    if($problems.Count){throw ($problems -join '; ')}
}
