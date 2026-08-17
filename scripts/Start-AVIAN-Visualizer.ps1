[CmdletBinding()]
param(
    [int]$Port = 3211
)

$repoRoot = Split-Path -Parent $PSScriptRoot
$server = Join-Path $repoRoot 'apps\mesh-visualizer\server.mjs'
$logRoot = Join-Path $repoRoot 'target\mesh-visualizer'
$baseUrl = "http://127.0.0.1:$Port"

New-Item -ItemType Directory -Force -Path $logRoot | Out-Null

try {
    $health = Invoke-RestMethod -TimeoutSec 2 -Uri "$baseUrl/api/health"
} catch {
    $health = $null
}

if (-not $health.ok) {
    $node = (Get-Command node -ErrorAction Stop).Source
    $environment = @{ AVIAN_VISUALIZER_PORT = "$Port" }
    Start-Process `
        -FilePath $node `
        -ArgumentList @($server) `
        -WorkingDirectory $repoRoot `
        -WindowStyle Hidden `
        -Environment $environment `
        -RedirectStandardOutput (Join-Path $logRoot 'server-output.log') `
        -RedirectStandardError (Join-Path $logRoot 'server-error.log')

    $ready = $false
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        Start-Sleep -Milliseconds 500
        try {
            $health = Invoke-RestMethod -TimeoutSec 2 -Uri "$baseUrl/api/health"
            if ($health.ok) {
                $ready = $true
                break
            }
        } catch {
            # Rust may still be compiling the simulator trace.
        }
    }
    if (-not $ready) {
        throw "AVIAN visualizer did not become ready. Check $logRoot."
    }
}

Start-Process $baseUrl

