<#
Deterministic command-routing tests for scripts/build-windows.ps1.

Each invocation runs in a fresh child PowerShell process with fake Cargo and
MSYS2 CLANG64/CLANGARM64 commands. No compiler, installer, package manager, network
access, GUI toolkit, or Balun artifact is required.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ScriptDirectory = Split-Path -Parent $PSCommandPath
$HelperUnderTest = Join-Path $ScriptDirectory 'build-windows.ps1'
$InnoRecipeUnderTest = Join-Path $ScriptDirectory '..\build-aux\inno\balun.iss'
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
$FakeArmMsysPrefix = Join-Path $FakeMsysRoot 'clangarm64'
$FakeArmMsysBin = Join-Path $FakeArmMsysPrefix 'bin'
$FakeArmPkgConfigDirectory = Join-Path $FakeArmMsysPrefix 'lib\pkgconfig'
$FakeArmPluginDirectory = Join-Path $FakeArmMsysPrefix 'lib\gstreamer-1.0'
$IncompleteMsysRoot = Join-Path $TemporaryRoot 'incomplete-msys2'
$PowerShellExecutable = (Get-Process -Id $PID).Path
$DesktopTarget = 'x86_64-pc-windows-gnullvm'
$ArmDesktopTarget = 'aarch64-pc-windows-gnullvm'
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
    'BALUN_WINDOWS_FAKE_PE_MACHINE',
    'BALUN_WINDOWS_FAKE_NATIVE_PROBE',
    'BALUN_WINDOWS_FAKE_PROCESS_MACHINE',
    'BALUN_WINDOWS_FAKE_NATIVE_MACHINE',
    'BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS',
    'BALUN_WINDOWS_TEST_RESTRICTED_PATH',
    'CARGO_BUILD_TARGET',
    'CARGO_LLVM_COV_BUILD_DIR',
    'CARGO_LLVM_COV_TARGET_DIR',
    'CARGO_TARGET_DIR',
    'MSYS2_ROOT',
    'MSYS_ENV',
    'MSYSTEM',
    'MINGW_PACKAGE_PREFIX',
    'INNO_ARCH',
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
    <#
    .SYNOPSIS
        Require exactly one recorded rustc target probe for the desktop target.
    #>
    param([string]$ExpectedTarget = $DesktopTarget)

    $Lines = @([System.IO.File]::ReadAllLines($TargetProbeLog))
    if ($Lines.Count -ne 1 -or $Lines[0] -cne "target-libdir <$ExpectedTarget>") {
        Assert-RoutingTestFailure "unexpected Rust target probe: $($Lines -join '; ')"
    }
}

function Assert-DesktopEnvironment {
    param(
        [string]$ExpectedPkgConfig = $FakePkgConfigCommand,
        [string]$ExpectedPkgConfigDirectory = $FakePkgConfigDirectory,
        [string]$ExpectedBin = $FakeMsysBin
    )

    $EnvironmentText = [System.IO.File]::ReadAllText($EnvironmentLog)
    foreach ($Expected in @(
        "PKG_CONFIG=<$ExpectedPkgConfig>",
        "PKG_CONFIG_PATH=<$ExpectedPkgConfigDirectory>",
        "PKG_CONFIG_LIBDIR=<$ExpectedPkgConfigDirectory>",
        'PKG_CONFIG_ALLOW_CROSS=<1>',
        "TARGET_PKG_CONFIG=<$ExpectedPkgConfig>",
        "TARGET_PKG_CONFIG_PATH=<$ExpectedPkgConfigDirectory>",
        "TARGET_PKG_CONFIG_LIBDIR=<$ExpectedPkgConfigDirectory>",
        'TARGET_PKG_CONFIG_ALLOW_CROSS=<1>',
        "PATH_FIRST=<$ExpectedBin>",
        "CC=<$ExpectedBin$([System.IO.Path]::DirectorySeparatorChar)clang$FakeToolSuffix>",
        "CXX=<$ExpectedBin$([System.IO.Path]::DirectorySeparatorChar)clang++$FakeToolSuffix>",
        "AR=<$ExpectedBin$([System.IO.Path]::DirectorySeparatorChar)llvm-ar$FakeToolSuffix>",
        "DLLTOOL=<$ExpectedBin$([System.IO.Path]::DirectorySeparatorChar)llvm-dlltool$FakeToolSuffix>"
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

function New-FakePeFile {
    param([string]$Path, [uint16]$Machine)

    $Bytes = [byte[]]::new(0x86)
    $Bytes[0] = 0x4D
    $Bytes[1] = 0x5A
    [BitConverter]::GetBytes([uint32]0x80).CopyTo($Bytes, 0x3C)
    $Bytes[0x80] = 0x50
    $Bytes[0x81] = 0x45
    [BitConverter]::GetBytes($Machine).CopyTo($Bytes, 0x84)
    [System.IO.File]::WriteAllBytes($Path, $Bytes)
}

function Set-DesktopProfileEnvironment {
    param([ValidateSet('x86_64', 'aarch64')][string]$Architecture)

    if ($Architecture -ceq 'aarch64') {
        $env:RUST_TARGET = $ArmDesktopTarget
        $env:MSYS_ENV = 'clangarm64'
        $env:MSYSTEM = 'CLANGARM64'
        $env:MINGW_PACKAGE_PREFIX = 'mingw-w64-clang-aarch64'
        $env:INNO_ARCH = 'arm64'
    }
    else {
        $env:RUST_TARGET = $DesktopTarget
        $env:MSYS_ENV = 'clang64'
        $env:MSYSTEM = 'CLANG64'
        $env:MINGW_PACKAGE_PREFIX = 'mingw-w64-clang-x86_64'
        $env:INNO_ARCH = 'x64'
    }
}

function Get-NativeDesktopTestArchitecture {
    $OsArchitecture = (
        [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    ).ToString()
    switch ($OsArchitecture) {
        'X64' { return 'x86_64' }
        'Arm64' { return 'aarch64' }
        default {
            Assert-RoutingTestFailure (
                "unsupported Windows test host architecture: $OsArchitecture"
            )
        }
    }
}

function Clear-DesktopProfileDeclarations {
    foreach ($Name in @('MSYS_ENV', 'MSYSTEM', 'MINGW_PACKAGE_PREFIX', 'INNO_ARCH')) {
        [Environment]::SetEnvironmentVariable(
            $Name,
            '',
            [EnvironmentVariableTarget]::Process
        )
    }
}

try {
    [System.IO.Directory]::CreateDirectory($FixtureScripts) | Out-Null
    [System.IO.Directory]::CreateDirectory($RestrictedPath) | Out-Null
    [System.IO.Directory]::CreateDirectory($IncompleteMsysRoot) | Out-Null
    Copy-Item -LiteralPath $HelperUnderTest -Destination $FixtureHelper

    $WindowsHost = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
    $FakeToolSuffix = if ($WindowsHost) { '.exe' } else { '' }
    $FakeProfiles = @(
        [pscustomobject]@{
            Target = $DesktopTarget
            Bin = $FakeMsysBin
            PkgConfigDirectory = $FakePkgConfigDirectory
            PluginDirectory = $FakePluginDirectory
            Machine = [uint16]0x8664
        },
        [pscustomobject]@{
            Target = $ArmDesktopTarget
            Bin = $FakeArmMsysBin
            PkgConfigDirectory = $FakeArmPkgConfigDirectory
            PluginDirectory = $FakeArmPluginDirectory
            Machine = [uint16]0xAA64
        }
    )
    foreach ($FakeProfile in $FakeProfiles) {
        [System.IO.Directory]::CreateDirectory($FakeProfile.Bin) | Out-Null
        [System.IO.Directory]::CreateDirectory($FakeProfile.PkgConfigDirectory) | Out-Null
        [System.IO.Directory]::CreateDirectory($FakeProfile.PluginDirectory) | Out-Null
        foreach ($Plugin in @(
            'libgstcoreelements', 'libgstplayback', 'libgstapp', 'libgsttypefindfunctions',
            'libgstdeinterlace', 'libgstmpegtsdemux', 'libgstgtk4', 'libgstlibav'
        )) {
            New-FakePeFile `
                (Join-Path $FakeProfile.PluginDirectory "$Plugin.dll") `
                $FakeProfile.Machine
        }
        foreach ($Tool in @('clang', 'clang++', 'llvm-ar', 'llvm-dlltool')) {
            New-FakePeFile `
                (Join-Path $FakeProfile.Bin "$Tool$FakeToolSuffix") `
                $FakeProfile.Machine
        }
    }

    if ($WindowsHost) {
        $FakePkgConfigCommand = Join-Path $FakeMsysBin 'pkg-config.cmd'
        $FakeArmPkgConfigCommand = Join-Path $FakeArmMsysBin 'pkg-config.cmd'
        $PkgConfigSource = @'
@echo off
>>"%BALUN_WINDOWS_TEST_PKG_LOG%" echo pkg-config ^<%1^> ^<%2^> ^<%3^>
if not "%BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE%"=="" if /I not "%~3"=="%BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE%" exit /b 0
exit /b %BALUN_WINDOWS_FAKE_PKG_STATUS%
'@
        foreach ($PkgConfigCommand in @($FakePkgConfigCommand, $FakeArmPkgConfigCommand)) {
            [System.IO.File]::WriteAllText(
                $PkgConfigCommand,
                $PkgConfigSource,
                [System.Text.Encoding]::ASCII
            )
        }
    }
    else {
        $FakePkgConfigCommand = Join-Path $FakeMsysBin 'pkg-config'
        $FakeArmPkgConfigCommand = Join-Path $FakeArmMsysBin 'pkg-config'
        $PkgConfigSource = @'
#!/bin/sh
printf 'pkg-config <%s> <%s> <%s>\n' "$1" "$2" "$3" >> "$BALUN_WINDOWS_TEST_PKG_LOG"
if [ -n "$BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE" ] && [ "$3" != "$BALUN_WINDOWS_FAKE_PKG_FAIL_PACKAGE" ]; then
    exit 0
fi
exit "$BALUN_WINDOWS_FAKE_PKG_STATUS"
'@
        foreach ($PkgConfigCommand in @($FakePkgConfigCommand, $FakeArmPkgConfigCommand)) {
            [System.IO.File]::WriteAllText(
                $PkgConfigCommand,
                $PkgConfigSource,
                [System.Text.UTF8Encoding]::new($false)
            )
            [System.IO.File]::SetUnixFileMode(
                $PkgConfigCommand,
                [System.IO.UnixFileMode]::UserRead -bor
                    [System.IO.UnixFileMode]::UserWrite -bor
                    [System.IO.UnixFileMode]::UserExecute
            )
        }
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

    $TargetIndex = [Array]::IndexOf([object[]]$args, '--target')
    $CargoTarget = if ($TargetIndex -ge 0 -and $TargetIndex + 1 -lt $args.Count) {
        $args[$TargetIndex + 1].ToString()
    }
    else {
        'x86_64-pc-windows-gnullvm'
    }
    $TargetToken = $CargoTarget.Replace('-', '_').Replace('.', '_')
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

        if ($TargetIndex -ge 0) {
            $ArtifactDirectory = Join-Path $ArtifactDirectory $CargoTarget
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
            $Machine = if (-not [string]::IsNullOrWhiteSpace(
                $env:BALUN_WINDOWS_FAKE_PE_MACHINE
            )) {
                [Convert]::ToUInt16($env:BALUN_WINDOWS_FAKE_PE_MACHINE, 16)
            }
            elseif ($CargoTarget.StartsWith('aarch64-', [StringComparison]::Ordinal)) {
                [uint16]0xAA64
            }
            else {
                [uint16]0x8664
            }
            $Bytes = [byte[]]::new(0x86)
            $Bytes[0] = 0x4D
            $Bytes[1] = 0x5A
            [BitConverter]::GetBytes([uint32]0x80).CopyTo($Bytes, 0x3C)
            $Bytes[0x80] = 0x50
            $Bytes[0x81] = 0x45
            [BitConverter]::GetBytes([uint16]$Machine).CopyTo($Bytes, 0x84)
            [System.IO.File]::WriteAllBytes($ArtifactPath, $Bytes)
        }
    }

    $global:LASTEXITCODE = $Status
}

if ([int]$env:BALUN_WINDOWS_FAKE_NATIVE_PROBE -eq 1) {
    Add-Type -TypeDefinition @"
using System;

namespace Balun.Windows
{
    public static class NativeArchitectureProbe
    {
        public static bool Query(out ushort processMachine, out ushort nativeMachine)
        {
            processMachine = Convert.ToUInt16(
                Environment.GetEnvironmentVariable(
                    "BALUN_WINDOWS_FAKE_PROCESS_MACHINE"
                ),
                16
            );
            nativeMachine = Convert.ToUInt16(
                Environment.GetEnvironmentVariable(
                    "BALUN_WINDOWS_FAKE_NATIVE_MACHINE"
                ),
                16
            );
            return Environment.GetEnvironmentVariable(
                "BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS"
            ) == "1";
        }
    }
}
"@
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
    $env:BALUN_WINDOWS_FAKE_PE_MACHINE = ''
    $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE = '0'
    $env:BALUN_WINDOWS_FAKE_PROCESS_MACHINE = '8664'
    $env:BALUN_WINDOWS_FAKE_NATIVE_MACHINE = '8664'
    $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS = '1'
    $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH = $RestrictedPath
    $env:CARGO_BUILD_TARGET = 'i686-pc-windows-msvc'
    $env:CARGO_LLVM_COV_BUILD_DIR = Join-Path $TemporaryRoot 'caller llvm-cov build override'
    $env:CARGO_LLVM_COV_TARGET_DIR = Join-Path $TemporaryRoot 'caller llvm-cov target override'
    $env:CARGO_TARGET_DIR = Join-Path $TemporaryRoot 'caller cargo target override'
    $env:MSYS2_ROOT = $FakeMsysRoot
    Set-DesktopProfileEnvironment 'x86_64'

    Invoke-TestHelper -Arguments @('-Help')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'A lightweight cross-platform HDHomeRun live TV viewer'
    Assert-ExpectedOutput 'Application ID: io.github.jm2.Balun'
    Assert-ExpectedOutput 'Windows desktop build and packaging helper'
    Assert-ExpectedOutput 'InspectLocal'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    foreach ($HelpArgument in @('--help', '-h')) {
        Invoke-TestHelper -Arguments @($HelpArgument)
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Windows desktop build and packaging helper'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    $UnavailableCases = @(
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

    foreach ($FalseSwitch in @('-CargoUpdate:$false')) {
        $SwitchName = $FalseSwitch.Substring(0, $FalseSwitch.IndexOf(':'))
        Invoke-TestHelper -Arguments @($FalseSwitch)
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput "Unavailable option(s): $SwitchName"
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }

    Invoke-TestHelper -Arguments @('-Help', '-Zip')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Windows desktop build and packaging helper'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    # Package modes are exclusive with each other, every quick mode, the
    # launch route, and the GTK-free diagnostic routes; -SkipBundle belongs
    # only to -InnoSetup, and -NoCargoBuild only to a package mode. Each
    # rejection happens before any Cargo, pkg-config, or packaging work.
    Invoke-TestHelper -Arguments @('-Bundle', '-Zip')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Package modes cannot be combined: -Bundle, -Zip'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-Zip', '-Check')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Zip cannot be combined with quick-exit mode -Check'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-InnoSetup', '-Run')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-InnoSetup cannot be combined with -Run'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-Bundle', '-Diagnostic')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'cannot be combined with -Diagnostic or -InspectLocal'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-SkipBundle', '-Zip')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-SkipBundle contradicts -Bundle and -Zip'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-NoCargoBuild')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-NoCargoBuild applies only to -Bundle, -Zip, or -InnoSetup'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

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

    Invoke-TestHelper -Arguments @('-ProbePlayback', '-Diagnostic')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-ProbePlayback exercises the desktop playback runtime and cannot be combined with -Diagnostic'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    Invoke-TestHelper -Arguments @('-ProbePlayback', '-Check')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Quick-exit modes cannot be combined'
    Assert-EmptyLog $CommandLog 'Cargo'

    Invoke-TestHelper -Arguments @('-Run', '-InspectLocal')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'mutually exclusive launch operations'
    Assert-EmptyLog $CommandLog 'Cargo'
    Assert-EmptyLog $PkgConfigLog 'pkg-config'

    foreach ($QuickMode in @('-Fmt', '-Check', '-Clippy', '-Test', '-Coverage', '-ProbePlayback')) {
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
    Assert-ExpectedOutput '-Bundle packages the desktop application and cannot be combined with -Diagnostic or -InspectLocal'
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

    # The ARM64 profile is one indivisible tuple: Cargo target, CLANGARM64
    # prefix and tools, target-scoped pkg-config variables, PE machine, receipt,
    # and Inno architecture all derive from the selected Rust target.
    $ArmDesktopBuildCommand = (
        'cargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$ArmDesktopTarget>"
    )
    Set-DesktopProfileEnvironment 'aarch64'
    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput "Using MSYS2 CLANGARM64 at $FakeArmMsysPrefix"
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--all-features> <--locked> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$ArmDesktopTarget>"
    )
    Assert-ExpectedPkgConfigProbeSet
    Assert-DesktopTargetProbe $ArmDesktopTarget
    Assert-DesktopEnvironment `
        $FakeArmPkgConfigCommand `
        $FakeArmPkgConfigDirectory `
        $FakeArmMsysBin

    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Desktop output:'
    Assert-ExpectedLog $ArmDesktopBuildCommand
    Assert-DesktopTargetProbe $ArmDesktopTarget
    Assert-DesktopEnvironment `
        $FakeArmPkgConfigCommand `
        $FakeArmPkgConfigDirectory `
        $FakeArmMsysBin

    # A successful Cargo command cannot smuggle an AMD64 executable into the
    # ARM profile (or vice versa).
    $env:BALUN_WINDOWS_FAKE_PE_MACHINE = '8664'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'Desktop output architecture validation failed'
    Assert-ExpectedOutput 'Windows profile aarch64 requires ARM64 (0xAA64)'
    Assert-ExpectedLog $ArmDesktopBuildCommand
    $env:BALUN_WINDOWS_FAKE_PE_MACHINE = ''

    foreach ($MixedDeclaration in @(
        [pscustomobject]@{ Name = 'MSYS_ENV'; Wrong = 'clang64'; Expected = 'clangarm64' },
        [pscustomobject]@{ Name = 'MSYSTEM'; Wrong = 'CLANG64'; Expected = 'CLANGARM64' },
        [pscustomobject]@{
            Name = 'MINGW_PACKAGE_PREFIX'
            Wrong = 'mingw-w64-clang-x86_64'
            Expected = 'mingw-w64-clang-aarch64'
        },
        [pscustomobject]@{ Name = 'INNO_ARCH'; Wrong = 'x64'; Expected = 'arm64' }
    )) {
        [Environment]::SetEnvironmentVariable(
            $MixedDeclaration.Name,
            $MixedDeclaration.Wrong,
            [EnvironmentVariableTarget]::Process
        )
        Invoke-TestHelper -Arguments @('-Check')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput (
            "RUST_TARGET=$ArmDesktopTarget requires $($MixedDeclaration.Name)=" +
            "$($MixedDeclaration.Expected), not $($MixedDeclaration.Wrong)"
        )
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        Set-DesktopProfileEnvironment 'aarch64'
    }

    if ($WindowsHost) {
        $FakeArmClang = Join-Path $FakeArmMsysBin 'clang.exe'
        New-FakePeFile $FakeArmClang ([uint16]0x8664)
        Invoke-TestHelper -Arguments @('-Check')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'MSYS2 tool architecture validation failed'
        Assert-ExpectedOutput 'Windows profile aarch64 requires ARM64 (0xAA64)'
        Assert-EmptyLog $CommandLog 'Cargo'
        New-FakePeFile $FakeArmClang ([uint16]0xAA64)
    }

    Set-DesktopProfileEnvironment 'x86_64'

    # -SkipBundle alone is the build-only default.
    Invoke-TestHelper -Arguments @('-SkipBundle')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Build-only run (-SkipBundle specified'
    Assert-ExpectedOutput 'Desktop output:'
    Assert-ExpectedLog $DesktopBuildCommand

    if ($WindowsHost) {
        # Package modes resolve every packaging input before any build: the shared
        # component policy, the PE inspector, the plugin scanner, and the GLib and
        # GTK resource tools. The fixture repository starts without the policy.
        Invoke-TestHelper -Arguments @('-Bundle')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Required bundled-component policy is missing'
        Assert-EmptyLog $CommandLog 'Cargo'

        $FixturePolicyDirectory = Join-Path $FixtureRoot 'build-aux\packaging'
        [System.IO.Directory]::CreateDirectory($FixturePolicyDirectory) | Out-Null
        [System.IO.File]::WriteAllLines(
            (Join-Path $FixtureRoot 'Cargo.toml'),
            @('[package]', 'name = "balun"', 'version = "0.1.0-routing"')
        )
        Copy-Item -LiteralPath (
            Join-Path $ScriptDirectory '..\build-aux\packaging\forbidden-bundled-components.txt'
        ) -Destination (Join-Path $FixturePolicyDirectory 'forbidden-bundled-components.txt')
        Invoke-TestHelper -Arguments @('-Zip')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Required packaging tools are missing from MSYS2 CLANG64'
        Assert-ExpectedOutput 'llvm-readobj.exe'
        Assert-EmptyLog $CommandLog 'Cargo'

        $FakePackagingProfiles = @(
            [pscustomobject]@{
                Prefix = $FakeMsysPrefix
                Bin = $FakeMsysBin
                Machine = [uint16]0x8664
            },
            [pscustomobject]@{
                Prefix = $FakeArmMsysPrefix
                Bin = $FakeArmMsysBin
                Machine = [uint16]0xAA64
            }
        )
        foreach ($FakePackagingProfile in $FakePackagingProfiles) {
            $FakeScannerDirectory = Join-Path $FakePackagingProfile.Prefix 'libexec\gstreamer-1.0'
            [System.IO.Directory]::CreateDirectory($FakeScannerDirectory) | Out-Null
            foreach ($FakeTool in @(
                (Join-Path $FakePackagingProfile.Bin 'llvm-readobj.exe'),
                (Join-Path $FakePackagingProfile.Bin 'glib-compile-schemas.exe'),
                (Join-Path $FakePackagingProfile.Bin 'gtk4-update-icon-cache.exe'),
                (Join-Path $FakeScannerDirectory 'gst-plugin-scanner.exe')
            )) {
                New-FakePeFile $FakeTool $FakePackagingProfile.Machine
            }
        }

        # Packaging tools are architecture-bound before Cargo or any inspector
        # is allowed to run.
        Set-DesktopProfileEnvironment 'aarch64'
        $FakeArmInspector = Join-Path $FakeArmMsysBin 'llvm-readobj.exe'
        New-FakePeFile $FakeArmInspector ([uint16]0x8664)
        Invoke-TestHelper -Arguments @('-Bundle')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Packaging tool architecture validation failed'
        Assert-ExpectedOutput 'Windows profile aarch64 requires ARM64 (0xAA64)'
        Assert-EmptyLog $CommandLog 'Cargo'
        New-FakePeFile $FakeArmInspector ([uint16]0xAA64)
        Set-DesktopProfileEnvironment 'x86_64'

        # Installer-only mode needs an existing, receipted tree and starts no build.
        Invoke-TestHelper -Arguments @('-InnoSetup', '-SkipBundle')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'No staged Windows bundle exists'
        Assert-EmptyLog $CommandLog 'Cargo'

        # -NoCargoBuild packages only an executable that already exists.
        Invoke-TestHelper -Arguments @('-Zip', '-NoCargoBuild')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Skipping the cargo build (-NoCargoBuild specified)'
        Assert-ExpectedOutput 'The expected desktop application output path is not a nonempty regular'
        Assert-EmptyLog $CommandLog 'Cargo'

        # A package mode builds the desktop first, then fails closed at the
        # application resource gate because the fake inspector cannot run.
        Invoke-TestHelper -Arguments @('-Bundle')
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Package version:'
        Assert-ExpectedOutput 'Built application resource validation failed'
        Assert-ExpectedLog $DesktopBuildCommand
        Assert-ExpectedPkgConfigProbeSet
        Assert-DesktopTargetProbe
    }
    else {
        Invoke-TestHelper -Arguments @('-Bundle')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput '-Bundle stages and probes the Windows package only from Windows'
        Assert-EmptyLog $CommandLog 'Cargo'
    }

    $ProbeCommands = (
        'cargo <test> <--release> <--locked> <--features> <desktop> <--lib> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget> " +
        "<playback::runtime::tests::installed_runtime_has_the_exact_playback_foundation> <--> <--ignored> <--exact> <--nocapture>`n" +
        'cargo <test> <--release> <--locked> <--features> <desktop> <--lib> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget> " +
        "<playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc> <--> <--ignored> <--exact> <--nocapture>`n" +
        'cargo <test> <--release> <--locked> <--features> <desktop> <--lib> ' +
        "<--target-dir> <$FixtureTargetRoot> <--target> <$DesktopTarget> " +
        '<playback::runtime::tests::installed_runtime_reports_the_decoder_and_sink_inventory> <--> <--ignored> <--exact> <--nocapture>'
    )
    Invoke-TestHelper -Arguments @('-ProbePlayback')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'GStreamer runtime plugin checks passed'
    Assert-ExpectedOutput 'Playback runtime probes passed'
    Assert-ExpectedLog $ProbeCommands
    Assert-ExpectedPkgConfigProbeSet
    Assert-DesktopTargetProbe
    Assert-DesktopEnvironment

    # The desktop build and the runtime probes fail closed before any Cargo
    # work when a structural runtime plugin is missing; other quick modes
    # never consult runtime plugins.
    $FakeGtk4Plugin = Join-Path $FakePluginDirectory 'libgstgtk4.dll'
    Remove-Item -LiteralPath $FakeGtk4Plugin -Force
    Invoke-TestHelper -Arguments @('-ProbePlayback')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'Required GStreamer playback runtime is incomplete'
    Assert-EmptyLog $CommandLog 'Cargo'
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

        # The native-machine result, not the current PowerShell process
        # machine, selects the default desktop profile. These conflicting
        # fixtures model an x64 shell emulated on ARM64 and the inverse so the
        # expectation does not duplicate the helper's detection logic.
        $NativeMachineCases = @(
            [pscustomobject]@{
                ProcessMachine = '8664'
                NativeMachine = 'AA64'
                Architecture = 'aarch64'
                Target = $ArmDesktopTarget
            },
            [pscustomobject]@{
                ProcessMachine = 'AA64'
                NativeMachine = '8664'
                Architecture = 'x86_64'
                Target = $DesktopTarget
            }
        )
        $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE = '1'
        foreach ($NativeMachineCase in $NativeMachineCases) {
            $env:BALUN_WINDOWS_FAKE_PROCESS_MACHINE = $NativeMachineCase.ProcessMachine
            $env:BALUN_WINDOWS_FAKE_NATIVE_MACHINE = $NativeMachineCase.NativeMachine
            $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS = '1'
            $env:RUST_TARGET = ''
            Clear-DesktopProfileDeclarations
            $ExpectedNativeBuildCommand = (
                'cargo <build> <--release> <--locked> <--features> <desktop> ' +
                '<--bin> <balun> ' +
                "<--target-dir> <$FixtureTargetRoot> " +
                "<--target> <$($NativeMachineCase.Target)>"
            )
            Invoke-TestHelper -Arguments @()
            Assert-ExpectedStatus 0
            Assert-ExpectedLog $ExpectedNativeBuildCommand
            Assert-ExpectedPkgConfigProbeSet
            Assert-DesktopTargetProbe $NativeMachineCase.Target
            if ($NativeMachineCase.Architecture -ceq 'aarch64') {
                Assert-DesktopEnvironment `
                    $FakeArmPkgConfigCommand `
                    $FakeArmPkgConfigDirectory `
                    $FakeArmMsysBin
            }
            else {
                Assert-DesktopEnvironment
            }
        }

        # A caller-selected target bypasses native probing completely.
        Set-DesktopProfileEnvironment 'aarch64'
        $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS = '0'
        Invoke-TestHelper -Arguments @('-Check')
        Assert-ExpectedStatus 0
        Assert-ExpectedLog (
            'cargo <check> <--all-targets> <--all-features> <--locked> ' +
            "<--target-dir> <$FixtureTargetRoot> <--target> <$ArmDesktopTarget>"
        )
        Assert-DesktopTargetProbe $ArmDesktopTarget

        $env:RUST_TARGET = ''
        Clear-DesktopProfileDeclarations
        Invoke-TestHelper -Arguments @()
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Native Windows architecture detection failed with Win32 error'
        Assert-ExpectedOutput 'set RUST_TARGET explicitly'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        Assert-EmptyLog $TargetProbeLog 'Rust target probe'

        $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE_SUCCESS = '1'
        $env:BALUN_WINDOWS_FAKE_NATIVE_MACHINE = '014C'
        Invoke-TestHelper -Arguments @()
        Assert-ExpectedStatus 1
        Assert-ExpectedOutput 'Native Windows architecture 0x014C is unsupported'
        Assert-ExpectedOutput 'set RUST_TARGET explicitly'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
        Assert-EmptyLog $TargetProbeLog 'Rust target probe'

        # Exercise the real Windows API as well. OSArchitecture supplies the
        # independent expectation even when this PowerShell process is
        # running under emulation.
        $env:BALUN_WINDOWS_FAKE_NATIVE_PROBE = '0'
        $NativeDesktopArchitecture = Get-NativeDesktopTestArchitecture
        $NativeDesktopTarget = if ($NativeDesktopArchitecture -ceq 'aarch64') {
            $ArmDesktopTarget
        }
        else {
            $DesktopTarget
        }
        $NativeDesktopBuildCommand = (
            'cargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> ' +
            "<--target-dir> <$FixtureTargetRoot> <--target> <$NativeDesktopTarget>"
        )
        $env:RUST_TARGET = ''
        Clear-DesktopProfileDeclarations
        Invoke-TestHelper -Arguments @()
        Assert-ExpectedStatus 0
        Assert-ExpectedLog $NativeDesktopBuildCommand
        Assert-ExpectedPkgConfigProbeSet
        Assert-DesktopTargetProbe $NativeDesktopTarget
        if ($NativeDesktopArchitecture -ceq 'aarch64') {
            Assert-DesktopEnvironment `
                $FakeArmPkgConfigCommand `
                $FakeArmPkgConfigDirectory `
                $FakeArmMsysBin
        }
        else {
            Assert-DesktopEnvironment
        }
    }
    else {
        Invoke-TestHelper -Arguments @('-Diagnostic')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput 'A non-Windows host must set RUST_TARGET'
        Assert-EmptyLog $CommandLog 'Cargo'
        Assert-EmptyLog $PkgConfigLog 'pkg-config'
    }
    Set-DesktopProfileEnvironment 'x86_64'

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
    Assert-ExpectedOutput 'supports RUST_TARGET='
    Assert-ExpectedOutput $DesktopTarget
    Assert-ExpectedOutput $ArmDesktopTarget
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
        $NativeDesktopArchitecture = Get-NativeDesktopTestArchitecture
        Set-DesktopProfileEnvironment $NativeDesktopArchitecture
        $NativeRunTarget = if ($NativeDesktopArchitecture -ceq 'aarch64') {
            $ArmDesktopTarget
        }
        else {
            $DesktopTarget
        }
        $NativeRunBuildCommand = (
            'cargo <build> <--release> <--locked> <--features> <desktop,windows-console> ' +
            "<--bin> <balun> <--target-dir> <$FixtureTargetRoot> <--target> <$NativeRunTarget>"
        )
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = Join-Path $env:SystemRoot 'System32\whoami.exe'
        Invoke-TestHelper -Arguments @('-Run')
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Launching'
        Assert-ExpectedLog $NativeRunBuildCommand
        Assert-ExpectedPkgConfigProbeSet
        $env:BALUN_WINDOWS_FAKE_RUN_SYSTEM_BINARY = ''
        Set-DesktopProfileEnvironment 'x86_64'
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
        "RustTarget = 'x86_64-pc-windows-gnullvm'",
        "RustTarget = 'aarch64-pc-windows-gnullvm'",
        "MsysEnvironment = 'clangarm64'",
        "MsysSystem = 'CLANGARM64'",
        "MsysPackagePrefix = 'mingw-w64-clang-aarch64'",
        "PeMachine = [uint16]0xAA64",
        "InnoTargetArchitecture = 'arm64'",
        'IsWow64Process2(',
        'out ushort nativeMachine',
        '$ignoredProcessMachine',
        "'--features',",
        "'desktop',",
        "'desktop,windows-console'",
        "'gstreamer-1.0'",
        "'1.20'",
        "'libgstgtk4.dll'",
        "'gst-plugins-rs'",
        "'playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc'",
        '& $BinaryItem.FullName',
        "& `$BinaryItem.FullName '--inspect' '--local'",
        # The packaging contract: the shared policy, the capability-derived
        # closure with its libav and Windows audio anchors, non-executing PE
        # inspection, the sanitized probe environment, the exact sentinel, and
        # the reopened ZIP and installer.
        "'build-aux\packaging\forbidden-bundled-components.txt'",
        "Plugin = 'libgstlibav.dll'",
        "Plugin = 'libgstwasapi2.dll'",
        "Plugin = 'libgstgtk4.dll'",
        "'--coff-imports '",
        '''--coff-resources "''',
        "= '--balun-platform-runtime-probe'",
        '= "balun-windows-runtime-probe-v1`n"',
        "= 'balun-windows-runtime-probe-v2'",
        '$lines.Add("rust-target=$DesktopRustTarget")',
        '$lines.Add("msys-environment=$MsysEnvironment")',
        '$lines.Add("inno-architecture=$InnoTargetArchitecture")',
        '[System.Environment]::SystemDirectory',
        "['GST_REGISTRY'] = `$probeRegistry",
        'Assert-WindowsZipMatchesTree $zipPath $Distribution',
        "= 'build-aux\inno\balun.iss'",
        'Assert-WindowsProbeReceipt $Distribution',
        'Assert-Msys2ToolMachineContract $MsysLayout',
        'Assert-PeMachine',
        '"/DTargetArch=$InnoTargetArchitecture"'
    )) {
        if (-not $HelperText.Contains($RequiredText)) {
            Assert-RoutingTestFailure "helper is missing required text: $RequiredText"
        }
    }
    foreach ($ForbiddenText in @(
        'RuntimeInformation]::ProcessArchitecture',
        'PROCESSOR_ARCHITECTURE',
        'PROCESSOR_ARCHITEW6432'
    )) {
        if ($HelperText.Contains($ForbiddenText)) {
            Assert-RoutingTestFailure (
                "helper still uses process-derived native architecture: $ForbiddenText"
            )
        }
    }
    # Packaging may copy, archive, and launch bounded inspectors and the
    # staged executable, but the helper must never install, download, or
    # update anything.
    if ($HelperText -match '(?im)^\s*(cargo\s+install|rustup\s+(target|component)\s+add|winget|choco|pacman)(\s|$)' -or
        $HelperText -match '(?i)\b(Expand-Archive|Invoke-WebRequest|Invoke-RestMethod|Start-BitsTransfer|curl|wget|git)\b') {
        Assert-RoutingTestFailure 'helper contains installer, downloader, or update logic'
    }

    $InnoText = [System.IO.File]::ReadAllText($InnoRecipeUnderTest)
    foreach ($RequiredText in @(
        '#if TargetArch == "arm64"',
        'ArchitecturesAllowed=arm64',
        'ArchitecturesInstallIn64BitMode=arm64',
        '#elif TargetArch == "x64"',
        'ArchitecturesAllowed=x64compatible',
        'ArchitecturesInstallIn64BitMode=x64compatible',
        '#error Unsupported TargetArch; expected x64 or arm64'
    )) {
        if (-not $InnoText.Contains($RequiredText)) {
            Assert-RoutingTestFailure "Inno recipe is missing required text: $RequiredText"
        }
    }
    if ($InnoText.Contains('ARM64 Windows is not supported yet')) {
        Assert-RoutingTestFailure 'Inno recipe still rejects ARM64 Windows'
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
