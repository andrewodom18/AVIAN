[CmdletBinding()]
param([string]$StateRoot = "$env:USERPROFILE\Desktop\AVIAN Demo Runtime")
$requestedStateRoot = $StateRoot
. "$PSScriptRoot\Start-AvianArcDemo.ps1" -LibraryOnly
$manifestPath = Join-Path $requestedStateRoot 'active.json'
if (-not (Test-Path -LiteralPath $manifestPath)) { Write-Host 'No recorded demo to stop.'; return }
$lock = [IO.File]::Open((Join-Path $requestedStateRoot 'launcher.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
try {
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if (@($manifest.processes).Count -gt 0) { Stop-DemoProcesses $manifest.processes }
    Remove-DemoBuild $manifest
    $manifest.status = 'stopped'
    Save-DemoManifest $manifest $manifestPath
    Write-Host "Demo stopped and disposable builds removed. Logs retained at $($manifest.logs). Other apps and browser tabs were left alone."
} finally { $lock.Dispose() }
