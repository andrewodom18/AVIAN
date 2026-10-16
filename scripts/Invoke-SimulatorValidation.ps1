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
$peatReport = Join-Path $resolvedOutput "peat-validation-$timestamp.json"

Push-Location $workspace
try {
    cargo run --quiet -p mesh-sim -- --validate --seed $Seed --output $report
    if ($LASTEXITCODE -ne 0) { throw "mesh validation failed with exit code $LASTEXITCODE" }
    $peatEvidence = cargo run --quiet -p mesh-sim -- --validate-peat
    if ($LASTEXITCODE -ne 0) { throw "PEAT validation failed with exit code $LASTEXITCODE" }
    [System.IO.File]::WriteAllText($peatReport, ($peatEvidence -join [Environment]::NewLine) + [Environment]::NewLine)
    node --test --test-concurrency=1 'simulators/mesh-operations/chud-emulator/emulator.test.mjs' 'simulators/mesh-operations/validation-contract.test.mjs'
    if ($LASTEXITCODE -ne 0) { throw "contract validation failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}

Write-Host "AVIAN validation passed. Reports: $report and $peatReport"
