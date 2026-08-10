[CmdletBinding()]
param(
    [string]$AdapterName = 'Ethernet 2',
    [string]$ComputerAddress = '10.1.0.20',
    [string]$RadioAddress = '10.1.0.2',
    [int]$PrefixLength = 24
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdministrator) {
    Write-Host 'Windows administrator approval is required to re-enable the disabled Intel Ethernet device.' -ForegroundColor Yellow
    $arguments = @(
        '-NoLogo',
        '-NoExit',
        '-ExecutionPolicy', 'Bypass',
        '-File', "`"$PSCommandPath`"",
        '-AdapterName', "`"$AdapterName`"",
        '-ComputerAddress', $ComputerAddress,
        '-RadioAddress', $RadioAddress,
        '-PrefixLength', $PrefixLength
    )
    Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList $arguments
    exit
}

Write-Host "`n=== Enable the dedicated radio Ethernet interface ===" -ForegroundColor Cyan
Write-Host "Looking for adapter '$AdapterName', PC address $ComputerAddress/$PrefixLength, and radio $RadioAddress."
Write-Host 'No default gateway or DNS server will be assigned to this interface; Wi-Fi remains the internet route.'

$adapter = Get-NetAdapter -Name $AdapterName -IncludeHidden -ErrorAction SilentlyContinue
if (-not $adapter) { throw "Adapter '$AdapterName' was not found." }

$pnp = Get-PnpDevice -Class Net | Where-Object FriendlyName -eq $adapter.InterfaceDescription | Select-Object -First 1
if (-not $pnp) { throw "PnP device for '$AdapterName' was not found." }

if ($pnp.Problem -eq 'CM_PROB_DISABLED' -or $pnp.Status -ne 'OK') {
    Write-Host 'Re-enabling the disabled Intel network device...'
    Enable-PnpDevice -InstanceId $pnp.InstanceId -Confirm:$false
    Start-Sleep -Seconds 3
}

Enable-NetAdapter -Name $AdapterName -Confirm:$false -ErrorAction SilentlyContinue
Set-NetIPInterface -InterfaceAlias $AdapterName -AddressFamily IPv4 -Dhcp Disabled -InterfaceMetric 500

Get-NetRoute -InterfaceAlias $AdapterName -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue |
    Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue

$existing = @(Get-NetIPAddress -InterfaceAlias $AdapterName -AddressFamily IPv4 -ErrorAction SilentlyContinue)
$wanted = $existing | Where-Object { $_.IPAddress -eq $ComputerAddress -and $_.PrefixLength -eq $PrefixLength } | Select-Object -First 1
if (-not $wanted) {
    $existing | Where-Object { $_.PrefixOrigin -ne 'WellKnown' } |
        Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue
    New-NetIPAddress -InterfaceAlias $AdapterName -IPAddress $ComputerAddress -PrefixLength $PrefixLength | Out-Null
}

Start-Sleep -Seconds 2
$adapter = Get-NetAdapter -Name $AdapterName
$address = Get-NetIPAddress -InterfaceAlias $AdapterName -AddressFamily IPv4 -ErrorAction SilentlyContinue |
    Where-Object IPAddress -eq $ComputerAddress |
    Select-Object -First 1

Write-Host "`nAdapter: $($adapter.Name) status=$($adapter.Status) link=$($adapter.LinkSpeed)" -ForegroundColor $(if ($adapter.Status -eq 'Up') { 'Green' } else { 'Yellow' })
Write-Host "Address: $(if ($address) { "$($address.IPAddress)/$($address.PrefixLength)" } else { 'not assigned' })"

$pingReply = (& ping.exe -n 4 -w 1000 $RadioAddress 2>&1 | Out-String)
Write-Host $pingReply
if ($pingReply -match '(?i)TTL[= ]\d+') {
    Write-Host "Radio $RadioAddress is reachable. Return to the monitor window and continue the CHUD/ARC test." -ForegroundColor Green
} elseif ($adapter.Status -ne 'Up') {
    Write-Host 'The adapter is enabled, but there is still no Ethernet carrier. Check radio power, the Ethernet cable, and the port used.' -ForegroundColor Yellow
} else {
    Write-Host "Ethernet carrier is present, but $RadioAddress did not reply. Confirm the radio's management IP." -ForegroundColor Yellow
}

Read-Host 'Press Enter to close this repair window' | Out-Null
