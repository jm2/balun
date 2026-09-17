<# Portable payload-manifest failures; optional real installer/extractor compatibility samples. #>
param([string]$SampleRoot, [string]$Inspector)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-installer-policy.ps1')
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'build-windows.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Build helper parse failure.' }
foreach ($function in $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -in @('Get-WindowsProbeSha256', 'Get-WindowsProbeTreeDigest',
            'Get-PeMachine', 'Assert-PeMachine', 'Invoke-BoundedInspector',
            'Stop-BoundedProcessTree', 'Get-BoundedProbeDiagnostic', 'Test-IsWindowsHost',
            'Find-InnoSetupCompiler', 'Get-RegularFilePath')
}, $false)) { . ([scriptblock]::Create($function.Extent.Text)) }
function Exit-WithError { param([string]$Message) throw $Message }
function Assert-Rejected {
    param([scriptblock]$Action, [string]$Label)
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw "Installer policy accepted $Label" }
}
function Get-FixtureListing {
    param($Manifest)
    foreach ($record in $Manifest.Records.Values) {
        $fields = $record.Split("`t")
        $path = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($fields[1])).Replace('/', '\')
        if ($fields[0] -eq 'D') { "{app}\$path/" }
        else { "$($fields[2]) SHA-256 $($fields[3]) {app}\$path" }
    }
}
$temporaryBase = if ($env:TMPDIR) { $env:TMPDIR } else { [System.IO.Path]::GetTempPath() }
$temporary = Join-Path $temporaryBase ('balun-inno-test-' + [Guid]::NewGuid().ToString('N'))
try {
    $root = Join-Path $temporary 'Staged Tree'
    $null = New-Item -ItemType Directory -Path (Join-Path $root 'bin'), (Join-Path $root 'empty')
    $application = Join-Path $root 'bin/balun.exe'
    $pe = [byte[]]::new(512)
    $pe[0] = 0x4d; $pe[1] = 0x5a; $pe[0x3c] = 0x80
    $pe[0x80] = 0x50; $pe[0x81] = 0x45
    [BitConverter]::GetBytes([uint16]0x8664).CopyTo($pe, 0x84)
    [System.IO.File]::WriteAllBytes($application, $pe)
    $oldCompiler = $env:BALUN_ISCC
    try {
        $env:BALUN_ISCC = $application
        if ((Find-InnoSetupCompiler) -cne $application) { throw 'Explicit compiler override was ignored.' }
        $env:BALUN_ISCC = 'relative-compiler.exe'
        Assert-Rejected { Find-InnoSetupCompiler } 'relative compiler override'
        $env:BALUN_ISCC = Join-Path $root 'missing.exe'
        Assert-Rejected { Find-InnoSetupCompiler } 'missing compiler override'
        $env:BALUN_ISCC = $root
        Assert-Rejected { Find-InnoSetupCompiler } 'directory compiler override'
    }
    finally { $env:BALUN_ISCC = $oldCompiler }
    $expected = Get-WindowsProbeTreeDigest $root -IncludeRecords
    if ((Get-WindowsProbeTreeDigest $root) -cne $expected.Digest) { throw 'Manifest API changed digest identity.' }
    $listing = @(Get-FixtureListing $expected)
    Assert-InnoPayloadManifest $expected.Records (ConvertFrom-InnoPayloadListing $listing)

    Assert-Rejected { Assert-PeMachine $application 'fixture' ([uint16]0xAA64) 'ARM64' 'aarch64' } 'wrong native architecture'
    $hash = '0' * 64
    foreach ($path in @('{app}\..\escape', '{app}\C:\escape', '{app}\\server\file',
        '{tmp}\outside', '{app}\bin\file:stream', '{app}\bin\NUL.txt', '{app}\bin\COM1',
        '{app}\bin\trailing.', '{app}\bin\trailing ', '{app}\bin\.\file',
        '{app}\bin\\file', "{app}\bin\bad`nname", '{app}\bin\{code:value}',
        ('{app}\' + ('x\' * 65) + 'file'))) {
        Assert-Rejected { ConvertFrom-InnoPayloadListing @("1 SHA-256 $hash $path") } "unsafe path $path"
    }
    foreach ($records in @(
        @($listing + $listing),
        @($listing + "1 SHA-256 $hash {app}\BIN\other"),
        @($listing + "1 SHA-256 $hash {app}\bin"),
        @('L {app}\alias -> outside'),
        @("1073741825 SHA-256 $hash {app}\large"),
        @(1..5 | ForEach-Object { "1073741824 SHA-256 $hash {app}\large$_" }),
        @("1 MD5 $hash {app}\unverified"),
        @('x' * 1201)
    )) { Assert-Rejected { ConvertFrom-InnoPayloadListing $records } 'invalid listing' }
    Assert-Rejected { ConvertFrom-InnoPayloadListing ([string[]]::new(65537)) } 'excessive members'

    $missing = @($listing | Where-Object { $_ -notmatch 'balun.exe$' })
    Assert-Rejected { Assert-InnoPayloadManifest $expected.Records (ConvertFrom-InnoPayloadListing $missing) } 'missing file'
    $extra = @($listing + "1 SHA-256 $hash {app}\extra")
    Assert-Rejected { Assert-InnoPayloadManifest $expected.Records (ConvertFrom-InnoPayloadListing $extra) } 'extra file'
    $changed = @($listing | ForEach-Object { $_ -replace '[0-9a-f]{64}', $hash })
    Assert-Rejected { Assert-InnoPayloadManifest $expected.Records (ConvertFrom-InnoPayloadListing $changed) } 'changed content'
    $missingDirectory = @($listing | Where-Object { $_ -notmatch 'empty/$' })
    Assert-Rejected { Assert-InnoPayloadManifest $expected.Records (ConvertFrom-InnoPayloadListing $missingDirectory) } 'missing empty directory'

    # Drive the production inspection gate with a controlled tool result.
    # No invalid listing may start extraction or reach native/runtime checks.
    $originalInspector = (Get-Item Function:Invoke-InnoPayloadInspector).ScriptBlock
    $dummyInstaller = Join-Path $temporary 'fixture-installer.exe'
    [System.IO.File]::WriteAllText($dummyInstaller, 'static fixture input')
    $script:ExtractCalls = 0
    $script:GateCalls = 0
    $script:ProbeCalls = 0
    $script:ReceiptCalls = 0
    $script:MutateDuringProbe = $false
    function Invoke-InnoPayloadInspector {
        param([string]$Inspector, [string]$Installer, [string]$OutputDirectory, [switch]$Extract)
        if (-not $Extract) { return $script:FixtureListing }
        $script:ExtractCalls++
        Copy-Item -LiteralPath $root -Destination (Join-Path $OutputDirectory 'app') -Recurse
    }
    function Assert-WindowsPackageFinalGates {
        param([string]$Distribution, [string]$Inspector, [string]$ExpectedVersion)
        if ($Distribution -eq $root -or -not (Test-Path (Join-Path $Distribution 'bin/balun.exe'))) {
            throw 'Native gates did not receive the extracted tree.'
        }
        $script:GateCalls++
    }
    function Invoke-PackagedRuntimeProbe {
        param([string]$Distribution)
        $script:ProbeCalls++
        if ($script:MutateDuringProbe) {
            [System.IO.File]::WriteAllText((Join-Path $Distribution 'bin/balun.exe'), 'mutated')
        }
    }
    function Assert-WindowsProbeReceipt { param([string]$Root) $script:ReceiptCalls++ }
    foreach ($bad in @($missing, $extra, $changed, $missingDirectory,
        @("1 SHA-256 $hash {app}\..\escape"))) {
        $script:FixtureListing = $bad
        Assert-Rejected {
            Assert-WindowsInstallerPayload $dummyInstaller $root 'fixture-tool' 'fixture-pe-tool' '1.0' $expected
        } 'invalid payload before extraction'
    }
    if ($script:ExtractCalls -or $script:GateCalls -or $script:ProbeCalls) {
        throw 'Unvalidated installer content reached extraction or execution.'
    }
    $script:FixtureListing = $listing
    Assert-WindowsInstallerPayload $dummyInstaller $root 'fixture-tool' 'fixture-pe-tool' '1.0' $expected
    if ($script:ExtractCalls -ne 1 -or $script:GateCalls -ne 1 -or
        $script:ProbeCalls -ne 1 -or $script:ReceiptCalls -ne 1) {
        throw 'Validated installer skipped a required final gate.'
    }
    $script:MutateDuringProbe = $true
    Assert-Rejected {
        Assert-WindowsInstallerPayload $dummyInstaller $root 'fixture-tool' 'fixture-pe-tool' '1.0' $expected
    } 'mutation during final runtime validation'

    # Cleanup failure cannot hide the primary validation defect. Conversely,
    # cleanup-only failure must still fail the packaging gate.
    $script:CleanupPaths = [System.Collections.Generic.List[string]]::new()
    function Remove-Item {
        param([string]$LiteralPath, [switch]$Recurse, [switch]$Force, [string]$ErrorAction)
        $script:CleanupPaths.Add($LiteralPath)
        throw 'forced payload cleanup failure'
    }
    try {
        foreach ($mutate in @($true, $false)) {
            $script:MutateDuringProbe = $mutate
            $failure = $null
            try {
                Assert-WindowsInstallerPayload $dummyInstaller $root 'fixture-tool' 'fixture-pe-tool' '1.0' $expected
            }
            catch { $failure = $_.Exception.Message }
            $expectedFailure = if ($mutate) { 'Installer or payload changed during final inspection.' }
                else { 'forced payload cleanup failure' }
            if ($failure -cne $expectedFailure) { throw "Unexpected primary error after cleanup failure: $failure" }
        }
        if ($script:CleanupPaths.Count -ne 2) { throw 'Cleanup failure fixtures did not exercise both paths.' }
    }
    finally {
        Microsoft.PowerShell.Management\Remove-Item -LiteralPath Function:Remove-Item
        foreach ($path in $script:CleanupPaths) {
            Microsoft.PowerShell.Management\Remove-Item -LiteralPath $path -Recurse -Force
        }
    }
    Set-Item Function:Invoke-InnoPayloadInspector $originalInspector

    $warningScript = Join-Path $temporary 'Warning Tool.ps1'
    [System.IO.File]::WriteAllText($warningScript, "[Console]::Error.WriteLine('unsupported format fixture')")
    Assert-Rejected {
        Invoke-BoundedInspector -Inspector (Get-Process -Id $PID).Path `
            -Arguments ('-NoProfile -File "' + $warningScript + '"') -Label 'warning fixture' `
            -TokenPrefix 'inno-test' -ClosureClock $null -ClosureDeadlineMs 10000 `
            -ProcessDeadlineMs 10000 -OutputByteLimit 1024 -RejectStandardError
    } 'successful process with unsupported-input warnings'

    if ($SampleRoot) {
        if (-not $Inspector) { throw 'An explicit inspector is required with SampleRoot.' }
        foreach ($arch in @('x86_64', 'aarch64')) {
            $installer = Join-Path $SampleRoot "balun-windows-$arch-setup.exe"
            $records = ConvertFrom-InnoPayloadListing @(Invoke-InnoPayloadInspector $Inspector $installer)
            $extraction = Join-Path $temporary "Extracted $arch"
            $null = New-Item -ItemType Directory -Path $extraction
            $null = Invoke-InnoPayloadInspector $Inspector $installer $extraction -Extract
            $actual = Get-WindowsProbeTreeDigest (Join-Path $extraction 'app') -IncludeRecords
            Assert-InnoPayloadManifest $records $actual.Records
            Write-Host "$arch real installer listing matches every extracted file and directory."
        }
    }
    Write-Host 'Windows installer payload manifest regressions passed.'
}
finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Recurse -Force }
}
