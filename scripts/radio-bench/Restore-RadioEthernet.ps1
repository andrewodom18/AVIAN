[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)]
    [string]$SnapshotPath,
    [string]$BenchAddress = '10.1.0.20'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this recovery command from an elevated PowerShell window.'
}
if (-not (Test-Path -LiteralPath $SnapshotPath)) { throw "Snapshot was not found at '$SnapshotPath'." }

$snapshot = Import-Clixml -LiteralPath $SnapshotPath
$adapterName = [string]$snapshot.Adapter.Name
if (-not $PSCmdlet.ShouldProcess($adapterName, "restore radio-bench network settings from $SnapshotPath")) { return }

$originalAddresses = @($snapshot.Addresses | ForEach-Object { [string]$_.IPAddress })
if ($BenchAddress -notin $originalAddresses) {
    Get-NetIPAddress -InterfaceAlias $adapterName -AddressFamily IPv4 -IPAddress $BenchAddress -ErrorAction SilentlyContinue |
        Remove-NetIPAddress -Confirm:$false
}
foreach ($address in @($snapshot.Addresses | Where-Object PrefixOrigin -ne 'WellKnown')) {
    $present = Get-NetIPAddress -InterfaceAlias $adapterName -AddressFamily IPv4 -IPAddress $address.IPAddress -ErrorAction SilentlyContinue
    if (-not $present) {
        New-NetIPAddress -InterfaceAlias $adapterName -IPAddress $address.IPAddress -PrefixLength $address.PrefixLength | Out-Null
    }
}

$originalInterface = @($snapshot.IpInterface)[0]
if ($originalInterface) {
    Set-NetIPInterface -InterfaceAlias $adapterName -AddressFamily IPv4 `
        -Dhcp $originalInterface.Dhcp -InterfaceMetric $originalInterface.InterfaceMetric
}
if ([string]$snapshot.Adapter.Status -eq 'Disabled') {
    Disable-NetAdapter -Name $adapterName -Confirm:$false
}
Write-Host "Restored $adapterName from $SnapshotPath" -ForegroundColor Green
