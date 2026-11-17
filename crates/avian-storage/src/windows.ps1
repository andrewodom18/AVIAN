[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$request = $null
$bytes = $null
try {
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
    function Assert-OrdinaryPath([string]$path) {
        $current = [System.IO.Path]::GetFullPath($path)
        while ($current) {
            if ([System.IO.File]::Exists($current) -or [System.IO.Directory]::Exists($current)) {
                $attributes = [System.IO.File]::GetAttributes($current)
                if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Reparse point' }
            }
            $current = [System.IO.Path]::GetDirectoryName($current)
        }
    }
    function Assert-PrivateAcl([string]$path, [bool]$directory) {
        Assert-OrdinaryPath $path
        if ($directory -ne [System.IO.Directory]::Exists($path)) { throw 'Unexpected file type' }
        $acl = Get-Acl -LiteralPath $path
        if ($acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $identity.Value) { throw 'Wrong owner' }
        if (-not $acl.AreAccessRulesProtected) { throw 'Inherited permissions' }
        $ownerAllowed = $false
        foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow) {
                if ($rule.IdentityReference.Value -ne $identity.Value) { throw 'Foreign access' }
                $ownerAllowed = $true
            }
        }
        if (-not $ownerAllowed) { throw 'Missing owner permissions' }
    }
    function Assert-ControlledParent([string]$path) {
        Assert-OrdinaryPath $path
        $acl = Get-Acl -LiteralPath $path
        $trusted = @($identity.Value, 'S-1-5-18', 'S-1-5-32-544')
        if ($trusted -notcontains $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value) { throw 'Uncontrolled parent owner' }
        $mutation = [System.Security.AccessControl.FileSystemRights]::Write -bor [System.Security.AccessControl.FileSystemRights]::Delete -bor [System.Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor [System.Security.AccessControl.FileSystemRights]::ChangePermissions -bor [System.Security.AccessControl.FileSystemRights]::TakeOwnership
        foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
            if ($rule.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and
                ($rule.FileSystemRights -band $mutation) -ne 0 -and
                $trusted -notcontains $rule.IdentityReference.Value) { throw 'Uncontrolled parent permissions' }
        }
    }
    function Set-PrivateAcl([string]$path, [bool]$directory) {
        if ($directory) { $acl = New-Object System.Security.AccessControl.DirectorySecurity }
        else { $acl = New-Object System.Security.AccessControl.FileSecurity }
        $acl.SetOwner($identity)
        $acl.SetAccessRuleProtection($true, $false)
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($identity, 'FullControl', 'Allow')
        $acl.AddAccessRule($rule)
        Set-Acl -LiteralPath $path -AclObject $acl
        Assert-PrivateAcl $path $directory
    }
    $data = ''
    switch ($request.action) {
        'protect' {
            Add-Type -AssemblyName System.Security
            $bytes = [Convert]::FromBase64String($request.data)
            $protected = [System.Security.Cryptography.ProtectedData]::Protect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
            $data = [Convert]::ToBase64String($protected)
        }
        'unprotect' {
            Add-Type -AssemblyName System.Security
            $protected = [Convert]::FromBase64String($request.data)
            $bytes = [System.Security.Cryptography.ProtectedData]::Unprotect($protected, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
            $data = [Convert]::ToBase64String($bytes)
        }
        'directory' { Assert-PrivateAcl $request.path $true }
        'create_directory' {
            Assert-OrdinaryPath $request.path
            if ([System.IO.Directory]::Exists($request.path)) { Assert-PrivateAcl $request.path $true }
            else {
                Assert-ControlledParent ([System.IO.Path]::GetDirectoryName($request.path))
                [System.IO.Directory]::CreateDirectory($request.path) | Out-Null
                Set-PrivateAcl $request.path $true
            }
        }
        'validate' {
            Assert-PrivateAcl ([System.IO.Path]::GetDirectoryName($request.path)) $true
            Assert-PrivateAcl $request.path $false
        }
        'read' {
            Assert-PrivateAcl ([System.IO.Path]::GetDirectoryName($request.path)) $true
            Assert-PrivateAcl $request.path $false
            $file = [System.IO.File]::Open($request.path, 'Open', 'Read', 'Read')
            try {
                if ($file.Length -gt $request.maximum_bytes) { throw 'File too large' }
                $bytes = New-Object byte[] $file.Length
                $offset = 0
                while ($offset -lt $bytes.Length) {
                    $count = $file.Read($bytes, $offset, $bytes.Length - $offset)
                    if ($count -eq 0) { throw 'Short read' }
                    $offset += $count
                }
                $data = [Convert]::ToBase64String($bytes)
            } finally { $file.Dispose() }
        }
        'lock' {
            $parent = [System.IO.Path]::GetDirectoryName($request.path)
            Assert-PrivateAcl $parent $true
            Assert-OrdinaryPath $request.path
            if (-not [System.IO.File]::Exists($request.path)) {
                $file = [System.IO.File]::Open($request.path, 'CreateNew', 'ReadWrite', 'None')
                $file.Dispose()
                Set-PrivateAcl $request.path $false
            }
            Assert-PrivateAcl $request.path $false
        }
        { $_ -in 'create', 'replace' } {
            $parent = [System.IO.Path]::GetDirectoryName($request.path)
            Assert-PrivateAcl $parent $true
            Assert-OrdinaryPath $request.path
            if ([System.IO.File]::Exists($request.path)) { Assert-PrivateAcl $request.path $false }
            $temporary = [System.IO.Path]::Combine($parent, '.avian-storage-' + [Guid]::NewGuid().ToString('N') + '.tmp')
            try {
                $file = [System.IO.File]::Open($temporary, 'CreateNew', 'ReadWrite', 'None')
                $file.Dispose()
                Set-PrivateAcl $temporary $false
                $bytes = [Convert]::FromBase64String($request.data)
                $file = [System.IO.File]::Open($temporary, 'Open', 'Write', 'None')
                try { $file.Write($bytes, 0, $bytes.Length); $file.Flush($true) }
                finally { $file.Dispose() }
                if ($request.action -eq 'replace' -and [System.IO.File]::Exists($request.path)) {
                    [System.IO.File]::Replace($temporary, $request.path, $null)
                } else { [System.IO.File]::Move($temporary, $request.path) }
                Assert-PrivateAcl $request.path $false
            } finally {
                if ([System.IO.File]::Exists($temporary)) { [System.IO.File]::Delete($temporary) }
            }
        }
        default { throw 'Unsupported storage operation' }
    }
    [Console]::Out.WriteLine((@{ok=$true; data=$data} | ConvertTo-Json -Compress))
} catch {
    # Do not serialize platform exceptions: they may include input or filesystem details.
    [Console]::Out.WriteLine('{"ok":false,"data":""}')
    exit 1
} finally {
    if ($null -ne $bytes) { [Array]::Clear($bytes, 0, $bytes.Length) }
    if ($null -ne $request) { $request.data = '' }
}
