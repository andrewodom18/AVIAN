#Requires -Version 5.1
[CmdletBinding()]
param([switch]$Execute)
$ErrorActionPreference='Stop'
$desktop=[Environment]::GetFolderPath('Desktop')
$repos=@('AVIAN','AVIAN-arc-main-compat','AVIAN-arc-radio-plugin','AVIAN-pipeline-hardening','AVIAN-simulator-validation','AVIAN-visual-simulation','arc-uas-main-20260827','arc-edge\arc-uas','arc-ui-avian-guided-demo')
$relativeOutputs=@('target','dist','services\dev-bridge\target','services\arc-ui\dist','services\arc-ui\node_modules\.vite','services\arc-ui\node_modules\.tmp','node_modules\.vite','node_modules\.tmp')
$results=[Collections.Generic.List[object]]::new()
foreach($repo in $repos){
    $root=[IO.Path]::GetFullPath((Join-Path $desktop $repo))
    foreach($relative in $relativeOutputs){
        $path=[IO.Path]::GetFullPath((Join-Path $root $relative))
        if(-not(Test-Path -LiteralPath $path -PathType Container)){continue}
        try{
            if(-not $path.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Output escaped repository root.'}
            # Inspect every ancestor BEFORE recursion; never follow junctions out of scope.
            $walk=Get-Item -LiteralPath $path -Force
            while($walk -and $walk.FullName.Length -ge $root.Length){
                if($walk.Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'Reparse point found; not deleting.'}
                $walk=$walk.Parent
            }
            $gitRoot=(& git -C (Split-Path $path -Parent) rev-parse --show-toplevel 2>$null)
            if($LASTEXITCODE -ne 0){throw 'No Git repository; output ownership cannot be verified.'}
            $gitRoot=[IO.Path]::GetFullPath([string]$gitRoot)
            $gitRelative=$path.Substring($gitRoot.Length+1).Replace('\','/')
            $tracked=@(& git -C $gitRoot ls-files -- $gitRelative)
            if($LASTEXITCODE -ne 0 -or $tracked.Count){throw 'Tracked files or failed Git check; preserving directory.'}
            & git -C $gitRoot check-ignore -q -- $gitRelative
            if($LASTEXITCODE -ne 0){throw 'Output is not Git-ignored; preserving directory.'}
            $items=@(Get-ChildItem -LiteralPath $path -Recurse -Force -ErrorAction Stop)
            if(@($items | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count){throw 'Nested reparse point found; preserving directory.'}
            $bytes=($items | Where-Object {-not $_.PSIsContainer} | Measure-Object Length -Sum).Sum
            if($Execute){Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop}
            $results.Add([pscustomobject]@{path=$path;bytes=$bytes;status=$(if($Execute){'removed'}else{'planned'})})
        }catch{$results.Add([pscustomobject]@{path=$path;bytes=0;status='preserved-or-partial';reason=$_.Exception.Message})}
    }
}
$out=Join-Path $desktop 'Radio Test Results\build-cleanup'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$report=Join-Path $out ((Get-Date -Format 'yyyyMMdd-HHmmss')+'.json')
@($results.ToArray()) | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $report -Encoding UTF8
$results | Select-Object path,status,@{n='GiB';e={[math]::Round($_.bytes/1GB,2)}},reason | Format-Table -Wrap
Write-Host "Report: $report"
