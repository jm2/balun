<#
Deterministic command-routing tests for scripts/build-windows.ps1.

Each invocation runs in a fresh child PowerShell process with an in-memory
fake Cargo command. No compiler, installer, package manager, network access,
GUI bundle, or native Windows artifact is used.
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
$Runner = Join-Path $TemporaryRoot 'runner.ps1'
$CommandLog = Join-Path $TemporaryRoot 'commands.log'
$RestrictedPath = Join-Path $TemporaryRoot 'empty-command-path'
$PowerShellExecutable = (Get-Process -Id $PID).Path
$EnvironmentNames = @(
    'BALUN_WINDOWS_HELPER',
    'BALUN_WINDOWS_TEST_LOG',
    'BALUN_WINDOWS_FAKE_CARGO_STATUS',
    'BALUN_WINDOWS_FAKE_CARGO_AVAILABLE',
    'BALUN_WINDOWS_FAKE_COVERAGE_VERSION',
    'BALUN_WINDOWS_FAKE_SKIP_BINARY',
    'BALUN_WINDOWS_FAKE_ZERO_BINARY',
    'BALUN_WINDOWS_TEST_RESTRICTED_PATH',
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

function Assert-RoutingTestFailure {
    param([string]$Message)

    [Console]::Error.WriteLine("build-windows routing test failed: $Message")
    [Console]::Error.WriteLine("status: $script:LastStatus")
    [Console]::Error.WriteLine("output:`n$script:LastOutput")
    if (Test-Path -LiteralPath $CommandLog -PathType Leaf) {
        [Console]::Error.WriteLine(
            "commands:`n$([System.IO.File]::ReadAllText($CommandLog))"
        )
    }
    exit 1
}

function Invoke-TestHelper {
    param([string[]]$Arguments)

    [System.IO.File]::WriteAllText($CommandLog, '')
    $TargetDirectory = Join-Path $FixtureRoot 'target'
    if (Test-Path -LiteralPath $TargetDirectory) {
        Remove-Item -LiteralPath $TargetDirectory -Recurse -Force
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

function Get-CommandLog {
    return [System.IO.File]::ReadAllText($CommandLog).TrimEnd(
        [char[]]@("`r", "`n")
    )
}

function Assert-ExpectedLog {
    param([string]$Expected)
    $Actual = Get-CommandLog
    if ($Actual -cne $Expected) {
        Assert-RoutingTestFailure "unexpected command routing; expected '$Expected', got '$Actual'"
    }
}

function Assert-EmptyLog {
    if ((Get-Item -LiteralPath $CommandLog).Length -ne 0) {
        Assert-RoutingTestFailure 'expected no Cargo invocation'
    }
}

try {
    [System.IO.Directory]::CreateDirectory($FixtureScripts) | Out-Null
    [System.IO.Directory]::CreateDirectory($RestrictedPath) | Out-Null
    Copy-Item -LiteralPath $HelperUnderTest -Destination $FixtureHelper

    $RunnerSource = @'
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$env:PATH = $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH

function global:cargo {
    $RenderedArguments = @($args | ForEach-Object { "<$($_.ToString())>" }) -join ' '
    [System.IO.File]::AppendAllText(
        $env:BALUN_WINDOWS_TEST_LOG,
        "cargo $RenderedArguments`n"
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
        $ArtifactDirectory = Join-Path (Get-Location).ProviderPath 'target'
        if (-not [string]::IsNullOrWhiteSpace($env:RUST_TARGET)) {
            $ArtifactDirectory = Join-Path $ArtifactDirectory $env:RUST_TARGET
        }
        $ArtifactDirectory = Join-Path $ArtifactDirectory 'release'
        [System.IO.Directory]::CreateDirectory($ArtifactDirectory) | Out-Null
        $ArtifactPath = Join-Path $ArtifactDirectory 'balun-discover.exe'
        if ([int]$env:BALUN_WINDOWS_FAKE_ZERO_BINARY -eq 1) {
            $EmptyArtifact = [System.IO.File]::Create($ArtifactPath)
            $EmptyArtifact.Dispose()
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

& $env:BALUN_WINDOWS_HELPER @args
exit $global:LASTEXITCODE
'@
    [System.IO.File]::WriteAllText($Runner, $RunnerSource)
    [System.IO.File]::WriteAllText($CommandLog, '')

    $env:BALUN_WINDOWS_HELPER = $FixtureHelper
    $env:BALUN_WINDOWS_TEST_LOG = $CommandLog
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'
    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '1'
    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 0.8.7'
    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '0'
    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '0'
    $env:BALUN_WINDOWS_TEST_RESTRICTED_PATH = $RestrictedPath
    $env:RUST_TARGET = 'x86_64-pc-windows-msvc'

    Invoke-TestHelper -Arguments @('-Help')
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'A lightweight cross-platform HDHomeRun live TV viewer'
    Assert-ExpectedOutput 'Application ID: io.github.jm2.Balun'
    Assert-EmptyLog

    foreach ($HelpArgument in @('--help', '-h')) {
        Invoke-TestHelper -Arguments @($HelpArgument)
        Assert-ExpectedStatus 0
        Assert-ExpectedOutput 'Balun - Windows headless diagnostic build helper'
        Assert-EmptyLog
    }

    $UnavailableCases = @(
        [pscustomobject]@{ Name = '-Bundle'; Arguments = @('-Bundle') },
        [pscustomobject]@{ Name = '-Zip'; Arguments = @('-Zip') },
        [pscustomobject]@{ Name = '-InnoSetup'; Arguments = @('-InnoSetup') },
        [pscustomobject]@{ Name = '-Package'; Arguments = @('-Package') },
        [pscustomobject]@{ Name = '-Installer'; Arguments = @('-Installer') },
        [pscustomobject]@{ Name = '-SkipBundle'; Arguments = @('-SkipBundle') },
        [pscustomobject]@{ Name = '-NoCargoBuild'; Arguments = @('-NoCargoBuild') },
        [pscustomobject]@{
            Name = '-Msys2Root'
            Arguments = @('-Msys2Root', 'C:\msys64')
        },
        [pscustomobject]@{ Name = '-Run'; Arguments = @('-Run') },
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
        Assert-ExpectedOutput 'no build, install, package, launch, or network work was started'
        Assert-EmptyLog
    }

    foreach ($FalseSwitch in @(
        '-Bundle:$false',
        '-Zip:$false',
        '-InnoSetup:$false',
        '-Package:$false',
        '-Installer:$false',
        '-SkipBundle:$false',
        '-NoCargoBuild:$false',
        '-Run:$false',
        '-CargoUpdate:$false'
    )) {
        $SwitchName = $FalseSwitch.Substring(0, $FalseSwitch.IndexOf(':'))
        Invoke-TestHelper -Arguments @($FalseSwitch)
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput "Unavailable option(s): $SwitchName"
        Assert-EmptyLog
    }

    Invoke-TestHelper -Arguments @('-Help', '-Zip')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput '-Zip'
    Assert-EmptyLog

    Invoke-TestHelper -Arguments @('-Check', '-Clippy')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Quick-exit modes cannot be combined'
    Assert-EmptyLog

    Invoke-TestHelper -Arguments @('unexpected-positional-argument')
    Assert-ExpectedStatus 2
    Assert-ExpectedOutput 'Unknown argument(s): unexpected-positional-argument'
    Assert-EmptyLog

    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '0'
    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'cargo is unavailable'
    Assert-EmptyLog
    $env:BALUN_WINDOWS_FAKE_CARGO_AVAILABLE = '1'

    Invoke-TestHelper -Arguments @('-Fmt')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog 'cargo <fmt> <--all>'

    Invoke-TestHelper -Arguments @('-Check')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <check> <--all-targets> <--locked> <--target> ' +
        '<x86_64-pc-windows-msvc>'
    )

    Invoke-TestHelper -Arguments @('-Clippy')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <clippy> <--all-targets> <--locked> <--target> ' +
        '<x86_64-pc-windows-msvc> <--> <-D> <warnings>'
    )

    Invoke-TestHelper -Arguments @('-Test')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        'cargo <test> <--all-targets> <--locked> <--target> ' +
        '<x86_64-pc-windows-msvc>'
    )

    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 0
    Assert-ExpectedLog (
        "cargo <llvm-cov> <--version>`n" +
        'cargo <llvm-cov> <--all-targets> <--all-features> <--locked> ' +
        '<--target> <x86_64-pc-windows-msvc> <--summary-only>'
    )

    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 9.9.9'
    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
    Assert-ExpectedOutput 'will not install or replace tools'
    Assert-ExpectedLog 'cargo <llvm-cov> <--version>'
    $env:BALUN_WINDOWS_FAKE_COVERAGE_VERSION = 'cargo-llvm-cov 0.8.7'

    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '26'
    Invoke-TestHelper -Arguments @('-Coverage')
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
    Assert-ExpectedLog 'cargo <llvm-cov> <--version>'
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'

    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 0
    Assert-ExpectedOutput 'Application ID: io.github.jm2.Balun'
    Assert-ExpectedOutput 'balun-discover.exe'
    Assert-ExpectedLog (
        'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
        '<--target> <x86_64-pc-windows-msvc>'
    )

    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '24'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'cargo build failed with exit code 24'
    Assert-ExpectedLog (
        'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
        '<--target> <x86_64-pc-windows-msvc>'
    )
    $env:BALUN_WINDOWS_FAKE_CARGO_STATUS = '0'

    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
    Assert-ExpectedLog (
        'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
        '<--target> <x86_64-pc-windows-msvc>'
    )
    $env:BALUN_WINDOWS_FAKE_SKIP_BINARY = '0'

    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '1'
    Invoke-TestHelper -Arguments @()
    Assert-ExpectedStatus 1
    Assert-ExpectedOutput 'is not a nonempty regular, non-reparse-point file'
    Assert-ExpectedLog (
        'cargo <build> <--release> <--locked> <--bin> <balun-discover> ' +
        '<--target> <x86_64-pc-windows-msvc>'
    )
    $env:BALUN_WINDOWS_FAKE_ZERO_BINARY = '0'

    foreach ($InvalidTarget in @(
        '..\unsafe-windows-target',
        'x86_64-unknown-linux-gnu',
        'x86_64-pc-windows-msvc/..',
        '-windows-msvc',
        (('x' * 116) + '-windows-msvc')
    )) {
        $env:RUST_TARGET = $InvalidTarget
        Invoke-TestHelper -Arguments @('-Check')
        Assert-ExpectedStatus 2
        Assert-ExpectedOutput 'RUST_TARGET must name one bounded Windows Rust target'
        Assert-EmptyLog
    }
    $env:RUST_TARGET = 'x86_64-pc-windows-msvc'

    $HelperText = [System.IO.File]::ReadAllText($HelperUnderTest)
    foreach ($RequiredText in @(
        'A lightweight cross-platform HDHomeRun live TV viewer',
        'io.github.jm2.Balun'
    )) {
        if (-not $HelperText.Contains($RequiredText)) {
            Assert-RoutingTestFailure "helper is missing required text: $RequiredText"
        }
    }
    if ($HelperText -match '(?im)^\s*(cargo\s+install|rustup\s+(target|component)\s+add|winget|choco|pacman)(\s|$)' -or
        $HelperText -match '(?i)\b(Copy-Item|Compress-Archive|Expand-Archive|Invoke-WebRequest|Invoke-RestMethod|Start-BitsTransfer|Start-Process|curl|wget|git)\b') {
        Assert-RoutingTestFailure 'helper contains installer, downloader, archive, or runtime-copy logic'
    }

    Write-Output 'build-windows command-routing tests passed'
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
