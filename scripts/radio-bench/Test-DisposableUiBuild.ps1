# Validate the production UI's disposable-config path without launching any app.
$ErrorActionPreference='Stop'
. "$PSScriptRoot\TestSessionLifecycle.ps1"
$ui=Join-Path $env:USERPROFILE 'Desktop\arc-uas-main-20260827\services\arc-ui'
$parent=Join-Path $env:LOCALAPPDATA 'AVIAN-Test-Builds'
$id='test-'+(Get-Date -Format 'yyyyMMdd-HHmmss')+'-'+[guid]::NewGuid().ToString('N').Substring(0,6)
$build=Assert-SessionBuildPath (Join-Path $parent $id) $parent
New-Item -ItemType Directory -Force -Path $build | Out-Null
$source=([uri](Join-Path $ui 'vite.config.ts')).AbsoluteUri | ConvertTo-Json -Compress
$cache=(Join-Path $build 'vite-cache') | ConvertTo-Json -Compress
$config=Join-Path $build 'vite-session.config.mjs'
"import original from $source; export default env => ({...original(env), cacheDir: $cache});" | Set-Content -LiteralPath $config -Encoding UTF8
try{
    Push-Location $ui
    try{& npm run build -- --outDir "$build\ui-dist" --config $config;if($LASTEXITCODE -ne 0){throw 'Disposable UI build failed.'}}finally{Pop-Location}
    if(-not(Test-Path -LiteralPath "$build\ui-dist\index.html")){throw 'UI output missing.'}
    Write-Host 'PASS: fresh production UI built in owned session directory.'
}finally{Remove-TestSessionBuild $build $parent}
if(Test-Path -LiteralPath $build){throw 'Build directory survived cleanup.'}
Write-Host 'PASS: disposable UI build removed; source and results retained.'
