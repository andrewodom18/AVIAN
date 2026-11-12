[CmdletBinding()]
param(
    [ValidateRange(1, [long]::MaxValue)]
    [long]$Seed = 20260825,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $downloads = [Environment]::GetFolderPath('UserProfile')
    $OutputDirectory = Join-Path $downloads 'Downloads\AVIAN-validation'
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
$resolvedWorkspace = [System.IO.Path]::GetFullPath($workspace)
if ($resolvedOutput.StartsWith($resolvedWorkspace, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Validation artifacts must be written outside the repository.'
}

New-Item -ItemType Directory -Force -Path $resolvedOutput | Out-Null
$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$report = Join-Path $resolvedOutput "mesh-validation-$timestamp.json"
$meshReport = Join-Path $resolvedOutput "transport-validation-$timestamp.json"

Push-Location $workspace
try {
    cargo run --quiet -p mesh-sim -- --validate --seed $Seed --output $report
    if ($LASTEXITCODE -ne 0) { throw "mesh validation failed with exit code $LASTEXITCODE" }
    $meshEvidence = cargo run --quiet -p mesh-sim -- --validate-mesh
    if ($LASTEXITCODE -ne 0) { throw "AVIAN validation failed with exit code $LASTEXITCODE" }
    [System.IO.File]::WriteAllText($meshReport, ($meshEvidence -join [Environment]::NewLine) + [Environment]::NewLine)
    just sim-contract
    if ($LASTEXITCODE -ne 0) { throw "contract validation failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

Write-Host "AVIAN validation passed. Reports: $report and $meshReport"
