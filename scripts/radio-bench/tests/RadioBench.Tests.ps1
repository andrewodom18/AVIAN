BeforeAll {
    $script:BenchRoot = Split-Path -Parent $PSScriptRoot

    # Load function definitions without executing the hardware-facing body.
    $accessScript = Join-Path $script:BenchRoot 'Test-RadioAccess.ps1'
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $accessScript,
        [ref]$tokens,
        [ref]$errors
    )
    if ($errors.Count -gt 0) { throw 'Test-RadioAccess.ps1 did not parse.' }
    $ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true) |
        ForEach-Object { Invoke-Expression $_.Extent.Text }

    # These placeholders let Pester replace Windows-only networking commands
    # on Linux. No real adapter, route, socket, or radio operation is invoked.
    function Get-NetAdapter {}
    function Get-NetIPAddress {}
    function Get-NetNeighbor {}
}

Describe 'AVIAN radio-bench scripts' {
    It 'parses every script without executing it' {
        Get-ChildItem -LiteralPath $script:BenchRoot -Filter '*.ps1' -File |
            ForEach-Object {
                $tokens = $null
                $errors = $null
                [System.Management.Automation.Language.Parser]::ParseFile(
                    $_.FullName,
                    [ref]$tokens,
                    [ref]$errors
                ) | Out-Null
                $errors | Should -BeNullOrEmpty
            }
    }

    It 'honors an explicitly requested Ethernet adapter using mocked inventory' {
        Mock Get-NetAdapter {
            @([pscustomobject]@{
                Name = 'Bench Ethernet'
                InterfaceDescription = 'Mock Ethernet Adapter'
                Status = 'Up'
            })
        }

        Select-EthernetAdapter -Requested 'Bench Ethernet' | Should -Be 'Bench Ethernet'
        Should -Invoke Get-NetAdapter -Times 1 -Exactly
    }

    It 'returns a mocked reachable neighbor MAC without probing hardware' {
        Mock Get-NetNeighbor {
            [pscustomobject]@{
                State = 'Reachable'
                LinkLayerAddress = '00-1E-3F-20-9A-10'
            }
        }

        Get-RadioMac -Address '10.1.0.2' -Adapter 'Bench Ethernet' |
            Should -Be '00-1E-3F-20-9A-10'
        Should -Invoke Get-NetNeighbor -Times 1 -Exactly
    }
}
