<#
Deterministic complete-tree receipt regressions. -StagedRoot additionally tests
the actual successfully probed native Windows payload produced by the helper.
#>
param([string]$StagedRoot, [ValidateSet('x86_64', 'aarch64')][string]$Profile = 'x86_64')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
$helper = Join-Path $PSScriptRoot 'build-windows.ps1'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($helper, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw "Build helper failed to parse: $errors" }
foreach ($function in $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -match '^(Get|Write|Assert)-WindowsProbe'
}, $false)) {
    Set-Item -LiteralPath "Function:$($function.Name)" -Value ($function.Body.GetScriptBlock())
}
foreach ($assignment in $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and
        $node.Left.Extent.Text -in @('$ProbeReceiptHeader', '$ProbeReceiptSuffix')
}, $false)) {
    . ([scriptblock]::Create($assignment.Extent.Text))
}

function Set-TestProfile {
    param([string]$Name)
    $script:DesktopBuildProfile = @{ Name = $Name }
    if ($Name -eq 'aarch64') {
        $script:DesktopRustTarget = 'aarch64-pc-windows-gnullvm'
        $script:MsysEnvironment = 'clangarm64'
        $script:ExpectedPeMachine = [uint16]0xAA64
        $script:InnoTargetArchitecture = 'arm64'
    }
    else {
        $script:DesktopRustTarget = 'x86_64-pc-windows-gnullvm'
        $script:MsysEnvironment = 'clang64'
        $script:ExpectedPeMachine = [uint16]0x8664
        $script:InnoTargetArchitecture = 'x64'
    }
}

function Assert-Rejected {
    param([scriptblock]$Action, [string]$Description)
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    if (-not $rejected) { throw "Receipt incorrectly accepted $Description" }
}

Set-TestProfile $Profile
if ($StagedRoot) {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'The native receipt integration requires Windows.'
    }
    Assert-WindowsProbeReceipt $StagedRoot
    $library = Join-Path $StagedRoot 'bin\libgstreamer-1.0-0.dll'
    $original = [System.IO.File]::ReadAllBytes($library)
    try {
        $changed = [byte[]]$original.Clone()
        $pe = [BitConverter]::ToInt32($changed, 0x3c)
        if ([BitConverter]::ToUInt32($changed, $pe) -ne 0x00004550 -or
            [BitConverter]::ToUInt16($changed, $pe + 4) -ne $ExpectedPeMachine) {
            throw 'Native integration fixture is not the expected valid PE dependency.'
        }
        # Change only the COFF timestamp: all code, imports, exports, and PE
        # structure remain intact. This is outside every old receipt anchor.
        $changed[$pe + 8] = $changed[$pe + 8] -bxor 1
        [System.IO.File]::WriteAllBytes($library, $changed)
        Assert-Rejected { Assert-WindowsProbeReceipt $StagedRoot } 'a changed valid native dependency'
    }
    finally { [System.IO.File]::WriteAllBytes($library, $original) }
    Assert-WindowsProbeReceipt $StagedRoot
    Write-Host "Native $Profile probe receipt rejects a changed non-anchor DLL."
    exit 0
}

$temporaryBase = if ($env:TMPDIR) { $env:TMPDIR } else { [System.IO.Path]::GetTempPath() }
$temporary = Join-Path $temporaryBase "balun-receipt-$([Guid]::NewGuid().ToString('N'))"
$root = Join-Path $temporary 'Staged Tree With Spaces'
$inputRoot = Join-Path $temporary 'Local Packaging Inputs'
$originalRepository = $RepositoryRoot
try {
    foreach ($path in @('scripts/build-windows.ps1', 'Cargo.toml', 'Cargo.lock',
        'scripts/windows-installer-policy.ps1',
        'build-aux/packaging/forbidden-bundled-components.txt', 'build-aux/inno/balun.iss')) {
        $destination = Join-Path $inputRoot $path
        [System.IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
        [System.IO.File]::WriteAllText($destination, "fixture for $path")
    }
    $script:RepositoryRoot = $inputRoot
    $members = @('bin/balun.exe', 'bin/libgstreamer-1.0-0.dll', 'bin/decoder.dll',
        'lib/gstreamer-1.0/libgstcoreelements.dll', 'lib/gstreamer-1.0/libgstgtk4.dll',
        'libexec/gstreamer-1.0/gst-plugin-scanner.exe', 'share/glib-2.0/schemas/gschemas.compiled',
        'etc/runtime.conf')
    foreach ($path in $members) {
        $destination = Join-Path $root $path
        [System.IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
        [System.IO.File]::WriteAllText($destination, "original:$path")
    }
    foreach ($name in @('x86_64', 'aarch64')) {
        Set-TestProfile $name
        $before = @(Get-WindowsProbeReceiptLines $root)
        Write-WindowsProbeReceipt $root $before
        Assert-WindowsProbeReceipt $root
        foreach ($path in $members) {
            $destination = Join-Path $root $path
            $original = [System.IO.File]::ReadAllBytes($destination)
            $written = [System.IO.File]::GetLastWriteTimeUtc($destination)
            $modified = [byte[]]$original.Clone()
            $modified[0] = $modified[0] -bxor 1
            [System.IO.File]::WriteAllBytes($destination, $modified)
            [System.IO.File]::SetLastWriteTimeUtc($destination, $written)
            Assert-Rejected { Assert-WindowsProbeReceipt $root } "same-size/timestamp mutation of $path"
            Assert-Rejected { Write-WindowsProbeReceipt $root $before } 'a mutation while the runtime probe ran'
            Remove-Item -LiteralPath $destination
            Assert-Rejected { Assert-WindowsProbeReceipt $root } "deletion of $path"
            [System.IO.File]::WriteAllBytes($destination, $original)
        }
        $extra = Join-Path $root 'unexpected.dll'
        [System.IO.File]::WriteAllText($extra, 'unexpected')
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'an extra file'
        Remove-Item -LiteralPath $extra
        [System.IO.Directory]::CreateDirectory($extra) | Out-Null
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'an extra empty directory'
        [System.IO.Directory]::Delete($extra)
        $alias = Join-Path $root 'bin/decoder.dll'
        $outside = Join-Path $temporary 'outside.dll'
        Move-Item -LiteralPath $alias -Destination $outside
        New-Item -ItemType HardLink -Path $alias -Value $outside | Out-Null
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'a same-content hard-link replacement'
        Remove-Item -LiteralPath $alias
        Move-Item -LiteralPath $outside -Destination $alias
        if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
            Move-Item -LiteralPath $alias -Destination $outside
            New-Item -ItemType SymbolicLink -Path $alias -Value $outside | Out-Null
            Assert-Rejected { Assert-WindowsProbeReceipt $root } 'a same-content symbolic-link replacement'
            Remove-Item -LiteralPath $alias
            Move-Item -LiteralPath $outside -Destination $alias
        }
        $script:MsysEnvironment = 'wrong-profile'
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'changed profile identity'
        Set-TestProfile $name
        $policy = Join-Path $inputRoot 'build-aux/packaging/forbidden-bundled-components.txt'
        $original = [System.IO.File]::ReadAllBytes($policy)
        [System.IO.File]::AppendAllText($policy, 'changed')
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'changed packaging policy'
        [System.IO.File]::WriteAllBytes($policy, $original)
        Assert-WindowsProbeReceipt $root
        $receipt = Get-WindowsProbeReceiptPath $root
        foreach ($invalid in @('balun-windows-runtime-probe-v2', ('x' * 4097))) {
            [System.IO.File]::WriteAllText($receipt, $invalid)
            Assert-Rejected { Assert-WindowsProbeReceipt $root } 'legacy or oversized receipt'
        }
        [System.IO.File]::WriteAllBytes($receipt, [byte[]]@(0xff, 0xfe, 0x80))
        Assert-Rejected { Assert-WindowsProbeReceipt $root } 'invalid UTF-8 receipt'
    }
    Write-Host 'Complete-tree Windows probe receipt regressions passed for both profiles.'
}
finally {
    $script:RepositoryRoot = $originalRepository
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
