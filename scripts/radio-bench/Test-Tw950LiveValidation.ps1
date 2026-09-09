#Requires -Version 5.1
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Start-Tw950LiveValidation.ps1') -LibraryOnly
$script:checks = 0
function Check([string]$Name, [bool]$Condition) {
    if (-not $Condition) { throw "FAIL: $Name" }
    $script:checks++
    Write-Host "PASS: $Name"
}
$mac='001e3f17abf0'
Check 'MAC hyphen/case normalization' ((Normalize-Mac '00-1E-3F-17-AB-F0') -eq $mac)
Check 'MAC colon normalization' ((Normalize-Mac '00:1e:3f:17:ab:f0') -eq $mac)
Check 'Reject MAC substring in arbitrary JSON' ((Normalize-Mac '{mac:001e3f17abf0}') -eq '')
Check 'Reject empty MAC' ((Normalize-Mac '') -eq '')
Check 'Reject all-zero MAC' ((Normalize-Mac '00:00:00:00:00:00') -eq '')
Check 'Reject broadcast MAC' ((Normalize-Mac 'ff:ff:ff:ff:ff:ff') -eq '')
Check 'Yes handles whitespace' ((Answer-Status ' YES ') -eq 'PASS')
Check 'Unknown is not failure' ((Answer-Status 'u') -eq 'BLOCKED')
Check 'Skip supported' ((Answer-Status 's') -eq 'SKIPPED')
Check 'Quit supported' ((Answer-Status 'q') -eq 'QUIT')
Check 'Unrecognized answer reprompts' ((Answer-Status 'maybe') -eq '')
$device=[pscustomobject]@{mac='00:1E:3F:17:AB:F0';state='connected';driver_available=$true}
Check 'Exact eligible identity' (Eligible @($device) $mac)
Check 'Unrelated identity rejected' (-not (Eligible @($device) '001e3f17abf1'))
Check 'Duplicate records blocked' (-not (Eligible @($device,$device) $mac))
Check 'Label containing MAC is not identity' (-not (Eligible @([pscustomobject]@{label=$mac;state='connected';driver_available=$true}) $mac))
Check 'Auth failed not eligible' (-not (Eligible @([pscustomobject]@{mac=$mac;state='auth-failed';driver_available=$true}) $mac))
Check 'Missing driver blocked' (-not (Eligible @([pscustomobject]@{mac=$mac;state='connected'}) $mac))
Check 'False driver blocked' (-not (Eligible @([pscustomobject]@{mac=$mac;state='connected';driver_available=$false}) $mac))
Check 'Unknown state blocked' (-not (Eligible @([pscustomobject]@{mac=$mac;state='discovered';driver_available=$true}) $mac))
Check 'Preflight not full pass' ((Verdict @() $false) -eq 'INCOMPLETE')
Check 'Skipped makes incomplete' ((Verdict @([pscustomobject]@{status='SKIPPED'}) $true) -eq 'INCOMPLETE')
Check 'Blocked verdict' ((Verdict @([pscustomobject]@{status='BLOCKED'}) $true) -eq 'BLOCKED')
Check 'Failure takes precedence' ((Verdict @([pscustomobject]@{status='FAIL'},[pscustomobject]@{status='BLOCKED'}) $true) -eq 'FAIL')
Check 'Completed all-pass' ((Verdict @([pscustomobject]@{status='PASS'}) $true) -eq 'PASS')
$redacted = Redact ([pscustomobject]@{body=[pscustomobject]@{config=@{arbitrary='secret-value'};api_key='api-secret';meta=@{label=@{writable=$true}}}})
$json=$redacted | ConvertTo-Json -Depth 10
Check 'Config values omitted' (-not $json.Contains('secret-value'))
Check 'API key omitted' (-not $json.Contains('api-secret'))
Check 'Metadata retained' ($json.Contains('writable'))
Check 'Nested mock fixture rejected' (Has-Simulation ([pscustomobject]@{records=@([pscustomobject]@{endpoints=@([pscustomobject]@{capability_hints=@('mock','video')})})}))
Check 'Simulated snapshot rejected' (Has-Simulation ([pscustomobject]@{simulated=$true}))
Check 'Real provenance accepted' (-not (Has-Simulation ([pscustomobject]@{simulated=$false;source_authority='chud_authoritative'})))
Assert-Loopback 'http://127.0.0.1:8443'
Check 'Loopback URL accepted' $true
$rejected=$false
try { Assert-Loopback 'http://10.1.0.2' } catch { $rejected=$true }
Check 'Direct radio API URL blocked' $rejected
$rejected=$false
try { Assert-Loopback 'http://secret@localhost:8443' } catch { $rejected=$true }
Check 'URL credentials blocked' $rejected
$fleet1=[pscustomobject]@{body=[pscustomobject]@{records=@([pscustomobject]@{record_id='a'})}}
$fleet2=[pscustomobject]@{body=[pscustomobject]@{records=@([pscustomobject]@{record_id='b'})}}
Check 'Fleet replacement detected despite same count' (((Fleet-Ids $fleet1) -join '|') -ne ((Fleet-Ids $fleet2) -join '|'))
Check 'Missing Fleet schema blocked' (-not (Valid-Fleet ([pscustomobject]@{status=200;body=[pscustomobject]@{}})))
Check 'Missing Fleet identity blocked' (-not (Valid-Fleet ([pscustomobject]@{status=200;body=[pscustomobject]@{records=@([pscustomobject]@{label='drone'})}})))
Check 'Empty Fleet allowed' (Valid-Fleet ([pscustomobject]@{status=200;body=[pscustomobject]@{records=@()}}))
Check 'Unavailable Fleet not empty Fleet' (-not (Valid-Fleet ([pscustomobject]@{status=503;body=[pscustomobject]@{records=@()}})))
$tokens=$null; $errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'Start-Tw950LiveValidation.ps1'),[ref]$tokens,[ref]$errors)
Check 'PowerShell syntax' ($errors.Count -eq 0)
$commands=@($ast.FindAll({param($node) $node -is [System.Management.Automation.Language.CommandAst]},$true) | ForEach-Object GetCommandName)
Check 'No mutation/launch commands in live recorder' (@($commands | Where-Object { $_ -match '^(Set-Net|New-Net|Remove-Net|Restart-|Stop-Process|Start-Process|Import-PfxCertificate|Invoke-RestMethod)' }).Count -eq 0)
Write-Host "$script:checks checks passed. No hardware or application services contacted."
