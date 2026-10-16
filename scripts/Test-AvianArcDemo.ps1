# Offline helper tests; never starts services or kills a real process.
$ErrorActionPreference = 'Stop'
foreach ($name in @('Start-AvianArcDemo.ps1','Stop-AvianArcDemo.ps1','Test-AvianArcDemo.ps1')) {
    $tokens = $null; $errors = $null
    [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $name),[ref]$tokens,[ref]$errors)
    if ($errors.Count) { throw ($errors | Out-String) }
}
. "$PSScriptRoot\Start-AvianArcDemo.ps1" -LibraryOnly
$script:passed = 3
function Assert-Demo($Condition, $Name) {
    if (-not $Condition) { throw "FAIL: $Name" }
    $script:passed++
}
$current = Get-Process -Id $PID
$record = [pscustomobject]@{ name='test'; pid=$PID; startTicks=$current.StartTime.ToUniversalTime().Ticks.ToString(); path=$current.Path }
Assert-Demo (Test-DemoProcess $record) 'matching owner identity'
$record.startTicks = '0'
Assert-Demo (-not (Test-DemoProcess $record)) 'PID reuse rejected'
$record.startTicks = $current.StartTime.ToUniversalTime().Ticks.ToString()
$record.path = 'C:\not-the-process.exe'
Assert-Demo (-not (Test-DemoProcess $record)) 'wrong executable rejected'
# Verify cleanup order with a mock; do not invoke the real Stop-Process.
function Test-DemoProcess($Record) { return $Record.pid -ne 0 }
$script:stopped = @()
function Stop-Process { param([int]$Id,[string]$ErrorAction) $script:stopped += $Id }
Stop-DemoProcesses @()
Assert-Demo ($script:stopped.Count -eq 0) 'empty cleanup'
Stop-DemoProcesses @([pscustomobject]@{name='one';pid=1},[pscustomobject]@{name='not-owned';pid=0},[pscustomobject]@{name='two';pid=2})
Assert-Demo (($script:stopped -join ',') -eq '2,1') 'reverse order and unowned process preserved'
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('avian-demo-selftest-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
$testManifest = Join-Path $temporaryRoot 'active.json'
Save-DemoManifest ([pscustomobject]@{status='starting';processes=@()}) $testManifest
Save-DemoManifest ([pscustomobject]@{status='ready';processes=@()}) $testManifest
Assert-Demo ((Get-Content -LiteralPath $testManifest -Raw | ConvertFrom-Json).status -eq 'ready') 'atomic manifest replacement'
Assert-Demo ((Get-Content -LiteralPath "$testManifest.previous" -Raw | ConvertFrom-Json).status -eq 'starting') 'previous manifest retained'
Write-Host "$script:passed checks passed. Test evidence: $temporaryRoot"
