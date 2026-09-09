#Requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$guide = Join-Path $PSScriptRoot 'Start-ArcEmulatorWalkthrough.ps1'
$tokens = $null
$errors = $null
[void][Management.Automation.Language.Parser]::ParseFile($guide,[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
. $guide -LibraryOnly
$testDirectory = Join-Path ([IO.Path]::GetTempPath()) ('avian-guide-selftest-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDirectory | Out-Null
$script:TestToken = 'disposable-test-secret'
$script:Run = [ordered]@{id='selftest';directory=$testDirectory;completed=$false;updated_utc='';verdict='INCOMPLETE';steps=(New-Object System.Collections.ArrayList)}
$passed = 0
function Expect($Actual,$Expected,[string]$Name) {
    if ($Actual -cne $Expected) { throw "$Name expected '$Expected', got '$Actual'." }
    $script:passed++
}
Expect (Convert-Answer 'YES') 'PASS' 'case-insensitive yes'
Expect (Convert-Answer ' n ') 'FAIL' 'trimmed no'
Expect (Convert-Answer 'Unknown') 'BLOCKED' 'unknown is not failure'
Expect (Convert-Answer 'Skip') 'SKIPPED' 'skip is not pass'
Expect (Convert-Answer 'quit') 'QUIT' 'quit recognized'
Expect (Convert-Answer 'maybe') $null 'invalid answer rejected'
Expect (Get-Verdict @() $true) 'INCOMPLETE' 'empty run cannot pass'
Expect (Get-Verdict @([pscustomobject]@{status='PASS'}) $false) 'INCOMPLETE' 'interrupted run cannot pass'
Expect (Get-Verdict @([pscustomobject]@{status='PASS'}) $true) 'PASS' 'complete run'
Expect (Get-Verdict @([pscustomobject]@{status='SKIPPED'}) $true) 'INCOMPLETE' 'skipped required case'
Expect (Get-Verdict @([pscustomobject]@{status='BLOCKED'}) $true) 'BLOCKED' 'blocked case'
Expect (Get-Verdict @([pscustomobject]@{status='FAIL'},[pscustomobject]@{status='BLOCKED'}) $true) 'FAIL' 'failed evidence takes priority'
$rejected = $false
try { Assert-ExternalOutput 'C:\test-repo\nested' @('C:\test-repo') | Out-Null } catch { $rejected = $true }
Expect $rejected $true 'repository output rejected'
Expect (Assert-ExternalOutput 'C:\test-repo-other\results' @('C:\test-repo')) 'C:\test-repo-other\results' 'sibling path allowed'
Expect (Protect-Text 'token=disposable-test-secret') 'token=[REDACTED]' 'test-token redaction'
Add-Result 'first' 'PASS' 'checkpoint persists disposable-test-secret'
Add-Result 'second' 'BLOCKED' 'integration unavailable'
$saved = Get-Content -LiteralPath (Join-Path $testDirectory 'report.json') -Raw | ConvertFrom-Json
Expect $saved.verdict 'BLOCKED' 'report saved after each check'
Expect $saved.steps.Count 2 'atomic replacement keeps previous results'
Expect $saved.steps[0].detail 'checkpoint persists [REDACTED]' 'report contains no token'
Expect @(Import-Csv -LiteralPath (Join-Path $testDirectory 'steps.csv')).Count 2 'CSV checkpoint'
Assert-LedgerUnchanged @{digest='a';events=@()} @{digest='a';events=@()} 'no-write'
Expect $script:Run.steps[-1].status 'PASS' 'unchanged ledger'
Assert-LedgerUnchanged @{digest='a';events=@()} @{digest='b';events=@(@{action='apply'})} 'unexpected-write'
Expect $script:Run.steps[-1].status 'FAIL' 'unexpected write detected'
$rejected = $false
try { Invoke-LocalJson 'http://10.1.0.2/api/radio/devices' | Out-Null } catch { $rejected = $true }
Expect $rejected $true 'non-loopback destination rejected before connection'
$rejected = $false
try { Invoke-LocalJson 'http://127.0.0.1:8443/api/radio/devices' | Out-Null } catch { $rejected = $true }
Expect $rejected $true 'unowned port rejected before connection'
Write-Host "$passed guide self-checks passed. Test report retained at $testDirectory"
