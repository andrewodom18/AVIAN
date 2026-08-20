[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$requiredModules = @(
    @{ Name = 'PSScriptAnalyzer'; Version = '1.25.0' },
    @{ Name = 'Pester'; Version = '6.1.0' }
)

foreach ($module in $requiredModules) {
    $available = Get-Module -ListAvailable -Name $module.Name |
        Where-Object Version -eq ([version]$module.Version) |
        Select-Object -First 1
    if (-not $available) {
        Install-Module -Name $module.Name -RequiredVersion $module.Version -Repository PSGallery -Scope CurrentUser -Force
    }
    Import-Module -Name $module.Name -RequiredVersion $module.Version -Force
}

$benchRoot = Join-Path $PSScriptRoot '..\radio-bench'
$parseErrors = @()
Get-ChildItem -LiteralPath $benchRoot -Filter '*.ps1' -File | ForEach-Object {
    $tokens = $null
    $errors = $null
    [System.Management.Automation.Language.Parser]::ParseFile(
        $_.FullName,
        [ref]$tokens,
        [ref]$errors
    ) | Out-Null
    $parseErrors += $errors
}
if ($parseErrors.Count -gt 0) {
    $parseErrors | Format-List
    throw "$($parseErrors.Count) PowerShell parse error(s) found."
}

$analysis = @(Invoke-ScriptAnalyzer -Path $benchRoot -Recurse -Severity Error)
if ($analysis.Count -gt 0) {
    $analysis | Format-Table -AutoSize
    throw "$($analysis.Count) PSScriptAnalyzer error(s) found."
}

$pesterResult = Invoke-Pester -Path (Join-Path $benchRoot 'tests') -PassThru
if ($pesterResult.FailedCount -gt 0) {
    throw "$($pesterResult.FailedCount) Pester test(s) failed."
}
