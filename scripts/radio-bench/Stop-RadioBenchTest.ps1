[CmdletBinding()]
param(
    [string]$LinkManagerName = 'arc-avian-real-link-manager',
    [string]$DiscoveryDirectory = (Join-Path $env:USERPROFILE 'Desktop\Radio Test Results\live-discovery')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$latestManifest = Get-ChildItem -LiteralPath $DiscoveryDirectory -Filter 'bench-manifest-*.json' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if ($latestManifest) {
    $manifest = Get-Content -LiteralPath $latestManifest.FullName -Raw | ConvertFrom-Json
    $process = Get-Process -Id $manifest.AvianPluginProcessId -ErrorAction SilentlyContinue
    if ($process -and $process.ProcessName -eq 'arc-radio-plugin') {
        Stop-Process -Id $process.Id
        Write-Host "Stopped AVIAN discovery process $($process.Id)."
    }
}

$container = & docker ps -a --filter "name=^/$LinkManagerName$" --format '{{.Names}}'
if ($container -eq $LinkManagerName) {
    & docker stop $LinkManagerName | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Failed to stop $LinkManagerName." }
    Write-Host "Stopped $LinkManagerName. The --rm container was removed by Docker."
}
