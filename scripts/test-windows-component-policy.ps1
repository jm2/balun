Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).ProviderPath
$helper = Join-Path $PSScriptRoot 'build-windows.ps1'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($helper, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw "Build helper failed to parse: $errors" }
foreach ($function in $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -in @('Read-WindowsComponentPolicySnapshot', 'ConvertFrom-WindowsComponentPolicyText',
                        'Import-ForbiddenBundledComponentPolicy', 'Test-ForbiddenBundledComponentName')
}, $false)) {
    Set-Item -LiteralPath "Function:$($function.Name)" -Value ($function.Body.GetScriptBlock())
}
function Exit-WithError { param([string]$Message) throw $Message }
function Assert-Rejected {
    param([scriptblock]$Action, [string]$Label)
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw "Component policy incorrectly accepted $Label" }
}

$policy = Join-Path $repository 'build-aux/packaging/forbidden-bundled-components.txt'
$script:ForbiddenBundledComponentTokens = @(Import-ForbiddenBundledComponentPolicy $policy)
if ($script:ForbiddenBundledComponentTokens.Count -ne 20 -or
    (Test-ForbiddenBundledComponentName 'libavcodec.dll')) {
    throw 'Reviewed Windows policy does not enforce the expected component set.'
}
foreach ($token in $script:ForbiddenBundledComponentTokens) {
    if (-not (Test-ForbiddenBundledComponentName "prefix-$($token.ToUpperInvariant())-suffix.dll")) {
        throw 'Windows policy failed case-insensitive enforcement of a reviewed token.'
    }
}
$firstToken = $script:ForbiddenBundledComponentTokens[0]
$digest = (Get-FileHash -LiteralPath $policy -Algorithm SHA256).Hash.ToLowerInvariant()
foreach ($loader in @('scripts/build-windows.ps1', 'scripts/macos-package-policy.sh',
    'build-aux/packaging/validate-release-components.sh')) {
    if (-not [System.IO.File]::ReadAllText((Join-Path $repository $loader)).Contains($digest)) {
        throw "Policy checksum changed without synchronizing $loader"
    }
}
$temporaryBase = if ($env:TMPDIR) { $env:TMPDIR } else { [System.IO.Path]::GetTempPath() }
$temporary = Join-Path $temporaryBase "balun-policy-$([Guid]::NewGuid().ToString('N'))"
[System.IO.Directory]::CreateDirectory($temporary) | Out-Null
$candidate = Join-Path $temporary 'policy.txt'
try {
    $approved = [System.IO.File]::ReadAllBytes($policy)
    $approvedText = [System.Text.Encoding]::UTF8.GetString($approved)
    foreach ($invalid in @('dummy-token', $approvedText.Replace("$firstToken`n", ''),
        $approvedText.Replace($firstToken, 'dummy-token'), ($approvedText + "`0"),
        ('#' * 65537))) {
        [System.IO.File]::WriteAllText($candidate, $invalid, [System.Text.UTF8Encoding]::new($false))
        Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'modified, NUL, or oversized bytes'
    }
    [System.IO.File]::WriteAllBytes($candidate, [byte[]]@(0xc0, 0xaf))
    Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'malformed UTF-8'
    Remove-Item -LiteralPath $candidate
    $regular = Join-Path $temporary 'approved.txt'
    [System.IO.File]::WriteAllBytes($regular, $approved)
    New-Item -ItemType HardLink -Path $candidate -Value $regular | Out-Null
    Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'a hard-linked approved policy'
    Remove-Item -LiteralPath $candidate
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        # Junctions need no symlink privilege. A policy leaf directory is
        # rejected before any attempt to read through it.
        New-Item -ItemType Junction -Path $candidate -Value $temporary | Out-Null
        Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'a reparse directory'
        (Get-Item -LiteralPath $candidate -Force).Delete()
    }
    else {
        New-Item -ItemType SymbolicLink -Path $candidate -Value $policy | Out-Null
        Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'a symbolic link'
        Remove-Item -LiteralPath $candidate
    }
    [System.IO.File]::WriteAllBytes($candidate, $approved)
    $snapshot = Read-WindowsComponentPolicySnapshot $candidate
    [System.IO.File]::WriteAllText($candidate, 'dummy-token')
    if ([Convert]::ToBase64String($snapshot) -cne [Convert]::ToBase64String($approved)) {
        throw 'Validated snapshot changed with the original pathname.'
    }
    Assert-Rejected { Import-ForbiddenBundledComponentPolicy $candidate } 'replacement after a previous snapshot'

    # Exercise syntax/resource limits independently of the checksum gate, so
    # an approved future digest cannot accidentally loosen shared parsing rules.
    foreach ($invalid in @("first`nFIRST", ('a' * 65), ("#`n" * 1025), ('#' * 1025),
        '../unsafe', "first`n$([char]0xa0)second", '# comments only',
        ((1..257 | ForEach-Object { "token-$_" }) -join "`n"))) {
        Assert-Rejected { ConvertFrom-WindowsComponentPolicyText $invalid } 'invalid token syntax or parser bounds'
    }
    $parsed = @(ConvertFrom-WindowsComponentPolicyText " `tFIRST`r`n# comment`nsecond`n")
    if (($parsed -join ',') -cne 'first,second') { throw 'ASCII normalization differs from shared policy.' }
    Write-Host 'Windows component policy snapshot, checksum, alias, and syntax regressions passed.'
}
finally { Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue }
