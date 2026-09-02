<#
Deterministic command-routing tests for scripts/build-windows.ps1.

Each invocation runs in a fresh child PowerShell process with fake Cargo and
MSYS2 CLANG64 commands. No compiler, installer, package manager, network
access, GUI toolkit, or Balun artifact is required.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ScriptDirectory = Split-Path -Parent $PSCommandPath
$HelperUnderTest = Join-Path $ScriptDirectory 'build-windows.ps1'
$TemporaryRoot = Join-Path (
    [System.IO.Path]::GetTempPath()
) "balun-windows-routing-$([Guid]::NewGuid().ToString('N'))"
$FixtureRoot = Join-Path $TemporaryRoot 'repository with spaces'
$FixtureScripts = Join-Path $FixtureRoot 'scripts'
$FixtureHelper = Join-Path $FixtureScripts 'build-windows.ps1'
$FixtureTargetRoot = Join-Path $FixtureRoot 'target'
$Runner = Join-Path $TemporaryRoot 'runner.ps1'
$CommandLog = Join-Path $TemporaryRoot 'commands.log'
$EnvironmentLog = Join-Path $TemporaryRoot 'environment.log'
$PkgConfigLog = Join-Path $TemporaryRoot 'pkg-config.log'
$TargetProbeLog = Join-Path $TemporaryRoot 'rustc-target-probe.log'
$RestrictedPath = Join-Path $TemporaryRoot 'empty-command-path'
$FakeMsysRoot = Join-Path $TemporaryRoot 'MSYS2 root with spaces'
$FakeMsysPrefix = Join-Path $FakeMsysRoot 'clang64'
$FakeMsysBin = Join-Path $FakeMsysPrefix 'bin'
$FakePkgConfigDirectory = Join-Path $FakeMsysPrefix 'lib\pkgconfig'
$FakePluginDirectory = Join-Path $FakeMsysPrefix 'lib\gstreamer-1.0'
$IncompleteMsysRoot = Join-Path $TemporaryRoot 'incomplete-msys2'
$PowerShellExecutable = (Get-Process -Id $PID).Path
$DesktopTarget = 'x86_64-pc-windows-gnullvm'
$EnvironmentNames = @(
    'BALUN_WINDOWS_HELPER',
    'BALUN_WINDOWS_TEST_LOG',
    'BALUN_WINDOWS_TEST_ENV_LOG',
    'BALUN_WINDOWS_TEST_PKG_LOG',
    'BALUN_WINDOWS_TEST_TARGET_PROBE_LOG',
    'BALUN_WINDOWS_FAKE_CARGO_STATUS',
    'BALUN_WINDOWS_FAKE_CARGO_AVAILABLE',
    'BALUN_WINDOWS_FAKE_COVERAGE_VERSION',
    'BALUN_WINDOWS_FAKE_RUSTC_STATUS',
    'BALUN_WINDOWS_FAKE_RUSTC_AVAILABLE',
    'BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE',
    'BALUN_WINDOWS_FAKE_TARGET_LIBDIR',
    'BALUN_WINDOWS_FAKE_PKG_STATUS',
    'BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE',
    'BALUN_WINDOWS_FAKE_SKIP_BINARY',
    'BALUN_WINDOWS_FAKE_ZERO_BINARY',
    'BALUN_WINDOWS_FAKE_DIRECTORY_BINARY',
    'BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY',
    'BALUN_WINDOWS_TEST_RESTRICTED_PATH',
    'CARGO_BUILD_TARGET',
    'CARGO_LLVM_COV_BUILD_DIR',
    'CARGO_LLVM_COV_TARGET_DIR',
    'CARGO_TARGET_DIR',
    'MSYS2_ROOT',
    'RUST_TARGET'
)
$OriginalEnvironment = @{}
foreach ($Name in $EnvironmentNames) {
    $OriginalEnvironment[$Name] = [Environment]::GetEnvironmentVariable(
        $Name,
        [EnvironmentVariableTarget]::Process
    )
}

$LastStatus = 0
$LastOutput = ''
$LastArguments = @()

function Assert-RoutingTestFailure {
    param([string]$Message)

    [Console]::Error.WriteLine("build-windows routing test failed: $Message")
    [Console]::Error.WriteLine(
        "arguments: $(@($script:LastArguments | ForEach-Object { "<$_>" }) -join ' ')"
    )
    [Console]::Error.WriteLine("status: $script:LastStatus")
    [Console]::Error.WriteLine("output:`n$script:LastOutput")
    foreach ($Log in @($CommandLog, $EnvironmentLog, $PkgConfigLog, $TargetProbeLog)) {
        if (Test-Path -LiteralPath $Log -PathType Leaf) {
            [Console]::Error.WriteLine(
                "$([System.IO.Path]::GetFileName($Log)):`n$([System.IO.File]::ReadAllText($Log))"
            )
        }
    }
    exit 1
}

function Invoke-TestHelper {
    param([string[]]$Arguments)

    $script:LastArguments = @($Arguments)
    foreach ($Log in @($CommandLog, $EnvironmentLog, $PkgConfigLog, $TargetProbeLog)) {
        [System.IO.File]::WriteAllText($Log, '')
    }
    if (Test-Path -LiteralPath $FixtureTargetRoot) {
        Remove-Item -LiteralPath $FixtureTargetRoot -Recurse -Force
    }

    $Captured = @(
        & $PowerShellExecutable -NoLogo -NoProfile -File $Runner @Arguments 2>&1
    )
    $script:LastStatus = $LASTEXITCODE
    $script:LastOutput = (@($Captured | ForEach-Object { $_.ToString() }) -join "`n")
}

function Assert-ExpectedStatus {
    param([int]$Expected)
    if ($script:LastStatus -ne $Expected) {
        Assert-RoutingTestFailure "expected status $Expected"
    }
}

function Assert-ExpectedOutput {
    param([string]$Expected)
    if (-not $script:LastOutput.Contains($Expected)) {
        Assert-RoutingTestFailure "expected output containing: $Expected"
    }
}

function Assert-ExpectedLog {
    param([string]$Expected)
    $Actual = [System.IO.File]::ReadAllText($CommandLog).TrimEnd(
        [char[]]@("`r", "`n")
    )
    if ($Actual -cne $Expected) {
        Assert-RoutingTestFailure "unexpected command routing; expected '$Expected', got '$Actual'"
    }
}

function Assert-EmptyLog {
    param([string]$Path, [string]$Label)
    if ((Get-Item -LiteralPath $Path).Length -ne 0) {
        Assert-RoutingTestFailure "expected no $Label invocation"
    }
}

function Assert-ExpectedPkgConfigProbeSet {
    $Lines = @([System.IO.File]::ReadAllLines($PkgConfigLog))
    if ($Lines.Count -ne 3 -or
        $Lines[0] -cne 'pkg-config <--atleast-version> <4.16> <gtk4>' -or
        $Lines[1] -cne 'pkg-config <--atleast-version> <1.6> <libadwaita-1>' -or
        $Lines[2] -cne 'pkg-config <--atleast-version> <1.20> <gstreamer-1.0>') {
        Assert-RoutingTestFailure 'unexpected pkg-config probes'
    }
}

function Assert-ExpectedPkgConfigGtkProbe {
    $Lines = @([System.IO.File]::ReadAllLines($PkgConfigLog))
    if ($Lines.Count -ne 1 -or
        $Lines[0] -cne 'pkg-config <--atleast-version> <4.16> <gtk4>') {
        Assert-RoutingTestFailure 'unexpected pkg-config probes before GTK rejection'
    }
}

function Assert-ExpectedPkgConfigGtkAdwaitaProbes {
    $Lines = @([System.IO.File]::ReadAllLines($PkgConfigLog))
    if ($Lines.Count -ne 2 -or
        $Lines[0] -cne 'pkg-config <--atleast-version> <4.16> <gtk4>' -or
        $Lines[1] -cne 'pkg-config <--atleast-version> <1.6> <libadwaita-1>') {
        Assert-RoutingTestFailure 'unexpected pkg-config probes before libadwaita rejection'
    }
}

function Assert-DesktopTargetProbe {
    $Lines = @([System.IO.File]::ReadAllLines($TargetProbeLog))
    if ($Lines.Count -ne 1 -or $Lines[0] -cne "target-libdir <$DesktopTarget>") {
        Assert-RoutingTestFailure "unexpected Rust target probe: $($Lines -join '; ')"
    }
}

function Assert-DesktopEnvironment {
    $EnvironmentText = [System.IO.File]::ReadAllText($EnvironmentLog)
    foreach ($Expected in @(
        "PKG_CONFIG=<$FakePkgConfigCommand>",
        "PKG_CONFIG_PATH=<$FakePkgConfigDirectory>",
        "PKG_CONFIG_LIBDIR=<$FakePkgConfigDirectory>",
        'PKG_CONFIG_ALLOW_CROSS=<1>',
        "TARGET_PKG_CONFIG=<$FakePkgConfigCommand>",
        "TARGET_PKG_CONFIG_PATH=<$FakePkgConfigDirectory>",
        "TARGET_PKG_CONFIG_LIBDIR=<$FakePkgConfigDirectory>",
        'TARGET_PKG_CONFIG_ALLOW_CROSS=<1>',
        "PATH_FIRST=<$FakeMsysBin>",
        "CC=<$FakeMsysBin$([System.IO.Path]::DirectorySeparatorChar)clang$FakeToolSuffix>",
        "CXX=<$FakeMsysBin$([System.IO.Path]::DirectorySeparatorChar)clang++$FakeToolSuffix>",
        "AR=<$FakeMsysBin$([System.IO.Path]::DirectorySeparatorChar)llvm-ar$FakeToolSuffix>",
        "DLLTOOL=<$FakeMsysBin$([System.IO.Path]::DirectorySeparatorChar)llvm-dlltool$FakeToolSuffix>"
    )) {
        if (-not $EnvironmentText.Contains($Expected)) {
            Assert-RoutingTestFailure "desktop environment is missing: $Expected"
        }
    }
}

function Assert-CoverageEnvironment {
    $EnvironmentText = [System.IO.File]::ReadAllText($EnvironmentLog)
    $CoverageArtifactRoot = Join-Path $FixtureTargetRoot 'llvm-cov-target'
    foreach ($Expected in @(
        "CARGO_TARGET_DIR=<$FixtureTargetRoot>",
        "CARGO_LLVM_COV_TARGET_DIR=<$CoverageArtifactRoot>",
        "CARGO_LLVM_COV_BUILD_DIR=<$CoverageArtifactRoot>"
    )) {
        if (-not $EnvironmentText.Contains($Expected)) {
            Assert-RoutingTestFailure "coverage environment is missing: $Expected"
        }
    }
}

try {
    [System.IO.Directory]::CreateDirectory($FixtureScripts) | Out-Null
    [System.IO.Directory]::CreateDirectory($RestrictedPath) | Out-Null
    [System.IO.Directory]::CreateDirectory($FakeMsysBin) | Out-Null
    [System.IO.Directory]::CreateDirectory($FakePkgConfigDirectory) | Out-Null
    [System.IO.Directory]::CreateDirectory($FakePluginDirectory) | Out-Null
    foreach ($Plugin in @(
        'libgstcoreelements', 'libgstplayback', 'libgstapp', 'libgsttypefindfunctions',
        'libgstdeinterlace', 'libgstmpegtsdemux', 'libgstgtk4', 'libgstlibav'
    )) {
        [System.IO.File]::WriteAllBytes(
            (Join-Path $FakePluginDirectory "$Plugin.dll"),
            [byte[]]@(0x4d, 0x5a)
        )
    }
    [System.IO.Directory]::CreateDirectory($IncompleteMsysRoot) | Out-Null
    Copy-Item -LiteralPath $HelperUnderTest -Destination $FixtureHelper

    $WindowsHost = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
    $FakeToolSuffix = if ($WindowsHost) { '.exe' } else { '' }
    foreach ($Tool in @('clang', 'clang++', 'llvm-ar', 'llvm-dlltool')) {
        [System.IO.File]::WriteAllBytes(
            (Join-Path $FakeMsysBin "$Tool$FakeToolSuffix"),
            [byte[]]@(0x4d, 0x5a)
        )
    }

    if ($WindowsHost) {
        $FakePkgConfigCommand = Join-Path $FakeMsysBin 'pkg-config.cmd'
        $PkgConfigSource = @'
@echo off
>>"%BALUN_WINDOWS_TEST_PKG_LOG%" echo pkg-config ^<%1^> ^<%2^> ^<%3^>
if not "%BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE%"=="" if /I not "%~3"=="%BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE%" exit /b 0
exit /b %BALUN_WINDOWS_FAKE_PKG_STATUS%
'@
        [System.IO.File]::WriteAllText(
            $FakePkgConfigCommand,
            $PkgConfigSource,
            [System.Text.Encoding]::ASCII
        )
    }
    else {
        $FakePkgConfigCommand = Join-Path $FakeMsysBin 'pkg-config'
        $PkgConfigSource = @'
#!/bin/sh
printf 'pkg-config <%s> <%s> <%s>\n' "$1" "$2" "$3" >> "$BALUN_WINDOWS_TEST_PKG_LOG"
if [ -n "$BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE" ] && [ "$3" != "$BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE" ]; then
    exit 0
fi
exit "$BALUN_WINDOWS_FAKE_PKG_STATUS"
'@
        [System.IO.File]::WriteAllText(
            $FakePkgConfigCommand,
            $PkgConfigSource,
            [System.Text.UTF8Encoding]::new($false)
        )
        [System.IO.File]::SetUnixFileMode(
            $FakePkgConfigCommand,
            [System.IO.UnixFileMode]::UserRead -bor
                [System.IO.UnixFileMode]::UserWrite -bor
                [System.IO.UnixFileMode]::UserExecute
        )
    }

    $RunnerSource = @'
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$env:PATH = $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH

function global:rustc {
    if ($args -contains 'target-libdir') {
        # Read-only target probes are recorded separately from command routing
        # so desktop routes can assert which target they verified.
        $ProbeArguments = @($args)
        $TargetIndex = [array]::IndexOf($ProbeArguments, '--target')
        $ProbedTarget = if ($TargetIndex -ge 0 -and $TargetIndex + 1 -lt $ProbeArguments.Count) {
            [string]$ProbeArguments[$TargetIndex + 1]
        }
        else {
            '<missing>'
        }
        [System.IO.File]::AppendAllText(
            $env:BALUN_WINDOWS_TEST_TARGET_PROBE_LOG,
            "target-libdir <$ProbedTarget>`n"
        )
        Write-Output $env:BALUN_WINDOWS_FAKE_TARGET_LIBDIR
        $global:LASTEXITCODE = 0
        return
    }
    $RenderedArguments = @($args | ForEach-Object { "<$($_.ToString())>" }) -join ' '
    [System.IO.File]::AppendAllText(
        $env:BALUN_WINDOWS_TEST_LOG,
        "rustc $RenderedArguments`n"
    )
    Write-Output $env:BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE
    $global:LASTEXITCODE = [int]$env:BALUN_WINDOWS_FAKE_RUSTC_STATUS
}

function global:cargo {
    $RenderedArguments = @($args | ForEach-Object { "<$($_.ToString())>" }) -join ' '
    [System.IO.File]::AppendAllText(
        $env:BALUN_WINDOWS_TEST_LOG,
        "cargo $RenderedArguments`n"
    )

    $TargetToken = 'x86_64_pc_windows_gnullvm'
    $PathFirst = @($env:PATH -split [regex]::Escape(
        [string][System.IO.Path]::PathSeparator
    ))[0]
    [System.IO.File]::WriteAllLines(
        $env:BALUN_WINDOWS_TEST_ENV_LOG,
        @(
            "CARGO_BUILD_TARGET=<$env:CARGO_BUILD_TARGET>",
            "CARGO_TARGET_DIR=<$env:CARGO_TARGET_DIR>",
            "CARGO_LLVM_COV_TARGET_DIR=<$env:CARGO_LLVM_COV_TARGET_DIR>",
            "CARGO_LLVM_COV_BUILD_DIR=<$env:CARGO_LLVM_COV_BUILD_DIR>",
            "PKG_CONFIG=<$env:PKG_CONFIG>",
            "PKG_CONFIG_PATH=<$env:PKG_CONFIG_PATH>",
            "PKG_CONFIG_LIBDIR=<$env:PKG_CONFIG_LIBDIR>",
            "PKG_CONFIG_ALLOW_CROSS=<$env:PKG_CONFIG_ALLOW_CROSS>",
            "TARGET_PKG_CONFIG=<$([Environment]::GetEnvironmentVariable("PKG_CONFIG_$TargetToken"))>",
            "TARGET_PKG_CONFIG_PATH=<$([Environment]::GetEnvironmentVariable("PKG_CONFIG_PATH_$TargetToken"))>",
            "TARGET_PKG_CONFIG_LIBDIR=<$([Environment]::GetEnvironmentVariable("PKG_CONFIG_LIBDIR_$TargetToken"))>",
            "TARGET_PKG_CONFIG_ALLOW_CROSS=<$([Environment]::GetEnvironmentVariable("PKG_CONFIG_ALLOW_CROSS_$TargetToken"))>",
            "PATH_FIRST=<$PathFirst>",
            "CC=<$([Environment]::GetEnvironmentVariable("CC_$TargetToken"))>",
            "CXX=<$([Environment]::GetEnvironmentVariable("CXX_$TargetToken"))>",
            "AR=<$([Environment]::GetEnvironmentVariable("AR_$TargetToken"))>",
            "DLLTOOL=<$([Environment]::GetEnvironmentVariable("DLLTOOL_$TargetToken"))>"
        )
    )

    $Status = [int]$env:BALUN_WINDOWS_FAKE_CARGO_STATUS
    if ($args.Count -ge 2 -and
        $args[0].ToString() -ceq 'llvm-cov' -and
        $args[1].ToString() -ceq '--version') {
        Write-Output $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION
    }

    if ($Status -eq 0 -and
        $args.Count -gt 0 -and
        $args[0].ToString() -ceq 'build' -and
        [int]$env:BALUN_WINDOWS_FAKE_SKIP_BINARY -eq 0) {
        $TargetDirectoryIndex = [Array]::IndexOf([object[]]$args, '--target-dir')
        if ($TargetDirectoryIndex -lt 0 -or
            $TargetDirectoryIndex + 1 -ge $args.Count) {
            throw 'fake Cargo build requires --target-dir'
        }
        $ArtifactDirectory = $args[$TargetDirectoryIndex + 1].ToString()

        $TargetIndex = [Array]::IndexOf([object[]]$args, '--target')
        if ($TargetIndex -ge 0) {
            $ArtifactDirectory = Join-Path $ArtifactDirectory $args[$TargetIndex + 1].ToString()
        }
        $ArtifactDirectory = Join-Path $ArtifactDirectory 'release'
        [System.IO.Directory]::CreateDirectory($ArtifactDirectory) | Out-Null

        $BinaryName = if (@($args | ForEach-Object { $_.ToString() }) -ccontains 'balun-discover') {
            'balun-discover.exe'
        }
        else {
            'balun.exe'
        }
        $ArtifactPath = Join-Path $ArtifactDirectory $BinaryName

        if ([int]$env:BALUN_WINDOWS_FAKE_DIRECTORY_BINARY -eq 1) {
            [System.IO.Directory]::CreateDirectory($ArtifactPath) | Out-Null
        }
        elseif ([int]$env:BALUN_WINDOWS_FAKE_ZERO_BINARY -eq 1) {
            $EmptyArtifact = [System.IO.File]::Create($ArtifactPath)
            $EmptyArtifact.Dispose()
        }
        elseif (-not [string]::IsNullOrWhiteSpace($env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY)) {
            Copy-Item -LiteralPath $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY -Destination $ArtifactPath
        }
        else {
            [System.IO.File]::WriteAllBytes(
                $ArtifactPath,
                [byte[]]@(0x4d, 0x5a)
            )
        }
    }

    $global:LASTEXITCODE = $Status
}

if ([int]$env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE -ne 1) {
    Remove-Item -LiteralPath Function:\cargo -Force
}
if ([int]$env:BALUN_WINDOWS_FAKE_RUSTC_AVAILABLE -ne 1) {
    Remove-Item -LiteralPath Function:\rustc -Force
}

& $env:BALUN_WINDOWS_HELPER @args
exit $global:LASTEXITCODE
'@
    [System.IO.File]::WriteAllText($Runner, $RunnerSource)
    foreach ($Log in @($CommandLog, $EnvironmentLog, $PkgConfigLog, $TargetProbeLog)) {
        [System.IO.File]::WriteAllText($Log, '')
    }

    $env:BALUN_WINDOWS_HELPER = $FixtureHelper
    $env:BALUN_WINDOWS_TEST_LOG = $CommandLog
    $env:BALUN_WINDOWS_TEST_ENV_LOG = $EnvironmentLog
    $env:BALUN_WINDOWS_TEST_PKG_LOG = $PkgConfigLog
    $env:BALUN_WINDOWS_TEST_TARGET_PROBE_LOG = $TargetProbeLog
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'
    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '1'
    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 0.8.7'
    $env:BALUN_WINDOWS_FAKE_RUSTC_STATUS = '0'
    $env:BALUN_WINDOWS_FAKE_RUSTC_AVAILABLE = '1'
    $env:BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE = 'x86_64-pc-windows-msvc'
    $env:BALUN_WINDOWS_FAKE_TARGET_LIBDIR = $FixtureRoot
    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '0'
    $env:BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE = ''
    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '0'
    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '0'
    $env:BALUN_WINDOWS_FAKE_DIRECTORY_BINARY = '0'
    $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = ''
    $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH = $RestrictedPath
    $env:CARGO_BUILD_TARGET = 'i686-pc-windows-msvc'
    $env:CARGO_LLVM_COV_BUILD_DIR = Join-Path $TemporaryRoot 'caller llvm-cov build override'
    $env:CARGO_LLVM_COV_TARGET_DIR = Join-Path $TemporaryRoot 'caller llvm-cov target override'
    $env:CARGO_TARGET_DIR = Join-Path $TemporaryRoot 'caller cargo target override'
    $env:MSYS2_ROOT = $FakeMsysRoot
    $env:RUST_TARGET = $DesktopTarget

    Invoke-TestHelper -Arguments @('-Help')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'A lightweight cross-platform HDHomeRun live TV viewer'
    Assert-ExpectedOutput 'Application ID: io.github.jm2.Balun'
    Assert-ExpectedOutput 'Windows desktop build helper'
    Assert-ExpectedOutput 'InspectLocal'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    foreach ($HelpArgument in @('--help', '-h')) {
        Invoke-TestHelper -Arguments @($HelpArgument)
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Windows desktop build helper'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    $UnavailableCases = @(
        [pscustomobject]@{ Name = '-Bundle'; Arguments = @('-Bundle') },
        [pscustomobject]@{ Name = '-Zip'; Arguments = @('-Zip') },
        [pscustomobject]@{ Name = '-InnoSetup'; Arguments = @('-InnoSetup') },
        [pscustomobject]@{ Name = '-Package'; Arguments = @('-Package') },
        [pscustomobject]@{ Name = '-Installer'; Arguments = @('-Installer') },
        [pscustomobject]@{ Name = '-SkipBundle'; Arguments = @('-SkipBundle') },
        [pscustomobject]@{ Name = '-NoCargoBuild'; Arguments = @('-NoCargoBuild') },
        [pscustomobject]@{ Name = '-CargoUpdate'; Arguments = @('-CargoUpdate') },
        [pscustomobject]@{
            Name = '-CargoUpdateArgs'
            Arguments = @('-CargoUpdateArgs', '-p example')
        }
    )
    foreach ($Case in $UnavailableCases) {
        Invoke-TestHelper -Arguments $Case.Arguments
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput $Case.Name
        Assert-ExpectedOutput 'no external work was started'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    foreach ($FalseSwitch in @(
        '-Bundle:$false',
        '-Zip:$false',
        '-InnoSetup:$false',
        '-Package:$false',
        '-Installer:$false',
        '-SkipBundle:$false',
        '-NoCargoBuild:$false',
        '-CargoUpdate:$false'
    )) {
        $SwitchName = $FalseSwitch.Substring(0, $FalseSwitch.IndexOf(':'))
        Invoke-TestHelper -Arguments @($FalseSwitch)
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput "Unavailable option(s): $SwitchName"
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    Invoke-TestHelper -Arguments @('-Help', '-Zip')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Zip'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Check', '-Clippy')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Quick-exit modes cannot be combined'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Run', '-Check')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Run cannot be combined'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Run', '-Diagnostic')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'cannot be combined with -Diagnostic'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Run', '-InspectLocal')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'mutually exclusive launch operations'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    foreach ($QuickMode in @('-Fmt', '-Check', '-Clippy', '-Test', '-Coverage')) {
        Invoke-TestHelper -Arguments @('-InspectLocal', $QuickMode)
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput '-InspectLocal cannot be combined with quick-exit mode'
        Assert-ExpectedOutput $QuickMode
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    Invoke-TestHelper -Arguments @('-InspectLocal', '-Msys2Root', $FakeMsysRoot)
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Msys2Root cannot be combined with GTK-free -InspectLocal'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-InspectLocal', '-Bundle')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Unavailable option(s): -Bundle'
    Assert-ExpectedOutput 'no external work was started'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-InspectLocal', '-CargoUpdateArgs', '-p example')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Unavailable option(s): -CargoUpdateArgs'
    Assert-ExpectedOutput 'no external work was started'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-Diagnostic', '-Msys2Root', $FakeMsysRoot)
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Msys2Root applies only to desktop compilation'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('unexpected-positional-argument')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Unknown argument(s): unexpected-positional-argument'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-InspectLocal', '--remote')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Unknown argument(s): --remote'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '0'
    Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'cargo is unavailable'
    Assert-EmptyLog $CommandLog 'Cargo'
    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '1'

    Invoke-TestHelper -Arguments @('-Fmt')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog 'cargo <fmt> <--all>'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    $DesktopBuildCommand = (
        'cargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    $DiagnosticBuildCommand = (
        'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Desktop output:'
    Assert-ExpectedOutput 'balun.exe'
    Assert-ExpectedOutput 'GStreamer runtime plugin checks passed'
    Assert-ExpectedLog $DesktopBuildCommand
    Assert-ExpectedPkgConfigProbeSet
    Assert-DesktopTargetProbe
    Assert-DesktopEnvironment

    # The desktop build fails closed before any Cargo work when a structural
    # runtime plugin is missing; quick modes never consult runtime plugins.
    $FakeGtk4Plugin = Join-Path $FakePluginDirectory 'libgstgtk4.dll'
    Remove-Item -LiteralPath $FakeGtk4Plugin -Force
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'Required GStreamer playback runtime is incomplete'
    Assert-ExpectedOutput 'libgstgtk4.dll (gtk4paintablesink) from mingw-w64-clang-x86_64-gst-plugins-rs'
    Assert-ExpectedPkgConfigProbeSet
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    [System.IO.File]::WriteAllBytes($FakeGtk4Plugin, [byte[]]@(0x4d, 0x5a))

    $FakeDemuxPlugin = Join-Path $FakePluginDirectory 'libgstmpegtsdemux.dll'
    Remove-Item -LiteralPath $FakeDemuxPlugin -Force
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'libgstmpegtsdemux.dll (tsdemux) from mingw-w64-clang-x86_64-gst-plugins-bad'
    Assert-EmptyLog $CommandLog 'Cargo'
    [System.IO.File]::WriteAllBytes($FakeDemuxPlugin, [byte[]]@(0x4d, 0x5a))

    $FakeLibavPlugin = Join-Path $FakePluginDirectory 'libgstlibav.dll'
    Remove-Item -LiteralPath $FakeLibavPlugin -Force
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Desktop output:'
    Assert-ExpectedLog $DesktopBuildCommand
    [System.IO.File]::WriteAllBytes($FakeLibavPlugin, [byte[]]@(0x4d, 0x5a))

    Invoke-TestHelper -Arguments @('-Msys2Root', $FakeMsysRoot)
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput "Using MSYS2 CLANG64 at $FakeMsysPrefix"
    Assert-ExpectedLog $DesktopBuildCommand
    Assert-DesktopEnvironment

    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    Assert-ExpectedPkgConfigProbeSet

    Invoke-TestHelper -Arguments @('-Clippy')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <clippy> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget> " +
        "<--> <-D> <warnings>`n" +
        'cargo <clippy> <--release> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget> " +
        '<--> <-D> <warnings>'
    )

    # The desktop routes verify the gnullvm Rust target read-only before any
    # MSYS2, pkg-config, or Cargo work; diagnostic routes never probe it.
    $env:BALUN_WINDOWS_FAKE_TARGET_LIBDIR = Join-Path $TemporaryRoot 'missing-target-libdir'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput "Rust target $DesktopTarget is not installed"
    Assert-ExpectedOutput 'this helper never installs targets'
    Assert-DesktopTargetProbe
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput "Rust target $DesktopTarget is not installed"
    Assert-DesktopTargetProbe
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    Assert-EmptyLog $TargetProbeLog 'Rust target probe'
    $env:BALUN_WINDOWS_FAKE_TARGET_LIBDIR = $FixtureRoot

    Invoke-TestHelper -Arguments @('-Test')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <test> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )

    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        "cargo <llvm-cov> <--version>`n" +
        'cargo <llvm-cov> <--all-targets> <--all-features> <--locked> ' +
        "<--target> <$DesktopTarget> <--summary-only>"
    )
    Assert-ExpectedPkgConfigProbeSet
    Assert-CoverageEnvironment

    Invoke-TestHelper -Arguments @('-Diagnostic', '-Coverage')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        "cargo <llvm-cov> <--version>`n" +
        'cargo <llvm-cov> <--all-targets> <--no-default-features> <--locked> ' +
        "<--target> <$DesktopTarget> <--summary-only>"
    )
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    Assert-CoverageEnvironment

    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 9.9.9'
    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
    Assert-ExpectedLog 'cargo <llvm-cov> <--version>'
    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 0.8.7'

    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '26'
    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
    Assert-ExpectedLog 'cargo <llvm-cov> <--version>'
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'

    $env:MSYS2_ROOT = Join-Path $TemporaryRoot 'missing-and-unused'
    Invoke-TestHelper -Arguments @('-Diagnostic')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Diagnostic output:'
    Assert-ExpectedOutput 'balun-discover.exe'
    Assert-ExpectedLog $DiagnosticBuildCommand
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    if ($WindowsHost) {
        # where.exe treats the two fixed diagnostic arguments as independent
        # current-directory patterns and resolves these PATHEXT fixtures only
        # when both arguments reach the validated executable unchanged.
        $InspectArgumentPath = Join-Path $FixtureRoot '--inspect.exe'
        $LocalArgumentPath = Join-Path $FixtureRoot '--local.exe'
        [System.IO.File]::WriteAllText($InspectArgumentPath, '')
        [System.IO.File]::WriteAllText($LocalArgumentPath, '')
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = Join-Path $env:SystemRoot 'System32\where.exe'

        Invoke-TestHelper -Arguments @('-InspectLocal')
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Diagnostic output:'
        Assert-ExpectedOutput (
            Join-Path $FixtureTargetRoot "$DesktopTarget\release\balun-discover.exe"
        )
        Assert-ExpectedOutput 'Inspecting local HDHomeRun discovery'
        Assert-ExpectedOutput $InspectArgumentPath
        Assert-ExpectedOutput $LocalArgumentPath
        Assert-ExpectedLog $DiagnosticBuildCommand
        Assert-EmptyLog $PkgConfigLog 'pkg-config'

        Invoke-TestHelper -Arguments @('-InspectLocal', '-Diagnostic')
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput $InspectArgumentPath
        Assert-ExpectedOutput $LocalArgumentPath
        Assert-ExpectedLog $DiagnosticBuildCommand
        Assert-EmptyLog $PkgConfigLog 'pkg-config'

        $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '1'
        Invoke-TestHelper -Arguments @('-InspectLocal')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
        Assert-ExpectedLog $DiagnosticBuildCommand
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '0'
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = ''
    }
    else {
        Invoke-TestHelper -Arguments @('-InspectLocal')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput '-InspectLocal can run the Windows diagnostic only from Windows'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    $env:MSYS2_ROOT = $FakeMsysRoot

    $env:RUST_TARGET = ''
    if ($WindowsHost) {
        $NativeDiagnosticTarget = $env:BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE
        $NativeDiagnosticBuildCommand = (
            'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
            "<--target-dir> <$FixtureTargetRoot> <--target> <$NativeDiagnosticTarget>"
        )
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 0
        Assert-ExpectedLog (
            "rustc <--print> <host-tuple>`n$NativeDiagnosticBuildCommand"
        )
        Assert-ExpectedOutput (
            Join-Path $FixtureTargetRoot (
                "$NativeDiagnosticTarget\release\balun-discover.exe"
            )
        )
        Assert-EmptyLog $PkgConfigLog 'pkg-config'

        $env:BALUN_WINDOWS_FAKE_RUSTC_STATUS = '37'
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'rustc --print host-tuple did not return one bounded Windows Rust target'
        Assert-ExpectedLog 'rustc <--print> <host-tuple>'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        $env:BALUN_WINDOWS_FAKE_RUSTC_STATUS = '0'

        $env:BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE = 'x86_64-unknown-linux-gnu'
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'rustc --print host-tuple did not return one bounded Windows Rust target'
        Assert-ExpectedLog 'rustc <--print> <host-tuple>'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        $env:BALUN_WINDOWS_FAKE_RUSTC_HOST_TUPLE = $NativeDiagnosticTarget

        $env:BALUN_WINDOWS_FAKE_RUSTC_AVAILABLE = '0'
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'rustc is unavailable'
        Assert-EmptyLog $CommandLog 'Cargo or rustc'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        $env:BALUN_WINDOWS_FAKE_RUSTC_AVAILABLE = '1'

        Invoke-TestHelper -Arguments @()
        Assert-ExpectedStatus 0
        Assert-ExpectedLog $DesktopBuildCommand
        Assert-ExpectedPkgConfigProbeSet
    }
    else {
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput 'A non-Windows host must set RUST_TARGET'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }
    $env:RUST_TARGET = $DesktopTarget

    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '31'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'gtk4 >= 4.16 was not found'
    Assert-ExpectedPkgConfigGtkProbe
    Assert-EmptyLog $CommandLog 'Cargo'
    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '0'

    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '32'
    $env:BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE = 'libadwaita-1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'libadwaita-1 >= 1.6 was not found'
    Assert-ExpectedPkgConfigGtkAdwaitaProbes
    Assert-EmptyLog $CommandLog 'Cargo'

    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '33'
    $env:BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE = 'gstreamer-1.0'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'gstreamer-1.0 >= 1.20 was not found'
    Assert-ExpectedPkgConfigProbeSet
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget>"
    )
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    $env:BALUN_WINDOWS_FAKE_PKG_STATUS = '0'
    $env:BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE = ''

    Invoke-TestHelper -Arguments @('-Msys2Root', 'relative-msys2')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Msys2Root must be an absolute filesystem path'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-Msys2Root', $IncompleteMsysRoot)
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'MSYS2 CLANG64 is incomplete'
    Assert-ExpectedOutput 'missing:'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '24'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'cargo build failed with exit code 24'
    Assert-ExpectedLog $DesktopBuildCommand
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'

    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
    Assert-ExpectedLog $DesktopBuildCommand
    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '0'

    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
    Assert-ExpectedLog $DesktopBuildCommand
    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '0'

    $env:BALUN_WINDOWS_FAKE_DIRECTORY_BINARY = '1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
    Assert-ExpectedLog $DesktopBuildCommand
    $env:BALUN_WINDOWS_FAKE_DIRECTORY_BINARY = '0'

    foreach ($InvalidTarget in @(
        '..\unsafe-windows-target',
        'x86_64-unknown-linux-gnu',
        'x86_64-pc-windows-msvc/..',
        '-windows-msvc',
        (('x' * 116) + '-windows-msvc')
    )) {
        $env:RUST_TARGET = $InvalidTarget
        Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput 'RUST_TARGET must name one bounded Windows Rust target'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    $env:RUST_TARGET = 'x86_64-pc-windows-msvc'
    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput "requires RUST_TARGET=$DesktopTarget"
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-Diagnostic', '-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <x86_64-pc-windows-msvc>"
    )
    Assert-EmptyLog $PkgConfigLog 'pkg-config'
    $env:RUST_TARGET = $DesktopTarget

    $env:MSYS2_ROOT = ''
    $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH = $FakeMsysBin
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput "Using MSYS2 CLANG64 at $FakeMsysPrefix"
    Assert-ExpectedLog $DesktopBuildCommand
    Assert-ExpectedPkgConfigProbeSet
    $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH = $RestrictedPath
    $env:MSYS2_ROOT = $FakeMsysRoot

    if ($WindowsHost) {
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = Join-Path $env:SystemRoot 'System32\whoami.exe'
        Invoke-TestHelper -Arguments @('-Run')
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Launching'
        Assert-ExpectedLog $DesktopBuildCommand
        Assert-ExpectedPkgConfigProbeSet
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = ''
    }
    else {
        Invoke-TestHelper -Arguments @('-Run')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput '-Run can launch the Windows desktop application only from Windows'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    $HelperText = [System.IO.File]::ReadAllText($HelperUnderTest)
    foreach ($RequiredText in @(
        'A lightweight cross-platform HDHomeRun live TV viewer',
        'io.github.jm2.Balun',
        "'--features',",
        "'desktop',",
        "'gstreamer-1.0'",
        "'1.20'",
        "'libgstgtk4.dll'",
        "'gst-plugins-rs'",
        '& $BinaryItem.FullName',
        "& `$BinaryItem.FullName '--inspect' '--local'"
    )) {
        if (-not $HelperText.Contains($RequiredText)) {
            Assert-RoutingTestFailure "helper is missing required text: $RequiredText"
        }
    }
    if ($HelperText -match '(?im)^\s*(cargo\s+install|rustup\s+(target|component)\s+add|winget|choco|pacman)(\s|$)' -or
        $HelperText -match '(?i)\b(Copy-Item|Compress-Archive|Expand-Archive|Invoke-WebRequest|Invoke-RestMethod|Start-BitsTransfer|Start-Process|curl|wget|git)\b') {
        Assert-RoutingTestFailure 'helper contains installer, downloader, archive, runtime-copy, or detached-launch logic'
    }

    Write-Output 'build-windows desktop command-routing tests passed'
}
finally {
    foreach ($Name in $EnvironmentNames) {
        [Environment]::SetEnvironmentVariable(
            $Name,
            $OriginalEnvironment[$Name],
            [EnvironmentVariableTarget]::Process
        )
    }
    if (Test-Path -LiteralPath $TemporaryRoot) {
        Remove-Item -LiteralPath $TemporaryRoot -Recurse -Force
    }
}

# GitHub Actions dot-sources PowerShell run blocks. Failed child-process probes
# intentionally leave LASTEXITCODE nonzero even after every assertion passes;
# clear it so the hosting shell receives the test suite's actual success.
$global:LASTEXITCODE = 0
