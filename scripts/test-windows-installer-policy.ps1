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
            'Assert-WindowsBundleRootIsNotReparsePoint', 'Find-InnoSetupCompiler', 'Get-RegularFilePath')
}, $false)) { Set-Item -LiteralPath "Function:$($function.Name)" -Value ($function.Body.GetScriptBlock()) }
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

    # Exercise the uncovered build-tool admission boundary with synthetic pins.
    # The files are inert; validation must succeed before a tool could execute.
    $toolRoot = Join-Path $temporary 'Pinned Inspector'
    $null = New-Item -ItemType Directory -Path $toolRoot
    foreach ($name in @('innoextract.exe', 'libbz2-1.dll')) {
        [System.IO.File]::WriteAllText((Join-Path $toolRoot $name), "synthetic $name")
    }
    $originalPins = (Get-Item Function:Get-InnoInspectorPins).ScriptBlock
    $script:FixtureToolPins = @{ Files = @{} }
    foreach ($name in @('innoextract.exe', 'libbz2-1.dll')) {
        $script:FixtureToolPins.Files[$name] = Get-WindowsProbeSha256 (Join-Path $toolRoot $name)
    }
    function Get-InnoInspectorPins { return $script:FixtureToolPins }
    if ((Get-ValidatedInnoInspector $toolRoot) -cne (Join-Path $toolRoot 'innoextract.exe')) {
        throw 'Validated tool path does not match its pinned directory.'
    }
    Assert-Rejected { Get-ValidatedInnoInspector 'relative-inspector' } 'relative tool directory'
    [System.IO.File]::WriteAllText((Join-Path $toolRoot 'extra.dll'), 'unexpected native library')
    Assert-Rejected { Get-ValidatedInnoInspector $toolRoot } 'extra tool member'
    Remove-Item -LiteralPath (Join-Path $toolRoot 'extra.dll')
    [System.IO.File]::AppendAllText((Join-Path $toolRoot 'libbz2-1.dll'), 'changed')
    Assert-Rejected { Get-ValidatedInnoInspector $toolRoot } 'modified inspector dependency'
    Remove-Item -LiteralPath (Join-Path $toolRoot 'libbz2-1.dll')
    Assert-Rejected { Get-ValidatedInnoInspector $toolRoot } 'missing inspector dependency'
    Set-Item Function:Get-InnoInspectorPins $originalPins

    Assert-Rejected { Invoke-InnoPayloadInspector 'relative-inspector' $application } 'relative inspector invocation'
    Assert-Rejected { Invoke-InnoPayloadInspector $application ($application + '"') } 'quote-bearing installer path'
    Assert-Rejected { Invoke-InnoPayloadInspector $application $application -Extract } 'extraction without an output directory'

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
    Set-Item Function:Invoke-InnoPayloadInspector $originalInspector

    $warningScript = Join-Path $temporary 'Warning Tool.ps1'
    [System.IO.File]::WriteAllText($warningScript, "[Console]::Error.WriteLine('unsupported format fixture')")
    Assert-Rejected {
        Invoke-BoundedInspector -Inspector (Get-Process -Id $PID).Path `
            -Arguments ('-NoProfile -File "' + $warningScript + '"') -Label 'warning fixture' `
            -TokenPrefix 'inno-test' -ClosureClock $null -ClosureDeadlineMs 10000 `
            -ProcessDeadlineMs 10000 -OutputByteLimit 1024 -RejectStandardError
    } 'successful process with unsupported-input warnings'

    # A real owned process must be terminated, not merely ignored after a
    # timeout. Its handle stays local so cleanup can never target another PID.
    $stalled = [System.Diagnostics.Process]::new()
    $started = $false
    try {
        $stalled.StartInfo.FileName = (Get-Process -Id $PID).Path
        $stalled.StartInfo.Arguments = '-NoProfile -Command "Start-Sleep -Seconds 60"'
        $stalled.StartInfo.UseShellExecute = $false
        $stalled.StartInfo.CreateNoWindow = $true
        $started = $stalled.Start()
        if (-not $started) { throw 'Stalled-process fixture could not start.' }
        Stop-BoundedProcessTree $stalled 'stalled inspector fixture'
        if (-not $stalled.HasExited) { throw 'Inspector termination left its process alive.' }
    }
    finally {
        if ($started -and -not $stalled.HasExited) { $stalled.Kill(); $null = $stalled.WaitForExit(1000) }
        $stalled.Dispose()
    }

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
