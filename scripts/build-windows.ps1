<#
.SYNOPSIS
    Balun - Windows headless diagnostic build helper

.DESCRIPTION
    Builds Balun's current GTK-free balun-discover diagnostic only.

    A lightweight cross-platform HDHomeRun live TV viewer
    Application ID: io.github.jm2.Balun

    With no mode, this runs a locked release build and requires a nonempty,
    regular, non-reparse-point balun-discover.exe at Cargo's expected output
    path. This is a path-shape check, not PE or package validation.

    The helper never invokes an installer, package manager, dependency update,
    or toolchain installation command. Cargo may fetch missing locked dependency
    sources, and a rustup-managed Cargo invocation may fetch the caller-selected
    Rust toolchain before Cargo starts.

    On Windows, Cargo's native target is used unless RUST_TARGET names an
    explicit Windows target. A non-Windows host must set an already-installed
    Windows RUST_TARGET; this helper never provisions cross-compilation tools.

.PARAMETER Fmt
    Run cargo fmt across the workspace and exit.

.PARAMETER Check
    Check all targets with Cargo's locked dependency graph and exit.

.PARAMETER Clippy
    Lint all targets with locked dependencies and warnings denied, then exit.

.PARAMETER Test
    Test all targets with Cargo's locked dependency graph, then exit.

.PARAMETER Coverage
    Print an all-target/all-feature coverage summary. Requires preinstalled
    cargo-llvm-cov 0.8.7 and its compiler support; nothing is installed.

.PARAMETER Bundle
    Unavailable until Balun has a reviewed GUI executable and runtime closure.

.PARAMETER Zip
    Unavailable until Balun has a reviewed GUI executable and runtime closure.

.PARAMETER InnoSetup
    Unavailable until Balun has a reviewed installer recipe and artifact gates.

.PARAMETER Package
    Unavailable until Balun has a reviewed GUI package implementation.

.PARAMETER Installer
    Unavailable until Balun has a reviewed installer recipe and artifact gates.

.PARAMETER SkipBundle
    Obsolete because this helper never bundles the diagnostic.

.PARAMETER NoCargoBuild
    Unavailable because this helper has no post-build package operation.

.PARAMETER Msys2Root
    Unavailable because the current headless diagnostic has no MSYS2 closure.

.PARAMETER Run
    Unavailable so the helper never initiates network discovery. Run the
    diagnostic explicitly when discovery is intended.

.PARAMETER CargoUpdate
    Unavailable because this build helper never edits the locked dependency
    graph or initiates an update.

.PARAMETER CargoUpdateArgs
    Unavailable with CargoUpdate.

.PARAMETER Help
    Show this help and exit. Equivalent to --help and -h.
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [switch]$Fmt,
    [switch]$Check,
    [switch]$Clippy,
    [switch]$Test,
    [switch]$Coverage,
    [switch]$Bundle,
    [switch]$Zip,
    [switch]$InnoSetup,
    [switch]$Package,
    [switch]$Installer,
    [switch]$SkipBundle,
    [switch]$NoCargoBuild,
    [AllowNull()][string]$Msys2Root,
    [switch]$Run,
    [switch]$CargoUpdate,
    [AllowNull()][string]$CargoUpdateArgs,
    [switch]$Help,
    [Parameter(ValueFromRemainingArguments = $true)]
    [AllowEmptyCollection()][object[]]$RemainingArguments
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ApplicationId = 'io.github.jm2.Balun'
$BinaryName = 'balun-discover.exe'
$RequiredCoverageVersion = 'cargo-llvm-cov 0.8.7'

function Write-Info {
    param([string]$Message)
    Write-Output "[balun] $Message"
}

function Exit-WithError {
    param([string]$Message)
    [Console]::Error.WriteLine("[balun] $Message")
    exit 1
}

function Exit-WithUsageError {
    param([string]$Message)
    [Console]::Error.WriteLine("[balun] $Message")
    exit 2
}

# Keep obsolete Tributary packaging entry points recognizable, but reject them
# before resolving Cargo, changing location, probing MSYS2, or starting any
# build, install, package, or network work.
$UnavailableOptions = @()
foreach ($option in @(
    @{ Name = '-Bundle'; Enabled = $PSBoundParameters.ContainsKey('Bundle') },
    @{ Name = '-Zip'; Enabled = $PSBoundParameters.ContainsKey('Zip') },
    @{ Name = '-InnoSetup'; Enabled = $PSBoundParameters.ContainsKey('InnoSetup') },
    @{ Name = '-Package'; Enabled = $PSBoundParameters.ContainsKey('Package') },
    @{ Name = '-Installer'; Enabled = $PSBoundParameters.ContainsKey('Installer') },
    @{ Name = '-SkipBundle'; Enabled = $PSBoundParameters.ContainsKey('SkipBundle') },
    @{ Name = '-NoCargoBuild'; Enabled = $PSBoundParameters.ContainsKey('NoCargoBuild') },
    @{ Name = '-Msys2Root'; Enabled = $PSBoundParameters.ContainsKey('Msys2Root') },
    @{ Name = '-Run'; Enabled = $PSBoundParameters.ContainsKey('Run') },
    @{ Name = '-CargoUpdate'; Enabled = $PSBoundParameters.ContainsKey('CargoUpdate') },
    @{ Name = '-CargoUpdateArgs'; Enabled = $PSBoundParameters.ContainsKey('CargoUpdateArgs') }
)) {
    if ($option.Enabled) {
        $UnavailableOptions += $option.Name
    }
}

if ($UnavailableOptions.Count -gt 0) {
    Exit-WithUsageError (
        "Unavailable option(s): $($UnavailableOptions -join ', '). " +
        'Balun currently builds only the headless diagnostic; no build, ' +
        'install, package, launch, or network work was started.'
    )
}

$RemainingArgumentText = @(
    $RemainingArguments |
        Where-Object { $null -ne $_ } |
        ForEach-Object { $_.ToString() }
)
$HelpRequested = $Help.IsPresent -or
    ($RemainingArgumentText -contains '--help') -or
    ($RemainingArgumentText -contains '-h')
$UnexpectedArguments = @(
    $RemainingArgumentText | Where-Object { $_ -ne '--help' -and $_ -ne '-h' }
)
if ($UnexpectedArguments.Count -gt 0) {
    Exit-WithUsageError "Unknown argument(s): $($UnexpectedArguments -join ', ')"
}
if ($HelpRequested) {
    Get-Help -Full $PSCommandPath
    exit 0
}

$QuickModes = @()
foreach ($mode in @(
    @{ Name = '-Fmt'; Enabled = $Fmt.IsPresent },
    @{ Name = '-Check'; Enabled = $Check.IsPresent },
    @{ Name = '-Clippy'; Enabled = $Clippy.IsPresent },
    @{ Name = '-Test'; Enabled = $Test.IsPresent },
    @{ Name = '-Coverage'; Enabled = $Coverage.IsPresent }
)) {
    if ($mode.Enabled) {
        $QuickModes += $mode.Name
    }
}
if ($QuickModes.Count -gt 1) {
    Exit-WithUsageError "Quick-exit modes cannot be combined: $($QuickModes -join ', ')"
}

function Test-IsWindowsHost {
    return [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
}

function Resolve-WindowsRustTarget {
    if (-not [string]::IsNullOrWhiteSpace($env:RUST_TARGET)) {
        $target = $env:RUST_TARGET
        if ($target.Length -gt 128 -or
            $target -notmatch '^[A-Za-z0-9_][A-Za-z0-9_.-]*$' -or
            $target -notmatch '-windows-') {
            Exit-WithUsageError "RUST_TARGET must name one bounded Windows Rust target."
        }
        return $target
    }

    if (Test-IsWindowsHost) {
        return $null
    }

    Exit-WithUsageError (
        'A non-Windows host must set RUST_TARGET to an already-installed ' +
        'Windows target; this helper will not install one.'
    )
}

function Invoke-Cargo {
    param(
        [System.Management.Automation.CommandInfo]$CargoCommand,
        [string[]]$Arguments,
        [string]$Description
    )

    $global:LASTEXITCODE = 0
    & $CargoCommand @Arguments
    if ($LASTEXITCODE -ne 0) {
        Exit-WithError "$Description failed with exit code $LASTEXITCODE."
    }
}

$RepositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$CargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $CargoCommand) {
    Exit-WithError 'cargo is unavailable; install and select Rust explicitly, then retry.'
}

Push-Location -LiteralPath $RepositoryRoot
try {
    if ($Fmt) {
        Write-Info 'Formatting Balun...'
        Invoke-Cargo $CargoCommand @('fmt', '--all') 'cargo fmt'
        Write-Info 'Formatting complete.'
        exit 0
    }

    $RustTarget = Resolve-WindowsRustTarget
    $TargetArguments = if ($null -eq $RustTarget) {
        @()
    }
    else {
        @('--target', $RustTarget)
    }

    if ($Check) {
        Write-Info 'Checking all Balun targets with locked dependencies...'
        $CargoArguments = @('check', '--all-targets', '--locked') + $TargetArguments
        Invoke-Cargo $CargoCommand $CargoArguments 'cargo check'
        Write-Info 'Check passed.'
        exit 0
    }

    if ($Clippy) {
        Write-Info 'Linting all Balun targets with locked dependencies...'
        $CargoArguments = @('clippy', '--all-targets', '--locked') +
            $TargetArguments + @('--', '-D', 'warnings')
        Invoke-Cargo $CargoCommand $CargoArguments 'cargo clippy'
        Write-Info 'Clippy passed.'
        exit 0
    }

    if ($Test) {
        Write-Info 'Testing all Balun targets with locked dependencies...'
        $CargoArguments = @('test', '--all-targets', '--locked') + $TargetArguments
        Invoke-Cargo $CargoCommand $CargoArguments 'cargo test'
        Write-Info 'Tests passed.'
        exit 0
    }

    if ($Coverage) {
        $global:LASTEXITCODE = 0
        $VersionOutput = @(& $CargoCommand llvm-cov --version 2>$null)
        $VersionStatus = $LASTEXITCODE
        $InstalledCoverageVersion = if ($VersionOutput.Count -gt 0) {
            [string]$VersionOutput[0]
        }
        else {
            ''
        }
        if ($VersionStatus -ne 0 -or
            $InstalledCoverageVersion -cne $RequiredCoverageVersion) {
            Exit-WithError (
                "Coverage requires preinstalled $RequiredCoverageVersion exactly; " +
                'this helper will not install or replace tools.'
            )
        }

        Write-Info "Running informational coverage with $RequiredCoverageVersion..."
        $CargoArguments = @(
            'llvm-cov',
            '--all-targets',
            '--all-features',
            '--locked'
        ) + $TargetArguments + @('--summary-only')
        Invoke-Cargo $CargoCommand $CargoArguments 'cargo llvm-cov'
        exit 0
    }

    $CargoArguments = @(
        'build',
        '--release',
        '--locked',
        '--bin',
        'balun-discover'
    ) + $TargetArguments
    Write-Info 'Building balun-discover (locked release)...'
    Invoke-Cargo $CargoCommand $CargoArguments 'cargo build'

    $BinaryPath = if ($null -eq $RustTarget) {
        Join-Path $RepositoryRoot "target\release\$BinaryName"
    }
    else {
        Join-Path $RepositoryRoot "target\$RustTarget\release\$BinaryName"
    }
    $BinaryItem = Get-Item -LiteralPath $BinaryPath -Force -ErrorAction SilentlyContinue
    if ($null -eq $BinaryItem -or
        $BinaryItem -isnot [System.IO.FileInfo] -or
        ($BinaryItem.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0 -or
        ($BinaryItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $BinaryItem.Length -le 0) {
        Exit-WithError (
            'The expected diagnostic output path is not a nonempty regular, ' +
            "non-reparse-point file: $BinaryPath"
        )
    }

    Write-Info "Application ID: $ApplicationId"
    Write-Info "Expected diagnostic output path: $($BinaryItem.FullName)"
    exit 0
}
finally {
    Pop-Location
}
