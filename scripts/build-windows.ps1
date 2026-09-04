<#
.SYNOPSIS
    Balun - Windows desktop build and packaging helper

.DESCRIPTION
    Builds Balun's GTK4/libadwaita/GStreamer desktop application with the MSYS2
    CLANG64 toolchain. With no mode, this runs a locked release build and
    requires a nonempty, regular, non-reparse-point balun.exe at Cargo's
    expected output path. It does not launch, bundle, or install the application.

    A lightweight cross-platform HDHomeRun live TV viewer
    Application ID: io.github.jm2.Balun

    The helper selects x86_64-pc-windows-gnullvm, verifies that Rust target is
    installed, discovers a standard MSYS2 installation automatically,
    configures its CLANG64 compiler and pkg-config paths, and checks the GTK
    4.16, libadwaita 1.6, and GStreamer 1.20 development-library floors. Only
    the x86_64 CLANG64 environment is supported; ARM64 Windows is not supported
    yet. Before a desktop build it also requires the
    GStreamer runtime plugin files that provide playbin3, appsrc, tsdemux,
    deinterlace, and gtk4paintablesink, and warns when the libav broadcast
    decoders are absent. Use Msys2Root only for a nonstandard MSYS2
    installation.

    The packaging modes stage a self-contained application tree under
    dist\balun-windows in the MSYS2 prefix shape (bin\balun.exe beside every
    DLL, lib\gstreamer-1.0, libexec\gstreamer-1.0, share), copying only the
    reviewed, capability-derived GStreamer plugin closure and the DLLs those
    binaries import; run the hidden packaged-runtime probe inside the staged
    tree with a sanitized environment; and reopen every completed artifact.
    The shared release component policy is enforced at every copy boundary,
    during import traversal, over the completed tree, and inside the ZIP.

    The helper never invokes an installer, package manager, dependency update,
    or toolchain installation command. Cargo may fetch missing locked dependency
    sources, and a rustup-managed Cargo invocation may fetch the caller-selected
    Rust toolchain before Cargo starts.

.PARAMETER Fmt
    Run cargo fmt across the workspace and exit. This mode does not require
    MSYS2 because it does not compile the application.

.PARAMETER Check
    Check all targets with Cargo's locked dependency graph and exit. The
    desktop feature is included unless Diagnostic is also specified.

.PARAMETER Clippy
    Lint all targets with locked dependencies and warnings denied in both the
    debug and release profiles, then exit. The desktop feature is included
    unless Diagnostic is also specified.

.PARAMETER Test
    Test all targets with Cargo's locked dependency graph, then exit. The
    desktop feature is included unless Diagnostic is also specified. Tests run
    in the debug profile here; CI additionally runs the release profile.

.PARAMETER Coverage
    Print an all-target coverage summary. Requires preinstalled cargo-llvm-cov
    0.8.7 and its compiler support; nothing is installed. Build, coverage, and
    intermediate artifacts are confined under this repository's target tree.

.PARAMETER ProbePlayback
    Run the installed-runtime playback probes in the release profile and
    exit: the exact structural factory snapshot and the constant-URI appsrc
    contract. Requires the MSYS2 desktop development libraries and runtime
    plugins; it cannot be combined with Diagnostic.

.PARAMETER Bundle
    Build the desktop application (unless NoCargoBuild is given), stage the
    self-contained tree under dist\balun-windows from the reviewed plugin
    closure, resolve and inspect every PE import, validate the application's
    icon and version resources, run the packaged-runtime probe, and record its
    receipt. No archive is created.

.PARAMETER Zip
    Everything Bundle does, then create dist\balun-windows.zip and reopen it
    to validate its entry names against the policy and the staged tree.

.PARAMETER InnoSetup
    Everything Zip does, then compile dist\balun-setup.exe from
    build-aux\inno\balun.iss with a preinstalled Inno Setup 6 and reopen the
    installer's version resource. With SkipBundle, build only the installer
    from an existing dist tree whose probe receipt still matches.

.PARAMETER SkipBundle
    With InnoSetup, skip the build, staging, and probe and accept the existing
    dist tree only while its packaged-runtime probe receipt matches. Alone it
    is the build-only default; it cannot be combined with Bundle or Zip.

.PARAMETER NoCargoBuild
    Skip the cargo build inside Bundle, Zip, or InnoSetup and package the
    balun.exe already at Cargo's expected output path.

.PARAMETER Msys2Root
    Optional root of a nonstandard MSYS2 installation. When omitted, the helper
    checks MSYS2_ROOT, CLANG64 tools already on PATH, C:\msys64, and the common
    GitHub Actions temporary installation location.

.PARAMETER Diagnostic
    Use the GTK-free balun-discover diagnostic instead of the desktop app.
    This preserves native Windows diagnostic builds without requiring MSYS2.
    When RUST_TARGET is absent on Windows, rustc's bounded native host tuple is
    passed to Cargo explicitly and is included in the validated output path.

.PARAMETER InspectLocal
    On a Windows host, build the GTK-free balun-discover diagnostic, validate
    its exact output path, and run it with exactly --inspect --local. This is a
    bounded one-command local-discovery diagnostic: it accepts no discovery
    argument passthrough. Combining Diagnostic is allowed but redundant.

.PARAMETER Run
    Build the release desktop application and launch the exact validated output
    path with its console-enabled developer feature so Rust tracing remains
    visible in this PowerShell session. Ordinary packaged release artifacts
    remain GUI-subsystem applications. This cannot be combined with Diagnostic,
    InspectLocal, or a quick-exit mode.

.PARAMETER CargoUpdate
    Unavailable. Run cargo update directly when a deliberate dependency update
    is intended; this build helper never edits the locked dependency graph.

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
    [switch]$ProbePlayback,
    [switch]$Bundle,
    [switch]$Zip,
    [switch]$InnoSetup,
    [switch]$SkipBundle,
    [switch]$NoCargoBuild,
    [AllowNull()][string]$Msys2Root,
    [switch]$Diagnostic,
    [switch]$InspectLocal,
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
$ProductName = 'Balun'
$DesktopBinaryName = 'balun.exe'
$DiagnosticBinaryName = 'balun-discover.exe'
$DesktopRustTarget = 'x86_64-pc-windows-gnullvm'
$MsysEnvironment = 'clang64'
$MsysPackagePrefix = 'mingw-w64-clang-x86_64'
$RequiredCoverageVersion = 'cargo-llvm-cov 0.8.7'
$Msys2RootSpecified = $PSBoundParameters.ContainsKey('Msys2Root')
$DistributionName = 'balun-windows'
$ZipFileName = 'balun-windows.zip'
$InstallerBaseName = 'balun-setup'
$InnoScriptRelativePath = 'build-aux\inno\balun.iss'
$InnoTargetArchitecture = 'x64'
$PlatformProbeFlag = '--balun-platform-runtime-probe'
$PlatformProbeSentinelName = 'balun-platform-runtime-probe.ok'
$PlatformProbeSentinel = "balun-windows-runtime-probe-v1`n"
$ProbeReceiptSuffix = '.probe-v1'
$ProbeReceiptHeader = 'balun-windows-runtime-probe-v1'
$RequiredIconEntryCount = 7
$PlatformProbeDeadlineMs = 90000
$PlatformProbeOutputLimit = 1MB

# Progress goes to the host stream, never the pipeline, so a function that
# returns a value cannot have its result polluted by its own progress lines.
function Write-Info {
    param([string]$Message)
    Write-Host "[balun] $Message"
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

# Keep Tributary's dependency-update entry points recognizable, but reject
# them before resolving Cargo, changing location, probing MSYS2, or starting
# any build, install, package, launch, or network work.
$UnavailableOptions = @()
foreach ($option in @(
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
        'This build helper never edits the locked dependency graph; run cargo ' +
        'update directly when a deliberate dependency update is intended; ' +
        'no external work was started.'
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
    @{ Name = '-Coverage'; Enabled = $Coverage.IsPresent },
    @{ Name = '-ProbePlayback'; Enabled = $ProbePlayback.IsPresent }
)) {
    if ($mode.Enabled) {
        $QuickModes += $mode.Name
    }
}
$PackageModes = @()
foreach ($mode in @(
    @{ Name = '-Bundle'; Enabled = $Bundle.IsPresent },
    @{ Name = '-Zip'; Enabled = $Zip.IsPresent },
    @{ Name = '-InnoSetup'; Enabled = $InnoSetup.IsPresent }
)) {
    if ($mode.Enabled) {
        $PackageModes += $mode.Name
    }
}
$PackageMode = $PackageModes.Count -gt 0
if ($QuickModes.Count -gt 1) {
    Exit-WithUsageError "Quick-exit modes cannot be combined: $($QuickModes -join ', ')"
}
if ($PackageModes.Count -gt 1) {
    Exit-WithUsageError (
        "Package modes cannot be combined: $($PackageModes -join ', '). " +
        '-Zip already includes -Bundle, and -InnoSetup already includes -Zip.'
    )
}
if ($PackageMode -and $QuickModes.Count -gt 0) {
    Exit-WithUsageError "$($PackageModes[0]) cannot be combined with quick-exit mode $($QuickModes[0])."
}
if ($PackageMode -and $Run.IsPresent) {
    Exit-WithUsageError "$($PackageModes[0]) cannot be combined with -Run."
}
if ($PackageMode -and ($Diagnostic.IsPresent -or $InspectLocal.IsPresent)) {
    Exit-WithUsageError "$($PackageModes[0]) packages the desktop application and cannot be combined with -Diagnostic or -InspectLocal."
}
if ($SkipBundle.IsPresent -and ($Bundle.IsPresent -or $Zip.IsPresent)) {
    Exit-WithUsageError '-SkipBundle contradicts -Bundle and -Zip; combine it only with -InnoSetup.'
}
if ($NoCargoBuild.IsPresent -and -not $PackageMode) {
    Exit-WithUsageError '-NoCargoBuild applies only to -Bundle, -Zip, or -InnoSetup.'
}
if ($Run.IsPresent -and $QuickModes.Count -gt 0) {
    Exit-WithUsageError "-Run cannot be combined with quick-exit mode $($QuickModes[0])."
}
if ($InspectLocal.IsPresent -and $QuickModes.Count -gt 0) {
    Exit-WithUsageError (
        "-InspectLocal cannot be combined with quick-exit mode $($QuickModes[0])."
    )
}
if ($Run.IsPresent -and $InspectLocal.IsPresent) {
    Exit-WithUsageError '-Run and -InspectLocal are mutually exclusive launch operations.'
}
if ($Run.IsPresent -and $Diagnostic.IsPresent) {
    Exit-WithUsageError '-Run launches only the desktop application and cannot be combined with -Diagnostic.'
}
if ($ProbePlayback.IsPresent -and $Diagnostic.IsPresent) {
    Exit-WithUsageError '-ProbePlayback exercises the desktop playback runtime and cannot be combined with -Diagnostic.'
}
if ($InspectLocal.IsPresent -and $Msys2RootSpecified) {
    Exit-WithUsageError '-Msys2Root cannot be combined with GTK-free -InspectLocal.'
}
if ($Diagnostic.IsPresent -and $Msys2RootSpecified) {
    Exit-WithUsageError '-Msys2Root applies only to desktop compilation and cannot be combined with -Diagnostic.'
}

function Test-IsWindowsHost {
    return [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
}

if ($Run.IsPresent -and -not (Test-IsWindowsHost)) {
    Exit-WithUsageError '-Run can launch the Windows desktop application only from Windows.'
}
if ($InspectLocal.IsPresent -and -not (Test-IsWindowsHost)) {
    Exit-WithUsageError '-InspectLocal can run the Windows diagnostic only from Windows.'
}
if ($PackageMode -and -not (Test-IsWindowsHost)) {
    Exit-WithUsageError "$($PackageModes[0]) stages and probes the Windows package only from Windows."
}

$DiagnosticMode = $Diagnostic.IsPresent -or $InspectLocal.IsPresent

function Assert-BoundedWindowsRustTarget {
    param([string]$Target)

    if (-not (Test-IsBoundedWindowsRustTarget $Target)) {
        Exit-WithUsageError 'RUST_TARGET must name one bounded Windows Rust target.'
    }
}

function Test-IsBoundedWindowsRustTarget {
    param([AllowNull()][string]$Target)

    return -not ([string]::IsNullOrWhiteSpace($Target) -or
        $Target.Length -gt 128 -or
        $Target -notmatch '^[A-Za-z0-9_][A-Za-z0-9_.-]*$' -or
        $Target -notmatch '-windows-')
}

function Resolve-DiagnosticRustTarget {
    if (-not [string]::IsNullOrWhiteSpace($env:RUST_TARGET)) {
        Assert-BoundedWindowsRustTarget $env:RUST_TARGET
        return $env:RUST_TARGET
    }

    if (-not (Test-IsWindowsHost)) {
        Exit-WithUsageError (
            'A non-Windows host must set RUST_TARGET to an already-installed ' +
            'Windows target; this helper will not install one.'
        )
    }

    $rustcCommand = Get-Command rustc -ErrorAction SilentlyContinue
    if ($null -eq $rustcCommand) {
        Exit-WithError (
            'rustc is unavailable; it is required to resolve the native Windows ' +
            'diagnostic target explicitly.'
        )
    }

    $global:LASTEXITCODE = 0
    $hostOutput = @(& $rustcCommand '--print' 'host-tuple' 2>$null)
    $hostStatus = $LASTEXITCODE
    $hostLines = @(
        $hostOutput |
            Where-Object { $null -ne $_ } |
            ForEach-Object { $_.ToString().Trim() } |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
    if ($hostStatus -ne 0 -or
        $hostLines.Count -ne 1 -or
        -not (Test-IsBoundedWindowsRustTarget $hostLines[0])) {
        Exit-WithError (
            'rustc --print host-tuple did not return one bounded Windows Rust target.'
        )
    }
    return $hostLines[0]
}

function Resolve-DesktopRustTarget {
    if (-not [string]::IsNullOrWhiteSpace($env:RUST_TARGET)) {
        Assert-BoundedWindowsRustTarget $env:RUST_TARGET
        if ($env:RUST_TARGET -cne $DesktopRustTarget) {
            Exit-WithUsageError (
                "The Windows desktop helper requires RUST_TARGET=$DesktopRustTarget " +
                'to match the MSYS2 CLANG64 GTK libraries.'
            )
        }
        return $env:RUST_TARGET
    }

    if (-not (Test-IsWindowsHost)) {
        Exit-WithUsageError (
            "A non-Windows host must set RUST_TARGET=$DesktopRustTarget; " +
            'this helper will not install a cross-compilation target.'
        )
    }
    return $DesktopRustTarget
}

# A read-only replacement for Tributary's automatic `rustup target add`: the
# helper names the missing target and the command to add it, but never
# installs anything itself.
function Assert-DesktopRustTargetInstalled {
    <#
    .SYNOPSIS
        Fail closed unless the selected Rust toolchain has the desktop target.
    .DESCRIPTION
        Asks rustc for the target's library directory and requires it to exist.
        Nothing is installed; the error names the rustup command to run.
    .PARAMETER Target
        The Rust target triple the desktop build will compile for.
    #>
    param([string]$Target)

    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Exit-WithError 'rustc is unavailable; install Rust from https://rustup.rs and retry.'
    }
    $global:LASTEXITCODE = 0
    $libraryDirectories = @(& rustc '--target' $Target '--print' 'target-libdir' 2>$null)
    $libraryDirectory = if ($libraryDirectories.Count -gt 0) {
        [string]$libraryDirectories[0]
    }
    else {
        ''
    }
    if ($LASTEXITCODE -ne 0 -or
        [string]::IsNullOrWhiteSpace($libraryDirectory) -or
        -not (Test-Path -LiteralPath $libraryDirectory -PathType Container)) {
        Exit-WithError (
            "Rust target $Target is not installed for the selected toolchain. Add it " +
            "with 'rustup target add $Target' and retry; this helper never installs targets."
        )
    }
}

function Get-RegularFilePath {
    param([string[]]$Candidates)

    foreach ($candidate in $Candidates) {
        $item = Get-Item -LiteralPath $candidate -Force -ErrorAction SilentlyContinue
        if ($null -ne $item -and
            $item -is [System.IO.FileInfo] -and
            ($item.Attributes -band [System.IO.FileAttributes]::Directory) -eq 0 -and
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0 -and
            $item.Length -gt 0) {
            return $item.FullName
        }
    }
    return $null
}

function Get-Msys2Layout {
    param([string]$Root)

    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction SilentlyContinue
    if ($null -eq $rootItem -or
        ($rootItem.Attributes -band [System.IO.FileAttributes]::Directory) -eq 0) {
        return [pscustomobject]@{
            Valid = $false
            Root = $Root
            Missing = @('installation root')
        }
    }

    $resolvedRoot = $rootItem.FullName
    $prefix = Join-Path $resolvedRoot $MsysEnvironment
    $bin = Join-Path $prefix 'bin'
    $pkgConfigDirectory = Join-Path $prefix 'lib\pkgconfig'
    $pluginDirectory = Join-Path $prefix 'lib\gstreamer-1.0'
    $missing = [System.Collections.Generic.List[string]]::new()

    if (-not (Test-Path -LiteralPath $pkgConfigDirectory -PathType Container)) {
        $missing.Add("$MsysEnvironment\lib\pkgconfig")
    }

    $pkgConfig = Get-RegularFilePath @(
        (Join-Path $bin 'pkg-config.exe'),
        (Join-Path $bin 'pkg-config.cmd'),
        (Join-Path $bin 'pkg-config')
    )
    if ($null -eq $pkgConfig) {
        $missing.Add("$MsysEnvironment\bin\pkg-config.exe")
    }

    $clang = Get-RegularFilePath @(
        (Join-Path $bin 'clang.exe'),
        (Join-Path $bin 'clang')
    )
    if ($null -eq $clang) { $missing.Add("$MsysEnvironment\bin\clang.exe") }

    $clangxx = Get-RegularFilePath @(
        (Join-Path $bin 'clang++.exe'),
        (Join-Path $bin 'clang++')
    )
    if ($null -eq $clangxx) { $missing.Add("$MsysEnvironment\bin\clang++.exe") }

    $archiveTool = Get-RegularFilePath @(
        (Join-Path $bin 'llvm-ar.exe'),
        (Join-Path $bin 'llvm-ar')
    )
    if ($null -eq $archiveTool) { $missing.Add("$MsysEnvironment\bin\llvm-ar.exe") }

    $dllTool = Get-RegularFilePath @(
        (Join-Path $bin 'llvm-dlltool.exe'),
        (Join-Path $bin 'llvm-dlltool')
    )
    if ($null -eq $dllTool) { $missing.Add("$MsysEnvironment\bin\llvm-dlltool.exe") }

    return [pscustomobject]@{
        Valid = $missing.Count -eq 0
        Root = $resolvedRoot
        Prefix = $prefix
        Bin = $bin
        PkgConfigDirectory = $pkgConfigDirectory
        PluginDirectory = $pluginDirectory
        PkgConfig = $pkgConfig
        Clang = $clang
        Clangxx = $clangxx
        ArchiveTool = $archiveTool
        DllTool = $dllTool
        Missing = @($missing)
    }
}

function Add-Msys2RootCandidate {
    param(
        [System.Collections.Generic.List[string]]$Candidates,
        [System.Collections.Generic.HashSet[string]]$Known,
        [AllowNull()][string]$Candidate
    )

    if ([string]::IsNullOrWhiteSpace($Candidate) -or $Candidates.Count -ge 16) {
        return
    }
    if (-not [System.IO.Path]::IsPathFullyQualified($Candidate)) {
        return
    }
    try {
        $fullPath = [System.IO.Path]::GetFullPath($Candidate)
    }
    catch {
        return
    }
    if ($Known.Add($fullPath)) {
        $Candidates.Add($fullPath)
    }
}

function Add-Msys2RootFromToolPath {
    param(
        [System.Collections.Generic.List[string]]$Candidates,
        [System.Collections.Generic.HashSet[string]]$Known,
        [AllowNull()][string]$ToolPath
    )

    if ([string]::IsNullOrWhiteSpace($ToolPath)) { return }
    $binDirectory = Split-Path -Parent $ToolPath
    if ([string]::IsNullOrWhiteSpace($binDirectory) -or
        (Split-Path -Leaf $binDirectory) -ine 'bin') {
        return
    }
    $environmentDirectory = Split-Path -Parent $binDirectory
    if ((Split-Path -Leaf $environmentDirectory) -ine $MsysEnvironment) {
        return
    }
    Add-Msys2RootCandidate $Candidates $Known (Split-Path -Parent $environmentDirectory)
}

function Resolve-Msys2Layout {
    $candidates = [System.Collections.Generic.List[string]]::new()
    $known = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $explicitRoot = $script:Msys2RootSpecified

    if ($explicitRoot) {
        if ([string]::IsNullOrWhiteSpace($Msys2Root)) {
            Exit-WithUsageError '-Msys2Root must name one existing MSYS2 installation root.'
        }
        if (-not [System.IO.Path]::IsPathFullyQualified($Msys2Root)) {
            Exit-WithUsageError '-Msys2Root must be an absolute filesystem path.'
        }
        Add-Msys2RootCandidate $candidates $known $Msys2Root
    }
    else {
        Add-Msys2RootCandidate $candidates $known $env:MSYS2_ROOT

        foreach ($commandName in @('pkg-config.exe', 'pkg-config')) {
            $command = Get-Command $commandName -CommandType Application -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($null -ne $command) {
                Add-Msys2RootFromToolPath $candidates $known $command.Source
            }
        }

        if (Test-IsWindowsHost) {
            Add-Msys2RootCandidate $candidates $known 'C:\msys64'
        }
        if (-not [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
            Add-Msys2RootCandidate $candidates $known (Join-Path $env:RUNNER_TEMP 'msys64')
        }

        if (-not [string]::IsNullOrWhiteSpace($env:PATH)) {
            $separator = [regex]::Escape([string][System.IO.Path]::PathSeparator)
            foreach ($pathEntry in @($env:PATH -split $separator)) {
                if ([string]::IsNullOrWhiteSpace($pathEntry)) { continue }
                $leaf = Split-Path -Leaf $pathEntry
                if ($leaf -ine 'bin') { continue }
                $parent = Split-Path -Parent $pathEntry
                if ((Split-Path -Leaf $parent) -ieq $MsysEnvironment) {
                    Add-Msys2RootCandidate $candidates $known (Split-Path -Parent $parent)
                }
                elseif ((Split-Path -Leaf $parent) -ieq 'usr') {
                    Add-Msys2RootCandidate $candidates $known (Split-Path -Parent $parent)
                }
            }
        }
    }

    $firstIncomplete = $null
    foreach ($candidate in $candidates) {
        $layout = Get-Msys2Layout $candidate
        if ($layout.Valid) { return $layout }
        if ($null -eq $firstIncomplete -and
            -not ($layout.Missing -contains 'installation root')) {
            $firstIncomplete = $layout
        }
    }

    if ($explicitRoot -and $candidates.Count -eq 0) {
        Exit-WithUsageError '-Msys2Root is not a valid filesystem path.'
    }
    if ($explicitRoot -or $null -ne $firstIncomplete) {
        $incomplete = if ($null -ne $firstIncomplete) {
            $firstIncomplete
        }
        else {
            Get-Msys2Layout $candidates[0]
        }
        Exit-WithError (
            "MSYS2 CLANG64 is incomplete under $($incomplete.Root); missing: " +
            "$($incomplete.Missing -join ', '). Install the " +
            'mingw-w64-clang-x86_64-gtk4, ' +
            'mingw-w64-clang-x86_64-libadwaita, ' +
            'mingw-w64-clang-x86_64-gstreamer, ' +
            'mingw-w64-clang-x86_64-pkg-config, and ' +
            'mingw-w64-clang-x86_64-toolchain packages.'
        )
    }

    Exit-WithError (
        'Could not locate an MSYS2 CLANG64 installation automatically. ' +
        'Install MSYS2 at C:\msys64 or pass its root once with -Msys2Root.'
    )
}

function Initialize-DesktopBuildEnvironment {
    param([pscustomobject]$Layout)

    $env:PKG_CONFIG = $Layout.PkgConfig
    $env:PKG_CONFIG_PATH = $Layout.PkgConfigDirectory
    $env:PKG_CONFIG_LIBDIR = $Layout.PkgConfigDirectory
    $env:PKG_CONFIG_ALLOW_CROSS = '1'
    $env:PATH = $Layout.Bin + [System.IO.Path]::PathSeparator + $env:PATH

    $targetToken = $DesktopRustTarget.Replace('-', '_').Replace('.', '_')
    foreach ($targetSuffix in @($DesktopRustTarget, $targetToken)) {
        [Environment]::SetEnvironmentVariable(
            "PKG_CONFIG_$targetSuffix",
            $Layout.PkgConfig,
            'Process'
        )
        [Environment]::SetEnvironmentVariable(
            "PKG_CONFIG_PATH_$targetSuffix",
            $Layout.PkgConfigDirectory,
            'Process'
        )
        [Environment]::SetEnvironmentVariable(
            "PKG_CONFIG_LIBDIR_$targetSuffix",
            $Layout.PkgConfigDirectory,
            'Process'
        )
        [Environment]::SetEnvironmentVariable(
            "PKG_CONFIG_ALLOW_CROSS_$targetSuffix",
            '1',
            'Process'
        )
    }
    [Environment]::SetEnvironmentVariable("DLLTOOL_$targetToken", $Layout.DllTool, 'Process')
    [Environment]::SetEnvironmentVariable("CC_$targetToken", $Layout.Clang, 'Process')
    [Environment]::SetEnvironmentVariable("CXX_$targetToken", $Layout.Clangxx, 'Process')
    [Environment]::SetEnvironmentVariable("AR_$targetToken", $Layout.ArchiveTool, 'Process')
}

function Assert-PkgConfigFloor {
    param(
        [pscustomobject]$Layout,
        [string]$Package,
        [string]$MinimumVersion,
        [string]$InstallPackage
    )

    $global:LASTEXITCODE = 0
    & $Layout.PkgConfig '--atleast-version' $MinimumVersion $Package
    if ($LASTEXITCODE -ne 0) {
        Exit-WithError (
            "$Package >= $MinimumVersion was not found in $($Layout.Prefix). " +
            "Install or update mingw-w64-clang-x86_64-$InstallPackage in MSYS2 CLANG64."
        )
    }
}

# Runtime GStreamer plugins are invisible to pkg-config, and balun.exe checks
# the same structural factories at startup. Fail before a desktop build whose
# only outcome would be "playback components unavailable".
function Assert-PlaybackRuntime {
    param([pscustomobject]$Layout)

    $pluginDirectory = $Layout.PluginDirectory
    if (-not (Test-Path -LiteralPath $pluginDirectory -PathType Container)) {
        Exit-WithError (
            "GStreamer plugin directory $pluginDirectory is missing. Install the " +
            "$MsysPackagePrefix-gstreamer, $MsysPackagePrefix-gst-plugins-base, " +
            "$MsysPackagePrefix-gst-plugins-good, $MsysPackagePrefix-gst-plugins-bad, and " +
            "$MsysPackagePrefix-gst-plugins-rs packages in MSYS2 CLANG64."
        )
    }
    $required = @(
        @{ Plugin = 'libgstcoreelements.dll'; Factories = 'core elements'; Package = 'gstreamer' },
        @{ Plugin = 'libgstplayback.dll'; Factories = 'playbin3, uridecodebin3, decodebin3'; Package = 'gst-plugins-base' },
        @{ Plugin = 'libgstapp.dll'; Factories = 'appsrc'; Package = 'gst-plugins-base' },
        @{ Plugin = 'libgsttypefindfunctions.dll'; Factories = 'stream type detection'; Package = 'gst-plugins-base' },
        @{ Plugin = 'libgstdeinterlace.dll'; Factories = 'deinterlace'; Package = 'gst-plugins-good' },
        @{ Plugin = 'libgstmpegtsdemux.dll'; Factories = 'tsdemux'; Package = 'gst-plugins-bad' },
        @{ Plugin = 'libgstgtk4.dll'; Factories = 'gtk4paintablesink'; Package = 'gst-plugins-rs' }
    )
    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $required) {
        $pluginPath = Join-Path $pluginDirectory $entry.Plugin
        if (-not (Test-Path -LiteralPath $pluginPath -PathType Leaf)) {
            $missing.Add(
                "$($entry.Plugin) ($($entry.Factories)) from $MsysPackagePrefix-$($entry.Package)"
            )
        }
    }
    if ($missing.Count -gt 0) {
        Exit-WithError (
            "Required GStreamer playback runtime is incomplete in ${pluginDirectory}: " +
            ($missing -join '; ') +
            '. Install the matching packages in MSYS2 CLANG64 and retry.'
        )
    }
    $libavPath = Join-Path $pluginDirectory 'libgstlibav.dll'
    if (-not (Test-Path -LiteralPath $libavPath -PathType Leaf)) {
        Write-Warning (
            "libgstlibav.dll is missing from $pluginDirectory; MPEG-2, H.264, AC-3, and AAC " +
            "broadcast decoding commonly needs $MsysPackagePrefix-gst-libav. The build " +
            'continues, but live channels may report a missing codec.'
        )
    }
    Write-Info 'GStreamer runtime plugin checks passed for the structural playback factories.'
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

function Get-ValidatedBuildOutput {
    param([string]$Path, [string]$Label)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -eq $item -or
        $item -isnot [System.IO.FileInfo] -or
        ($item.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0 -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $item.Length -le 0) {
        Exit-WithError (
            "The expected $Label output path is not a nonempty regular, " +
            "non-reparse-point file: $Path"
        )
    }
    return $item
}

# ---------------------------------------------------------------------------
# Packaging: shared release component policy
#
# Balun does not play encrypted optical discs or proprietary DRM media. The
# shared filename-token policy is loaded once per packaging run and applied at
# every copy boundary, during import traversal, over the completed tree, and
# inside the reopened ZIP. Ordinary codecs, TLS, and generic cryptography
# remain eligible for the package.
# ---------------------------------------------------------------------------

$script:ForbiddenBundledComponentTokens = @()

function Import-ForbiddenBundledComponentPolicy {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Exit-WithError "Required bundled-component policy is missing: $Path"
    }

    $tokens = [System.Collections.Generic.List[string]]::new()
    $known = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        $token = $line.Trim()
        if (-not $token -or $token.StartsWith('#')) { continue }
        if ($token -notmatch '^[A-Za-z0-9][A-Za-z0-9._+-]*$') {
            Exit-WithError "Bundled-component policy contains an invalid filename token: '$token'"
        }
        if (-not $known.Add($token)) {
            Exit-WithError "Bundled-component policy contains a duplicate filename token: '$token'"
        }
        $tokens.Add($token)
    }
    if ($tokens.Count -eq 0) {
        Exit-WithError "Bundled-component policy contains no filename tokens: $Path"
    }
    return @($tokens)
}

function Test-ForbiddenBundledComponentName {
    param([AllowNull()][string]$Name)
    if ([string]::IsNullOrWhiteSpace($Name)) { return $false }
    $fileName = [System.IO.Path]::GetFileName($Name)
    foreach ($token in $script:ForbiddenBundledComponentTokens) {
        if ($fileName.IndexOf($token, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
            return $true
        }
    }
    return $false
}

function Test-ForbiddenBundledRelativePath {
    param([AllowNull()][string]$RelativePath)
    if ([string]::IsNullOrWhiteSpace($RelativePath)) { return $false }
    foreach ($component in @($RelativePath -split '[\\/]')) {
        if ($component -and (Test-ForbiddenBundledComponentName $component)) {
            return $true
        }
    }
    return $false
}

# Enumerate a filesystem tree ourselves so PowerShell 5.1 and 7 use the same
# rule: include hidden files and directories, report reparse points as
# members, but never recurse through a junction/symlink into an external tree.
function Get-WindowsTreeMembersWithoutReparseTraversal {
    param([string]$Root)
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { return @() }

    $members = [System.Collections.Generic.List[System.IO.FileSystemInfo]]::new()
    $pendingDirectories = [System.Collections.Queue]::new()
    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    $rootIsReparsePoint = ($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    if ($rootIsReparsePoint) {
        $members.Add($rootItem)
        return @($members)
    }
    $pendingDirectories.Enqueue($rootItem.FullName)
    while ($pendingDirectories.Count -gt 0) {
        $directory = [string]$pendingDirectories.Dequeue()
        foreach ($member in @(Get-ChildItem -LiteralPath $directory -Force -ErrorAction Stop)) {
            $members.Add($member)
            $isDirectory = ($member.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0
            $isReparsePoint = ($member.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
            if ($isDirectory -and -not $isReparsePoint) {
                $pendingDirectories.Enqueue($member.FullName)
            }
        }
    }
    return @($members)
}

function Get-ForbiddenWindowsBundleMembers {
    param([string]$Root)
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { return @() }

    $rootFull = (Get-Item -LiteralPath $Root -Force -ErrorAction Stop).FullName.TrimEnd(
        [char[]]@('\', '/')
    )
    return @(Get-WindowsTreeMembersWithoutReparseTraversal $rootFull | Where-Object {
        $relativePath = $_.FullName.Substring($rootFull.Length).TrimStart([char[]]@('\', '/'))
        Test-ForbiddenBundledRelativePath $relativePath
    })
}

function Get-WindowsBundleReparsePointMembers {
    param([string]$Root)
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { return @() }
    return @(Get-WindowsTreeMembersWithoutReparseTraversal $Root | Where-Object {
        ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
    })
}

function Assert-WindowsBundleRootIsNotReparsePoint {
    param([string]$Root)
    $rootItem = Get-Item -LiteralPath $Root -Force -ErrorAction Stop
    if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Exit-WithError "Windows bundle root must not be a filesystem reparse point: $($rootItem.FullName)"
    }
}

# Delete selected entries deepest-first and non-recursively. DirectoryInfo's
# zero-argument Delete removes a directory or reparse-point link itself; it
# cannot walk into a junction target. Every real descendant was enumerated and
# selected separately because its relative path inherits the forbidden parent.
function Remove-ForbiddenWindowsBundleMembers {
    param([string]$Root)
    $forbiddenMembers = @(Get-ForbiddenWindowsBundleMembers $Root | Sort-Object `
        @{ Expression = { $_.FullName.Length }; Descending = $true })
    foreach ($member in $forbiddenMembers) {
        try {
            if (($member.Attributes -band [System.IO.FileAttributes]::ReadOnly) -ne 0) {
                $member.Attributes = $member.Attributes -band (-bnot [System.IO.FileAttributes]::ReadOnly)
            }
            $member.Delete()
        }
        catch [System.IO.FileNotFoundException] { }
        catch [System.IO.DirectoryNotFoundException] { }
        catch {
            throw "Could not safely remove forbidden bundle member '$($member.FullName)': $($_.Exception.Message)"
        }
    }
    return $forbiddenMembers.Count
}

function Assert-WindowsBundleComponentPolicy {
    param([string]$Root)
    $forbiddenMembers = @(Get-ForbiddenWindowsBundleMembers $Root)
    $reparsePointMembers = @(Get-WindowsBundleReparsePointMembers $Root)
    if ($forbiddenMembers.Count -eq 0 -and $reparsePointMembers.Count -eq 0) { return }

    $forbiddenSample = @($forbiddenMembers | Select-Object -First 8 | ForEach-Object {
        $relativePath = $_.FullName.Substring($Root.Length).TrimStart('\', '/')
        if ($relativePath) { $relativePath } else { '<bundle-root>' }
    }) -join ', '
    if ($forbiddenMembers.Count -gt 8) { $forbiddenSample += ', ...' }
    $reparseSample = @($reparsePointMembers | Select-Object -First 8 | ForEach-Object {
        $relativePath = $_.FullName.Substring($Root.Length).TrimStart('\', '/')
        if ($relativePath) { $relativePath } else { '<bundle-root>' }
    }) -join ', '
    if ($reparsePointMembers.Count -gt 8) { $reparseSample += ', ...' }

    $details = [System.Collections.Generic.List[string]]::new()
    if ($forbiddenMembers.Count -gt 0) {
        $details.Add("$($forbiddenMembers.Count) forbidden component(s): $forbiddenSample")
    }
    if ($reparsePointMembers.Count -gt 0) {
        $details.Add("$($reparsePointMembers.Count) filesystem reparse point(s): $reparseSample")
    }
    Exit-WithError "Windows bundle violates the bundled-component policy ($($details -join '; '))"
}

# Reopen the completed ZIP and require its entry set to be exactly the staged
# tree's file set under the distribution folder, with no forbidden name.
function Assert-WindowsZipMatchesTree {
    param(
        [string]$Path,
        [string]$Root
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Exit-WithError "Completed Windows ZIP was not found for validation: $Path"
    }
    $rootFull = (Get-Item -LiteralPath $Root -Force -ErrorAction Stop).FullName.TrimEnd(
        [char[]]@('\', '/')
    )
    $rootLeaf = Split-Path -Leaf $rootFull
    $expected = [System.Collections.Generic.Dictionary[string, int64]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($member in @(Get-WindowsTreeMembersWithoutReparseTraversal $rootFull)) {
        if (($member.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0) { continue }
        $relativePath = $member.FullName.Substring($rootFull.Length).TrimStart([char[]]@('\', '/'))
        $expected["$rootLeaf/" + $relativePath.Replace('\', '/')] = [int64]$member.Length
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem -ErrorAction Stop
    $archive = [System.IO.Compression.ZipFile]::OpenRead(
        (Resolve-Path -LiteralPath $Path).ProviderPath
    )
    $forbiddenEntryCount = 0
    $forbiddenEntrySample = [System.Collections.Generic.List[string]]::new()
    $unexpectedEntries = [System.Collections.Generic.List[string]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    try {
        foreach ($entry in $archive.Entries) {
            $entryPath = $entry.FullName.Replace('\', '/').TrimEnd('/')
            if (-not $entryPath) { continue }
            if (Test-ForbiddenBundledRelativePath $entryPath) {
                $forbiddenEntryCount++
                if ($forbiddenEntrySample.Count -lt 8) {
                    $forbiddenEntrySample.Add($entry.FullName)
                }
            }
            if ($entry.FullName.EndsWith('/') -or $entry.FullName.EndsWith('\')) { continue }
            if (-not $expected.ContainsKey($entryPath) -or
                $expected[$entryPath] -ne [int64]$entry.Length -or
                -not $seen.Add($entryPath)) {
                if ($unexpectedEntries.Count -lt 8) { $unexpectedEntries.Add($entry.FullName) }
                elseif ($unexpectedEntries.Count -eq 8) { $unexpectedEntries.Add('...') }
            }
        }
    }
    finally {
        $archive.Dispose()
    }

    if ($forbiddenEntryCount -gt 0) {
        $sample = $forbiddenEntrySample -join ', '
        if ($forbiddenEntryCount -gt $forbiddenEntrySample.Count) { $sample += ', ...' }
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
        Exit-WithError "Completed Windows ZIP contains $forbiddenEntryCount forbidden entry name(s): $sample"
    }
    if ($unexpectedEntries.Count -gt 0) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
        Exit-WithError "Completed Windows ZIP contains entries outside the staged tree: $($unexpectedEntries -join ', ')"
    }
    if ($seen.Count -ne $expected.Count) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
        Exit-WithError "Completed Windows ZIP holds $($seen.Count) file(s) but the staged tree holds $($expected.Count)."
    }
}

function Get-BoundedProbeDiagnostic {
    param(
        [string]$Path,
        [string]$Label,
        [int]$Limit = 32768
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '' }

    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try {
        $length = $stream.Length
        $count = [int][Math]::Min([int64]$Limit, $length)
        if ($length -gt $count) { $null = $stream.Seek(-$count, [System.IO.SeekOrigin]::End) }
        $bytes = [byte[]]::new($count)
        $offset = 0
        while ($offset -lt $count) {
            $read = $stream.Read($bytes, $offset, $count - $offset)
            if ($read -eq 0) { break }
            $offset += $read
        }
        $text = [System.Text.Encoding]::UTF8.GetString($bytes, 0, $offset)
        $prefix = if ($length -gt $count) { "[earlier $Label output truncated; showing final $count bytes]`n" } else { '' }
        return "$prefix$text"
    }
    finally {
        $stream.Dispose()
    }
}

function Stop-BoundedProcessTree {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$Label
    )
    if ($null -eq $Process -or $Process.HasExited) { return }

    # Process.Kill(bool) is unavailable in Windows PowerShell 5.1's .NET
    # Framework. Prefer it when present; otherwise use the absolute inbox
    # taskkill path so termination never depends on the sanitized PATH.
    $killTreeMethod = $Process.GetType().GetMethods() |
        Where-Object {
            $_.Name -eq 'Kill' -and
            $_.GetParameters().Count -eq 1 -and
            ($_.GetParameters())[0].ParameterType -eq [bool]
        } |
        Select-Object -First 1

    $useTaskkill = $null -eq $killTreeMethod
    if (-not $useTaskkill) {
        try {
            $null = $killTreeMethod.Invoke($Process, [object[]]@($true))
        }
        catch {
            $useTaskkill = -not $Process.HasExited
        }
    }

    $taskkillFailure = $null
    if ($useTaskkill) {
        $system32 = [System.Environment]::SystemDirectory
        $taskkillPath = Join-Path $system32 'taskkill.exe'
        $taskkillProcess = $null
        try {
            if (-not [System.IO.Path]::IsPathRooted($taskkillPath) -or
                -not (Test-Path -LiteralPath $taskkillPath -PathType Leaf)) {
                throw 'absolute System32 taskkill.exe was not available'
            }

            $taskkillInfo = [System.Diagnostics.ProcessStartInfo]::new()
            $taskkillInfo.FileName = $taskkillPath
            $taskkillInfo.Arguments = "/PID $($Process.Id) /T /F"
            $taskkillInfo.UseShellExecute = $false
            $taskkillInfo.CreateNoWindow = $true
            $taskkillProcess = [System.Diagnostics.Process]::new()
            $taskkillProcess.StartInfo = $taskkillInfo
            if (-not $taskkillProcess.Start()) {
                throw 'absolute System32 taskkill.exe could not start'
            }
            if (-not $taskkillProcess.WaitForExit(10000)) {
                try { $taskkillProcess.Kill() } catch { }
                $null = $taskkillProcess.WaitForExit(1000)
                throw 'absolute System32 taskkill.exe exceeded its 10-second deadline'
            }
            if ($taskkillProcess.ExitCode -ne 0 -and -not $Process.HasExited) {
                throw 'absolute System32 taskkill.exe could not terminate the probe tree'
            }
        }
        catch {
            $taskkillFailure = $_.Exception.Message
            if (-not $Process.HasExited) {
                try { $Process.Kill() } catch { }
            }
        }
        finally {
            if ($null -ne $taskkillProcess) { $taskkillProcess.Dispose() }
        }
    }

    if (-not $Process.WaitForExit(10000)) {
        throw "$Label process tree did not terminate within 10 seconds"
    }
    if ($taskkillFailure) {
        throw "$Label required degraded termination: $taskkillFailure"
    }
}

# ---------------------------------------------------------------------------
# Packaging: bounded, non-executing PE inspection
# ---------------------------------------------------------------------------

# Extract one dependency basename from llvm-readobj's PE import-table form:
#   Import {
#     Name: libfoo.dll
#   }
# Windows also exposes a small number of legacy system modules with a .drv
# suffix, notably WINSPOOL.DRV imported by current GTK packages.
# --coff-imports includes ordinary and delay-load imports. Reject every other
# suffix, path separator, and invalid filename character so inspector output
# can never redirect a copy outside the selected MSYS2 bin folder.
function Get-PeImportDependencyName {
    param([string]$Line)
    if ($Line -notmatch '^\s*Name:\s*([^\\/:*?"<>|\x00-\x1F]+\.(?:dll|drv))\s*$') { return $null }
    return $matches[1]
}

function Format-PeImportTargetForDiagnostic {
    param(
        [AllowNull()][string]$Target,
        [int]$Limit = 192
    )
    if ($null -eq $Target) { return '<null>' }

    $safe = [System.Text.RegularExpressions.Regex]::Replace(
        $Target,
        '[\p{Cc}\p{Zl}\p{Zp}"]',
        '?'
    )
    if ($safe.Length -gt $Limit) {
        $safe = $safe.Substring(0, $Limit) + '...[truncated]'
    }
    return "'$safe'"
}

function ConvertTo-BoundedPeImportArgumentBatch {
    param(
        [AllowNull()][System.Collections.Generic.List[string]]$TargetBatch,
        [int]$ArgumentCharacterLimit
    )
    if ($null -eq $TargetBatch) {
        throw 'PE import-inspection target batch was null'
    }

    $quotedTargets = [System.Collections.Generic.List[string]]::new()
    $targetNames = [System.Collections.Generic.List[string]]::new()
    for ($targetIndex = 0; $targetIndex -lt $TargetBatch.Count; $targetIndex++) {
        $target = $TargetBatch[$targetIndex]
        $targetNumber = $targetIndex + 1
        $targetLabel = Format-PeImportTargetForDiagnostic $target

        if ([string]::IsNullOrWhiteSpace($target)) {
            throw "PE import-inspection target #$targetNumber was empty ($targetLabel)"
        }
        if ([System.Text.RegularExpressions.Regex]::IsMatch(
                $target,
                '[\p{Cc}\p{Zl}\p{Zp}"]'
            )) {
            throw "PE import-inspection target #$targetNumber contains an unsupported quote or control character ($targetLabel)"
        }
        if (-not [System.IO.Path]::IsPathRooted($target)) {
            throw "PE import-inspection target #$targetNumber is not absolute ($targetLabel)"
        }

        try {
            $normalizedTarget = [System.IO.Path]::GetFullPath($target)
        }
        catch {
            throw "PE import-inspection target #$targetNumber is not a valid filesystem path ($targetLabel)"
        }
        $extension = [System.IO.Path]::GetExtension($normalizedTarget)
        if ($extension -ine '.dll' -and $extension -ine '.drv' -and
            $extension -ine '.exe') {
            throw "PE import-inspection target #$targetNumber is not a DLL, DRV, or EXE ($targetLabel)"
        }
        if (-not [System.IO.File]::Exists($normalizedTarget)) {
            throw "PE import-inspection target #$targetNumber is not an existing file ($targetLabel)"
        }

        $quotedTargets.Add('"' + $normalizedTarget + '"')
        if ($targetNames.Count -lt 3) {
            $targetNames.Add([System.IO.Path]::GetFileName($normalizedTarget))
        }
    }

    $arguments = '--coff-imports ' + ($quotedTargets -join ' ')
    if ($arguments.Length -gt $ArgumentCharacterLimit) {
        throw "PE import-inspection batch exceeded its $ArgumentCharacterLimit-character command-line limit"
    }

    $batchLabel = $targetNames -join ', '
    if ($TargetBatch.Count -gt $targetNames.Count) { $batchLabel += ', ...' }
    return [PSCustomObject]@{
        Arguments = $arguments
        Label = $batchLabel
    }
}

# Run one bounded inspector invocation without loading or executing a target.
# Start-Process redirects both streams straight to files, avoiding deadlocks
# from synchronous whole-stream pipe reads. File sizes are polled while the
# process runs and checked again after exit; only then is the capped stdout
# read into memory for parsing.
function Invoke-BoundedInspector {
    param(
        [string]$Inspector,
        [string]$Arguments,
        [string]$Label,
        [string]$TokenPrefix,
        [AllowNull()][System.Diagnostics.Stopwatch]$ClosureClock,
        [int]$ClosureDeadlineMs,
        [int]$ProcessDeadlineMs,
        [int64]$OutputByteLimit
    )

    $token = [Guid]::NewGuid().ToString('N')
    $tempRoot = [System.IO.Path]::GetTempPath()
    $stdoutPath = Join-Path $tempRoot "balun-$TokenPrefix-$token.stdout"
    $stderrPath = Join-Path $tempRoot "balun-$TokenPrefix-$token.stderr"
    $process = $null
    $processClock = [System.Diagnostics.Stopwatch]::StartNew()
    $failure = $null
    $diagnostic = ''
    $stdoutLines = @()

    try {
        $process = Start-Process -FilePath $Inspector -ArgumentList $Arguments `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath `
            -NoNewWindow -PassThru

        while (-not $process.WaitForExit(50)) {
            $stdoutLength = if (Test-Path -LiteralPath $stdoutPath -PathType Leaf) {
                (Get-Item -LiteralPath $stdoutPath).Length
            } else { 0 }
            $stderrLength = if (Test-Path -LiteralPath $stderrPath -PathType Leaf) {
                (Get-Item -LiteralPath $stderrPath).Length
            } else { 0 }
            if (($stdoutLength + $stderrLength) -gt $OutputByteLimit) {
                throw "PE inspector output crossed its $OutputByteLimit-byte limit ($Label)"
            }
            if ($processClock.ElapsedMilliseconds -ge $ProcessDeadlineMs) {
                throw "PE inspector exceeded its $ProcessDeadlineMs-millisecond deadline ($Label)"
            }
            if ($null -ne $ClosureClock -and $ClosureClock.ElapsedMilliseconds -ge $ClosureDeadlineMs) {
                throw "PE import dependency closure exceeded its $ClosureDeadlineMs-millisecond deadline"
            }
        }

        $stdoutLength = if (Test-Path -LiteralPath $stdoutPath -PathType Leaf) {
            (Get-Item -LiteralPath $stdoutPath).Length
        } else { 0 }
        $stderrLength = if (Test-Path -LiteralPath $stderrPath -PathType Leaf) {
            (Get-Item -LiteralPath $stderrPath).Length
        } else { 0 }
        if (($stdoutLength + $stderrLength) -gt $OutputByteLimit) {
            throw "PE inspector output crossed its $OutputByteLimit-byte limit ($Label)"
        }
        if ($processClock.ElapsedMilliseconds -ge $ProcessDeadlineMs) {
            throw "PE inspector exceeded its $ProcessDeadlineMs-millisecond deadline ($Label)"
        }
        if ($null -ne $ClosureClock -and $ClosureClock.ElapsedMilliseconds -ge $ClosureDeadlineMs) {
            throw "PE import dependency closure exceeded its $ClosureDeadlineMs-millisecond deadline"
        }
        if ($process.ExitCode -ne 0) {
            throw "PE inspector exited with status $($process.ExitCode) ($Label)"
        }
        $stdoutLines = @([System.IO.File]::ReadAllLines($stdoutPath))
    }
    catch {
        $failure = $_.Exception.Message
    }
    finally {
        try {
            Stop-BoundedProcessTree $process 'PE inspector'
        }
        catch {
            if ($failure) { $failure += "; $($_.Exception.Message)" }
            else { $failure = $_.Exception.Message }
        }
        if ($failure) {
            $diagnostic = Get-BoundedProbeDiagnostic $stderrPath 'PE inspector stderr' 8192
            if (-not $diagnostic) {
                $diagnostic = Get-BoundedProbeDiagnostic $stdoutPath 'PE inspector stdout' 8192
            }
        }
        if ($null -ne $process) { $process.Dispose() }
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }

    if ($failure) {
        if ($diagnostic) { throw "$failure`n$diagnostic" }
        throw $failure
    }
    return $stdoutLines
}

function Invoke-BoundedPeImportBatch {
    param(
        [string]$Inspector,
        [AllowNull()][System.Collections.Generic.List[string]]$TargetBatch,
        [System.Diagnostics.Stopwatch]$ClosureClock,
        [int]$ClosureDeadlineMs,
        [int]$ProcessDeadlineMs,
        [int64]$OutputByteLimit,
        [int]$ArgumentCharacterLimit
    )
    if ($null -eq $TargetBatch) {
        throw 'PE import-inspection target batch was null'
    }
    if ($TargetBatch.Count -eq 0) { return @() }

    $argumentBatch = ConvertTo-BoundedPeImportArgumentBatch `
        -TargetBatch $TargetBatch `
        -ArgumentCharacterLimit $ArgumentCharacterLimit
    return @(Invoke-BoundedInspector `
        -Inspector $Inspector `
        -Arguments $argumentBatch.Arguments `
        -Label $argumentBatch.Label `
        -TokenPrefix 'readobj' `
        -ClosureClock $ClosureClock `
        -ClosureDeadlineMs $ClosureDeadlineMs `
        -ProcessDeadlineMs $ProcessDeadlineMs `
        -OutputByteLimit $OutputByteLimit)
}

# Inspect the application's PE resources without loading or executing it.
# llvm-readobj emits the raw resource payload as hex; the shipped icon set is
# under 200 KiB, and an unexpectedly huge resource table is a packaging failure.
function Invoke-BoundedPeResourceInspection {
    param(
        [string]$Inspector,
        [string]$Application
    )

    $applicationLabel = Format-PeImportTargetForDiagnostic -Target $Application
    if ([string]::IsNullOrWhiteSpace($Application) -or
        [System.Text.RegularExpressions.Regex]::IsMatch(
            $Application,
            '[\p{Cc}\p{Zl}\p{Zp}"]'
        )) {
        throw "PE resource-inspection target contains an unsupported quote or control character ($applicationLabel)"
    }

    $arguments = '--coff-resources "' + $Application + '"'
    if ($arguments.Length -gt 24000) {
        throw 'PE resource-inspection command exceeded its 24000-character limit'
    }
    return @(Invoke-BoundedInspector `
        -Inspector $Inspector `
        -Arguments $arguments `
        -Label $applicationLabel `
        -TokenPrefix 'resource-readobj' `
        -ClosureClock $null `
        -ClosureDeadlineMs 0 `
        -ProcessDeadlineMs 45000 `
        -OutputByteLimit 8388608)
}

# Fail closed unless the application executable carries its complete Windows
# shell identity. RT_ICON supplies the image payloads, RT_GROUP_ICON is what
# Explorer and shortcuts resolve, and RT_VERSION carries the user-visible
# product metadata. Checking the copied application at each artifact boundary
# prevents a successful Cargo build from silently publishing a generic-icon
# executable when package topology or linker behavior changes.
function Assert-WindowsApplicationResourceContract {
    param(
        [string]$Application,
        [string]$Inspector,
        [string]$ExpectedVersion
    )

    $applicationLabel = Format-PeImportTargetForDiagnostic $Application
    if ([string]::IsNullOrWhiteSpace($Application) -or
        -not [System.IO.Path]::IsPathRooted($Application)) {
        throw "Windows application resource target must be absolute ($applicationLabel)"
    }
    $applicationFull = [System.IO.Path]::GetFullPath($Application)
    if (-not (Test-Path -LiteralPath $applicationFull -PathType Leaf)) {
        throw "Windows application resource target was not found: $applicationLabel"
    }
    $applicationItem = Get-Item -LiteralPath $applicationFull -Force -ErrorAction Stop
    if (($applicationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Windows application resource target must not be a filesystem reparse point: $applicationLabel"
    }
    if ($applicationItem.Name -ine $DesktopBinaryName) {
        throw "Windows application resource target must be ${DesktopBinaryName}: $applicationLabel"
    }

    if ([string]::IsNullOrWhiteSpace($Inspector) -or
        -not [System.IO.Path]::IsPathRooted($Inspector)) {
        throw 'PE resource inspector path must be absolute'
    }
    $inspectorFull = [System.IO.Path]::GetFullPath($Inspector)
    if (-not (Test-Path -LiteralPath $inspectorFull -PathType Leaf)) {
        throw "Required PE resource inspector was not found: $inspectorFull"
    }
    $inspectorItem = Get-Item -LiteralPath $inspectorFull -Force -ErrorAction Stop
    if (($inspectorItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "PE resource inspector must not be a filesystem reparse point: $inspectorFull"
    }

    if ([string]::IsNullOrWhiteSpace($ExpectedVersion) -or
        [System.Text.RegularExpressions.Regex]::IsMatch(
            $ExpectedVersion,
            '[\p{Cc}\p{Zl}\p{Zp}"]'
        )) {
        throw 'Expected Windows application version is empty or contains unsupported characters'
    }

    Assert-MzHeader $applicationFull "Windows application resource target ($applicationLabel)"

    $applicationSnapshot = "$($applicationItem.Length):$($applicationItem.LastWriteTimeUtc.Ticks)"
    $lines = @(Invoke-BoundedPeResourceInspection `
        -Inspector $inspectorFull `
        -Application $applicationFull)

    $requiredTypeNames = @{
        '3' = 'ICON'
        '14' = 'GROUP_ICON'
        '16' = 'VERSIONINFO'
    }
    $requiredDataCounts = @{
        '3' = $RequiredIconEntryCount
        '14' = 1
        '16' = 1
    }
    $typeCounts = @{ '3' = 0; '14' = 0; '16' = 0 }
    $dataCounts = @{ '3' = 0; '14' = 0; '16' = 0 }
    $iconDataSizes = @{}
    $groupIconBytes = [System.Collections.Generic.List[byte]]::new()
    $groupIconDeclaredSize = $null
    $groupIconDataSeen = $false
    $capturingGroupIconData = $false
    $currentTypeId = ''
    $currentResourceNameId = $null
    $totalResourceCount = $null
    $totalResourceLines = 0

    foreach ($lineValue in $lines) {
        $line = [string]$lineValue
        if ($capturingGroupIconData) {
            if ($line -match '^\s*\)\s*$') {
                $capturingGroupIconData = $false
                continue
            }
            if ($line -notmatch '^\s*([0-9A-Fa-f]+):\s+([0-9A-Fa-f]+(?:\s+[0-9A-Fa-f]+)*)\s+\|.*$') {
                throw 'GROUP_ICON resource contains a malformed hexadecimal payload'
            }
            $lineOffset = [Convert]::ToInt64($matches[1], 16)
            if ($lineOffset -ne $groupIconBytes.Count) {
                throw 'GROUP_ICON resource contains a discontinuous hexadecimal payload'
            }
            $hexPayload = ''
            foreach ($hexWord in @($matches[2] -split '\s+')) {
                if ($hexWord.Length -lt 2 -or $hexWord.Length -gt 8 -or
                    ($hexWord.Length % 2) -ne 0) {
                    throw 'GROUP_ICON resource contains a malformed hexadecimal word'
                }
                $hexPayload += $hexWord
            }
            for ($hexOffset = 0; $hexOffset -lt $hexPayload.Length; $hexOffset += 2) {
                if ($groupIconBytes.Count -ge [int64]$groupIconDeclaredSize) {
                    throw 'GROUP_ICON hexadecimal payload exceeds its declared size'
                }
                $groupIconBytes.Add(
                    [Convert]::ToByte($hexPayload.Substring($hexOffset, 2), 16)
                )
            }
            continue
        }
        if ($line -match '^\s*Total Number of Resources:\s*([0-9]+)\s*$') {
            $totalResourceLines++
            $totalResourceCount = [int64]$matches[1]
            continue
        }
        if ($line -match '^\s*Type:') {
            $currentTypeId = ''
            $currentResourceNameId = $null
            if ($line -notmatch '^\s*Type:\s*([A-Z_]+)\s+\(ID\s+([0-9]+)\)\s*\[\s*$') {
                continue
            }
            $resourceTypeName = $matches[1]
            $currentTypeId = $matches[2]
            if ($requiredTypeNames.ContainsKey($currentTypeId)) {
                if ($resourceTypeName -cne $requiredTypeNames[$currentTypeId]) {
                    throw "PE resource ID $currentTypeId used unexpected type name '$resourceTypeName'"
                }
                $typeCounts[$currentTypeId] = [int]$typeCounts[$currentTypeId] + 1
            }
            continue
        }
        if ($line -match '^\s*Name:') {
            $currentResourceNameId = $null
            if ($line -match '^\s*Name:\s*\(ID\s+([0-9]+)\)\s*\[\s*$') {
                $currentResourceNameId = $matches[1]
            }
            elseif ($currentTypeId -eq '3' -or $currentTypeId -eq '14') {
                throw "Required $($requiredTypeNames[$currentTypeId]) resource used a nonnumeric name"
            }
            continue
        }
        if ($line -match '^\s*DataSize:\s*([0-9]+)\s*$' -and
            $requiredTypeNames.ContainsKey($currentTypeId)) {
            $dataSize = [int64]$matches[1]
            if ($dataSize -le 0) {
                throw "Required $($requiredTypeNames[$currentTypeId]) resource has an empty payload"
            }
            $dataCounts[$currentTypeId] = [int]$dataCounts[$currentTypeId] + 1
            if ($currentTypeId -eq '3') {
                if ($null -eq $currentResourceNameId) {
                    throw 'ICON resource payload is missing its numeric resource ID'
                }
                if ($iconDataSizes.ContainsKey($currentResourceNameId)) {
                    throw "ICON resource ID $currentResourceNameId has multiple payloads"
                }
                $iconDataSizes[$currentResourceNameId] = $dataSize
            }
            elseif ($currentTypeId -eq '14') {
                if ($null -eq $currentResourceNameId) {
                    throw 'GROUP_ICON resource payload is missing its numeric resource ID'
                }
                if ($null -ne $groupIconDeclaredSize) {
                    throw "Final $DesktopBinaryName contains multiple GROUP_ICON payloads"
                }
                $groupIconDeclaredSize = $dataSize
            }
            continue
        }
        if ($line -match '^\s*Data\s+\(\s*$' -and $currentTypeId -eq '14') {
            if ($null -eq $groupIconDeclaredSize -or $groupIconDataSeen) {
                throw 'GROUP_ICON resource contains an unexpected hexadecimal payload'
            }
            $groupIconDataSeen = $true
            $capturingGroupIconData = $true
        }
    }

    if ($capturingGroupIconData) {
        throw 'GROUP_ICON resource contains a truncated hexadecimal payload'
    }
    if ($totalResourceLines -ne 1 -or $null -eq $totalResourceCount -or
        $totalResourceCount -lt ($RequiredIconEntryCount + 2)) {
        throw 'PE resource inspector did not report one plausible resource total'
    }
    foreach ($resourceTypeId in @('3', '14', '16')) {
        $resourceTypeName = $requiredTypeNames[$resourceTypeId]
        if ([int]$typeCounts[$resourceTypeId] -ne 1) {
            throw "Final $DesktopBinaryName must contain exactly one $resourceTypeName resource table"
        }
        $requiredDataCount = [int]$requiredDataCounts[$resourceTypeId]
        if ([int]$dataCounts[$resourceTypeId] -ne $requiredDataCount) {
            throw "Final $DesktopBinaryName must contain exactly $requiredDataCount nonempty $resourceTypeName resource(s)"
        }
    }

    if (-not $groupIconDataSeen -or $null -eq $groupIconDeclaredSize) {
        throw "Final $DesktopBinaryName GROUP_ICON payload was not reported"
    }
    if ($groupIconBytes.Count -ne [int64]$groupIconDeclaredSize) {
        throw 'GROUP_ICON hexadecimal payload does not match its declared size'
    }
    if ($groupIconBytes.Count -lt 6) {
        throw 'GROUP_ICON payload is too short to contain a directory header'
    }
    $groupIconPayload = $groupIconBytes.ToArray()
    $groupIconReserved = [BitConverter]::ToUInt16($groupIconPayload, 0)
    $groupIconType = [BitConverter]::ToUInt16($groupIconPayload, 2)
    $groupIconEntryCount = [BitConverter]::ToUInt16($groupIconPayload, 4)
    if ($groupIconReserved -ne 0 -or $groupIconType -ne 1) {
        throw 'GROUP_ICON payload has an invalid directory header'
    }
    if ($groupIconEntryCount -ne $RequiredIconEntryCount) {
        throw "GROUP_ICON directory must contain exactly $RequiredIconEntryCount entries"
    }
    $expectedGroupIconSize = 6 + (14 * [int]$groupIconEntryCount)
    if ($groupIconPayload.Length -ne $expectedGroupIconSize) {
        throw 'GROUP_ICON payload length does not match its directory entry count'
    }

    $groupIconResourceIds = @{}
    for ($entryIndex = 0; $entryIndex -lt $groupIconEntryCount; $entryIndex++) {
        $entryOffset = 6 + (14 * $entryIndex)
        $bytesInResource = [BitConverter]::ToUInt32($groupIconPayload, $entryOffset + 8)
        $iconResourceId = [BitConverter]::ToUInt16($groupIconPayload, $entryOffset + 12)
        $iconResourceIdKey = [string]$iconResourceId
        if ($groupIconResourceIds.ContainsKey($iconResourceIdKey)) {
            throw "GROUP_ICON directory contains duplicate ICON resource ID $iconResourceId"
        }
        $groupIconResourceIds[$iconResourceIdKey] = $true
        if (-not $iconDataSizes.ContainsKey($iconResourceIdKey)) {
            throw "GROUP_ICON directory references missing ICON resource ID $iconResourceId"
        }
        if ([uint64]$bytesInResource -ne [uint64]$iconDataSizes[$iconResourceIdKey]) {
            throw "GROUP_ICON directory size does not match ICON resource ID $iconResourceId"
        }
    }
    if ($groupIconResourceIds.Count -ne $iconDataSizes.Count) {
        throw 'GROUP_ICON directory does not reference the complete ICON resource set'
    }
    foreach ($iconResourceIdKey in $iconDataSizes.Keys) {
        if (-not $groupIconResourceIds.ContainsKey($iconResourceIdKey)) {
            throw "GROUP_ICON directory does not reference ICON resource ID $iconResourceIdKey"
        }
    }

    $versionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($applicationFull)
    if ([string]$versionInfo.ProductName -cne $ProductName) {
        throw "Final $DesktopBinaryName ProductName metadata must be '$ProductName'"
    }
    if ([string]$versionInfo.FileDescription -cne $ProductName) {
        throw "Final $DesktopBinaryName FileDescription metadata must be '$ProductName'"
    }
    if ([string]$versionInfo.FileVersion -cne $ExpectedVersion) {
        throw "Final $DesktopBinaryName FileVersion metadata does not match package version $ExpectedVersion"
    }
    if ([string]$versionInfo.ProductVersion -cne $ExpectedVersion) {
        throw "Final $DesktopBinaryName ProductVersion metadata does not match package version $ExpectedVersion"
    }
    if ([string]::IsNullOrWhiteSpace([string]$versionInfo.LegalCopyright)) {
        throw "Final $DesktopBinaryName LegalCopyright metadata is empty"
    }

    $finalApplicationItem = Get-Item -LiteralPath $applicationFull -Force -ErrorAction Stop
    $finalSnapshot = "$($finalApplicationItem.Length):$($finalApplicationItem.LastWriteTimeUtc.Ticks)"
    if (($finalApplicationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $finalSnapshot -ne $applicationSnapshot) {
        throw "Final $DesktopBinaryName changed during PE resource validation"
    }
}

function Assert-MzHeader {
    param([string]$Path, [string]$Label)
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        if ($stream.Length -lt 2 -or $stream.ReadByte() -ne 0x4D -or
            $stream.ReadByte() -ne 0x5A) {
            throw "$Label does not have an MZ header"
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Get-WindowsBundlePeTargets {
    param([string]$RootFull)
    return @(Get-WindowsTreeMembersWithoutReparseTraversal $RootFull | Where-Object {
        $isDirectory = ($_.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0
        $isReparsePoint = ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        $isPeCandidate = $_.Extension -ieq '.dll' -or $_.Extension -ieq '.drv' -or
            $_.Extension -ieq '.exe'
        -not $isDirectory -and -not $isReparsePoint -and $isPeCandidate
    })
}

# Reinspect the completed application tree without copying or repairing
# anything. This final gate runs after every writer, including the packaged
# runtime probe, and is also applied to the existing tree in installer-only
# mode, which must be treated as untrusted stale input.
function Assert-WindowsBundlePeImportPolicy {
    param(
        [string]$Root,
        [string]$Inspector
    )

    if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
        throw "Windows bundle was not found for final PE import validation: $Root"
    }
    $rootFull = (Get-Item -LiteralPath $Root -Force -ErrorAction Stop).FullName.TrimEnd(
        [char[]]@('\', '/')
    )
    Assert-WindowsBundleComponentPolicy $rootFull

    if ([string]::IsNullOrWhiteSpace($Inspector) -or
        -not [System.IO.Path]::IsPathRooted($Inspector)) {
        throw 'Final PE import inspector path must be absolute'
    }
    $inspectorFull = [System.IO.Path]::GetFullPath($Inspector)
    if (-not (Test-Path -LiteralPath $inspectorFull -PathType Leaf)) {
        throw "Required final PE import inspector was not found: $inspectorFull"
    }
    $inspectorItem = Get-Item -LiteralPath $inspectorFull -Force -ErrorAction Stop
    if (($inspectorItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Final PE import inspector must not be a filesystem reparse point: $inspectorFull"
    }

    $targetItems = @(Get-WindowsBundlePeTargets $rootFull | Sort-Object FullName)
    if ($targetItems.Count -eq 0) {
        throw 'Windows bundle contains no DLL, DRV, or EXE targets for final PE import validation'
    }
    $maxTargets = 4096
    if ($targetItems.Count -gt $maxTargets) {
        throw "Final PE import validation exceeded its $maxTargets-binary safety limit"
    }

    $targets = [System.Collections.Generic.List[string]]::new()
    $targetSnapshot = @{}
    foreach ($targetItem in $targetItems) {
        Assert-MzHeader $targetItem.FullName "Final PE import target $($targetItem.Name)"
        $fullPath = [System.IO.Path]::GetFullPath($targetItem.FullName)
        $targets.Add($fullPath)
        $targetSnapshot[$fullPath] = "$($targetItem.Length):$($targetItem.LastWriteTimeUtc.Ticks)"
    }

    $maxOutputLines = 131072
    $maxBatchTargets = 28
    $maxArgumentCharacters = 24000
    $maxBatchOutputBytes = 8388608
    $batchDeadlineMs = 45000
    $validationDeadlineMs = 300000
    $outputLineCount = 0
    $validationClock = [System.Diagnostics.Stopwatch]::StartNew()
    $offset = 0
    $batchNumber = 0

    while ($offset -lt $targets.Count) {
        if ($validationClock.ElapsedMilliseconds -ge $validationDeadlineMs) {
            throw "Final PE import validation exceeded its $validationDeadlineMs-millisecond deadline"
        }
        $batchNumber++
        $batchTargets = [System.Collections.Generic.List[string]]::new()
        $batchArgumentCharacters = '--coff-imports '.Length
        while ($offset -lt $targets.Count -and
            $batchTargets.Count -lt $maxBatchTargets) {
            $candidate = [string]$targets[$offset]
            $candidateCharacters = $candidate.Length + 3
            if (($batchArgumentCharacters + $candidateCharacters) -gt $maxArgumentCharacters) {
                if ($batchTargets.Count -eq 0) {
                    throw "Final PE import target exceeds the command-line safety limit: $([System.IO.Path]::GetFileName($candidate))"
                }
                break
            }
            $batchTargets.Add($candidate)
            $batchArgumentCharacters += $candidateCharacters
            $offset++
        }

        $lines = @(Invoke-BoundedPeImportBatch `
            -Inspector $inspectorFull `
            -TargetBatch $batchTargets `
            -ClosureClock $validationClock `
            -ClosureDeadlineMs $validationDeadlineMs `
            -ProcessDeadlineMs $batchDeadlineMs `
            -OutputByteLimit $maxBatchOutputBytes `
            -ArgumentCharacterLimit $maxArgumentCharacters)
        foreach ($line in $lines) {
            $outputLineCount++
            if ($outputLineCount -gt $maxOutputLines) {
                throw "Final PE import validation exceeded its $maxOutputLines-line safety limit"
            }
            if ([string]$line -notmatch '^\s*Name\s*:') { continue }

            $dllName = Get-PeImportDependencyName ([string]$line)
            if (-not $dllName) {
                throw "Final PE import inspector returned an unsupported dependency spelling in batch $batchNumber"
            }
            if (Test-ForbiddenBundledComponentName $dllName) {
                $dependencyLabel = Format-PeImportTargetForDiagnostic $dllName
                throw "Forbidden bundled dependency $dependencyLabel reported during final PE import validation"
            }
        }
    }

    # No writer is expected during this gate. Recheck the policy and the cheap
    # path/size/write-time snapshot so a late member cannot miss inspection.
    Assert-WindowsBundleComponentPolicy $rootFull
    $finalTargets = @(Get-WindowsBundlePeTargets $rootFull)
    if ($finalTargets.Count -ne $targetSnapshot.Count) {
        throw 'Windows bundle PE target set changed during final import validation'
    }
    foreach ($finalTarget in $finalTargets) {
        $finalPath = [System.IO.Path]::GetFullPath($finalTarget.FullName)
        $finalSignature = "$($finalTarget.Length):$($finalTarget.LastWriteTimeUtc.Ticks)"
        if (-not $targetSnapshot.ContainsKey($finalPath) -or
            $targetSnapshot[$finalPath] -ne $finalSignature) {
            throw "Windows bundle PE target changed during final import validation: $($finalTarget.Name)"
        }
    }
}

# ---------------------------------------------------------------------------
# Packaging: probe receipt
#
# The receipt binds an existing, already-probed tree to the exact application
# and the capability anchors of the closure, so installer-only mode can accept
# the tree only while none of them changed. It is written beside the tree and
# is never shipped.
# ---------------------------------------------------------------------------

$ProbeReceiptAnchors = @(
    'bin\balun.exe',
    'lib\gstreamer-1.0\libgstgtk4.dll',
    'lib\gstreamer-1.0\libgstwasapi2.dll',
    'lib\gstreamer-1.0\libgstlibav.dll'
)

function Get-WindowsProbeReceiptPath {
    param([Parameter(Mandatory = $true)][string]$Root)
    return "$Root$ProbeReceiptSuffix"
}

function Get-WindowsProbeSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'A Windows runtime-probe receipt input must be a regular file.'
    }
    return (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-WindowsProbeReceiptLines {
    param([Parameter(Mandatory = $true)][string]$Root)
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add($ProbeReceiptHeader)
    foreach ($anchor in $ProbeReceiptAnchors) {
        $path = Join-Path $Root $anchor
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Required receipt anchor is missing from the Windows bundle: $anchor"
        }
        $lines.Add("$anchor=$(Get-WindowsProbeSha256 $path)")
    }
    return @($lines)
}

function Write-WindowsProbeReceipt {
    param([Parameter(Mandatory = $true)][string]$Root)

    $receipt = Get-WindowsProbeReceiptPath $Root
    $temporary = "$receipt.$([Guid]::NewGuid().ToString('N')).tmp"
    $lines = @(Get-WindowsProbeReceiptLines $Root)
    try {
        [System.IO.File]::WriteAllLines(
            $temporary,
            $lines,
            [System.Text.UTF8Encoding]::new($false)
        )
        Move-Item -LiteralPath $temporary -Destination $receipt -Force
    }
    finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
}

function Assert-WindowsProbeReceipt {
    param([Parameter(Mandatory = $true)][string]$Root)

    $receipt = Get-WindowsProbeReceiptPath $Root
    if (-not (Test-Path -LiteralPath $receipt -PathType Leaf)) {
        throw 'The existing Windows bundle has no packaged-runtime probe receipt; run -Bundle or -Zip first.'
    }
    $receiptItem = Get-Item -LiteralPath $receipt -Force -ErrorAction Stop
    if (($receiptItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $receiptItem.Length -gt 1024) {
        throw 'The Windows packaged-runtime probe receipt is not a bounded regular file.'
    }

    $lines = @([System.IO.File]::ReadAllLines(
        $receiptItem.FullName,
        [System.Text.UTF8Encoding]::new($false, $true)
    ))
    $expected = @(Get-WindowsProbeReceiptLines $Root)
    if ($lines.Count -ne $expected.Count) {
        throw 'The Windows packaged-runtime probe receipt has an unsupported format.'
    }
    for ($index = 0; $index -lt $expected.Count; $index++) {
        if ([string]$lines[$index] -cne [string]$expected[$index]) {
            throw 'The Windows packaged-runtime probe receipt does not match the existing bundle; rerun -Bundle or -Zip.'
        }
    }
}

# ---------------------------------------------------------------------------
# Packaging: staging
# ---------------------------------------------------------------------------

# The capability-derived GStreamer plugin closure: the seven structural
# factories, the parsers and converters playbin3 autoplugs for MPEG-TS
# broadcasts, the decoders recorded in the Windows half of the P0.5 codec
# contract, and the Windows audio sinks. Nothing else from the MSYS2 plugin
# directory is staged, and a stale plugin outside this list is removed from
# an incremental tree. Every entry is required; a missing file fails the run.
$GStreamerPluginClosure = @(
    @{ Plugin = 'libgstcoreelements.dll'; Package = 'gstreamer'; Provides = 'typefind, multiqueue, queue, tee, capsfilter, identity, fakesink' },
    @{ Plugin = 'libgstplayback.dll'; Package = 'gst-plugins-base'; Provides = 'playbin3, uridecodebin3, decodebin3, parsebin, playsink, streamsynchronizer' },
    @{ Plugin = 'libgstapp.dll'; Package = 'gst-plugins-base'; Provides = 'appsrc' },
    @{ Plugin = 'libgsttypefindfunctions.dll'; Package = 'gst-plugins-base'; Provides = 'stream type detection' },
    @{ Plugin = 'libgstmpegtsdemux.dll'; Package = 'gst-plugins-bad'; Provides = 'tsdemux' },
    @{ Plugin = 'libgstdeinterlace.dll'; Package = 'gst-plugins-good'; Provides = 'deinterlace' },
    @{ Plugin = 'libgstgtk4.dll'; Package = 'gst-plugins-rs'; Provides = 'gtk4paintablesink' },
    @{ Plugin = 'libgstvideoparsersbad.dll'; Package = 'gst-plugins-bad'; Provides = 'mpegvideoparse, h264parse, h265parse' },
    @{ Plugin = 'libgstaudioparsers.dll'; Package = 'gst-plugins-good'; Provides = 'ac3parse, aacparse, mpegaudioparse' },
    @{ Plugin = 'libgstlibav.dll'; Package = 'gst-libav'; Provides = 'avdec_mpeg2video, avdec_h264, avdec_h265, avdec_mp2float, avdec_aac, avdec_ac3, avdec_eac3' },
    @{ Plugin = 'libgstd3d11.dll'; Package = 'gst-plugins-bad'; Provides = 'd3d11h264dec, d3d11h265dec' },
    @{ Plugin = 'libgstd3d12.dll'; Package = 'gst-plugins-bad'; Provides = 'd3d12h264dec, d3d12h265dec' },
    @{ Plugin = 'libgstmediafoundation.dll'; Package = 'gst-plugins-bad'; Provides = 'mfaacdec, mfmp3dec' },
    @{ Plugin = 'libgstopenh264.dll'; Package = 'gst-plugins-bad'; Provides = 'openh264dec' },
    @{ Plugin = 'libgstde265.dll'; Package = 'gst-plugins-bad'; Provides = 'libde265dec' },
    @{ Plugin = 'libgstmpg123.dll'; Package = 'gst-plugins-good'; Provides = 'mpg123audiodec' },
    @{ Plugin = 'libgstfaad.dll'; Package = 'gst-plugins-bad'; Provides = 'faad' },
    @{ Plugin = 'libgstfdkaac.dll'; Package = 'gst-plugins-bad'; Provides = 'fdkaacdec' },
    @{ Plugin = 'libgstvideoconvertscale.dll'; Package = 'gst-plugins-base'; Provides = 'videoconvert, videoscale' },
    @{ Plugin = 'libgstvideofilter.dll'; Package = 'gst-plugins-good'; Provides = 'videobalance' },
    @{ Plugin = 'libgstaudioconvert.dll'; Package = 'gst-plugins-base'; Provides = 'audioconvert' },
    @{ Plugin = 'libgstaudioresample.dll'; Package = 'gst-plugins-base'; Provides = 'audioresample' },
    @{ Plugin = 'libgstvolume.dll'; Package = 'gst-plugins-base'; Provides = 'volume' },
    @{ Plugin = 'libgstopengl.dll'; Package = 'gst-plugins-base'; Provides = 'glupload, glcolorconvert for gtk4paintablesink' },
    @{ Plugin = 'libgstautodetect.dll'; Package = 'gst-plugins-good'; Provides = 'autoaudiosink' },
    @{ Plugin = 'libgstwasapi2.dll'; Package = 'gst-plugins-bad'; Provides = 'wasapi2sink' },
    @{ Plugin = 'libgstwasapi.dll'; Package = 'gst-plugins-good'; Provides = 'wasapisink' }
)

function Get-PackagingTools {
    param([pscustomobject]$Layout)

    $tools = [ordered]@{
        PeInspector = Join-Path $Layout.Bin 'llvm-readobj.exe'
        PluginScanner = Join-Path $Layout.Prefix 'libexec\gstreamer-1.0\gst-plugin-scanner.exe'
        SchemaCompiler = Join-Path $Layout.Bin 'glib-compile-schemas.exe'
        IconCacheUpdater = Join-Path $Layout.Bin 'gtk4-update-icon-cache.exe'
    }
    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($name in $tools.Keys) {
        $path = [string]$tools[$name]
        if ($null -eq (Get-RegularFilePath @($path))) {
            $missing.Add("$name ($path)")
        }
    }
    if ($missing.Count -gt 0) {
        Exit-WithError (
            "Required packaging tools are missing from MSYS2 CLANG64: $($missing -join '; '). " +
            "Install the $MsysPackagePrefix-llvm, $MsysPackagePrefix-gstreamer, " +
            "$MsysPackagePrefix-glib2, and $MsysPackagePrefix-gtk4 packages."
        )
    }
    return [pscustomobject]$tools
}

function Get-CargoPackageVersion {
    param([string]$ManifestPath)
    if ($null -eq (Get-RegularFilePath @($ManifestPath))) {
        Exit-WithError "Cargo.toml is missing or is not a regular file: $ManifestPath"
    }
    $inPackage = $false
    foreach ($line in [System.IO.File]::ReadAllLines($ManifestPath)) {
        if ($line -match '^\s*\[(.+)\]\s*$') {
            $inPackage = $matches[1].Trim() -ceq 'package'
            continue
        }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $matches[1]
        }
    }
    Exit-WithError 'Cargo.toml does not declare a [package] version.'
}

function ConvertTo-NumericVersion {
    param([string]$Version)
    if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)') {
        Exit-WithError "Package version '$Version' is not a semantic version."
    }
    return "$($matches[1]).$($matches[2]).$($matches[3]).0"
}

# Reject source aliases before Copy-Item can follow them and turn a forbidden
# target into a safe-named regular destination. Keep the same guard on every
# incremental and unconditional bundle copy.
function Get-ValidatedWindowsBundleCopySourceItem {
    param([string]$Src)
    $sourceItem = Get-Item -LiteralPath $Src -Force -ErrorAction Stop
    if (($sourceItem.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0) {
        throw "Refusing to copy a directory as a Windows bundle file: $($sourceItem.FullName)"
    }
    if (($sourceItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to copy filesystem reparse point into the Windows bundle: $($sourceItem.FullName)"
    }
    if (Test-ForbiddenBundledComponentName $sourceItem.Name) {
        throw "Refusing to copy forbidden bundled component: $($sourceItem.Name)"
    }
    return $sourceItem
}

function Get-ValidatedWindowsBundleCopyDestinationItem {
    param([string]$Dst)
    if (Test-ForbiddenBundledComponentName ([System.IO.Path]::GetFileName($Dst))) {
        throw "Refusing to write forbidden Windows bundle destination: $Dst"
    }

    $destinationItem = Get-Item -LiteralPath $Dst -Force -ErrorAction SilentlyContinue
    if ($null -eq $destinationItem) { return $null }
    if (($destinationItem.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0) {
        throw "Refusing to overwrite a directory as a Windows bundle file: $($destinationItem.FullName)"
    }
    if (($destinationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to overwrite filesystem reparse point in the Windows bundle: $($destinationItem.FullName)"
    }
    return $destinationItem
}

function Copy-WindowsBundleFileForced {
    param([string]$Src, [string]$Dst)
    $sourceItem = Get-ValidatedWindowsBundleCopySourceItem $Src
    $null = Get-ValidatedWindowsBundleCopyDestinationItem $Dst
    $destinationDirectory = Split-Path -Parent $Dst
    if (-not (Test-Path -LiteralPath $destinationDirectory -PathType Container)) {
        New-Item -ItemType Directory -Force $destinationDirectory | Out-Null
    }
    Copy-Item -LiteralPath $sourceItem.FullName -Destination $Dst -Force
}

# Copy a single file only if the destination doesn't exist or the source is
# newer, so incremental rebuilds do not re-copy hundreds of unchanged DLLs.
function Copy-IfNewer {
    param([string]$Src, [string]$Dst)
    $sourceItem = Get-ValidatedWindowsBundleCopySourceItem $Src
    $destinationItem = Get-ValidatedWindowsBundleCopyDestinationItem $Dst
    if ($null -eq $destinationItem) {
        Copy-Item -LiteralPath $sourceItem.FullName -Destination $Dst
        return $true
    }
    if ($sourceItem.LastWriteTimeUtc -gt $destinationItem.LastWriteTimeUtc -or
        $sourceItem.Length -ne $destinationItem.Length) {
        Copy-Item -LiteralPath $sourceItem.FullName -Destination $Dst -Force
        return $true
    }
    return $false
}

function Sync-Directory {
    param(
        [string]$SrcDir,
        [string]$DstDir,
        [switch]$SkipForbiddenComponents
    )
    $copied = 0
    $sourceRoot = (Get-Item -LiteralPath $SrcDir -Force -ErrorAction Stop).FullName.TrimEnd(
        [char[]]@('\', '/')
    )
    if ($SkipForbiddenComponents -and
        (Test-Path -LiteralPath $DstDir -PathType Container)) {
        $null = Remove-ForbiddenWindowsBundleMembers $DstDir
    }
    if (Test-Path -LiteralPath $DstDir -PathType Container) {
        $destinationReparsePoints = @(Get-WindowsBundleReparsePointMembers $DstDir)
        if ($destinationReparsePoints.Count -gt 0) {
            throw "Refusing to sync into a Windows destination tree containing a filesystem reparse point: $($destinationReparsePoints[0].FullName)"
        }
    }
    foreach ($sourceMember in @(Get-WindowsTreeMembersWithoutReparseTraversal $sourceRoot)) {
        $relPath = $sourceMember.FullName.Substring($sourceRoot.Length).TrimStart(
            [char[]]@('\', '/')
        )
        $isDirectory = ($sourceMember.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0
        $isReparsePoint = ($sourceMember.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        if ($isReparsePoint) {
            throw "Refusing to sync filesystem reparse point into the Windows bundle: $relPath"
        }
        if ($isDirectory) { continue }

        $destFile = Join-Path $DstDir $relPath
        if (Test-ForbiddenBundledRelativePath $relPath) {
            if (-not $SkipForbiddenComponents) {
                throw "Refusing to sync forbidden bundled relative path: $relPath"
            }
            continue
        }
        $destDir = Split-Path $destFile
        if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Force $destDir | Out-Null }
        if (Copy-IfNewer $sourceMember.FullName $destFile) { $copied++ }
    }
    return $copied
}

function Add-DllScanTarget {
    param(
        [System.Collections.Queue]$Queue,
        [hashtable]$Known,
        [string]$Path,
        [int]$Limit
    )
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    if ($Known.ContainsKey($fullPath)) { return }
    if ($Known.Count -ge $Limit) {
        Exit-WithError "DLL dependency closure exceeded its $Limit-binary safety limit."
    }
    $Known[$fullPath] = $true
    $Queue.Enqueue($fullPath)
}

function Add-PeImportDependencies {
    param(
        [string[]]$Lines,
        [string]$SourceLabel,
        [string]$ArchitectureBin,
        [string]$DestinationBin,
        [System.Collections.Queue]$Queue,
        [hashtable]$Known,
        [int]$TargetLimit,
        [ref]$OutputLineCount,
        [int]$OutputLineLimit,
        [ref]$CopiedCount,
        [System.Diagnostics.Stopwatch]$ClosureClock,
        [int]$ClosureDeadlineMs
    )
    foreach ($line in $Lines) {
        if ($ClosureClock.ElapsedMilliseconds -ge $ClosureDeadlineMs) {
            throw "PE import dependency closure exceeded its $ClosureDeadlineMs-millisecond deadline"
        }
        $OutputLineCount.Value = [int]$OutputLineCount.Value + 1
        if ($OutputLineCount.Value -gt $OutputLineLimit) {
            throw "PE import dependency closure exceeded its $OutputLineLimit-line safety limit"
        }

        if ([string]$line -notmatch '^\s*Name\s*:') { continue }
        $dllName = Get-PeImportDependencyName ([string]$line)
        if (-not $dllName) {
            throw "PE import inspector returned an unsupported dependency spelling for $SourceLabel"
        }

        # Never satisfy an import with a prohibited copy-control/DRM runtime.
        # Failing here, rather than silently omitting the DLL, preserves the
        # strict dependency-closure contract and identifies the importer that
        # must be excluded from the closure.
        if (Test-ForbiddenBundledComponentName $dllName) {
            throw "Forbidden bundled dependency $dllName reported for $SourceLabel"
        }

        # Copy only exact imports that exist in the MSYS2 bin directory.
        # Imports provided by Windows itself are intentionally not bundled;
        # API-set contract names need not have a physical file.
        $srcPath = Join-Path $ArchitectureBin $dllName
        if (Test-Path -LiteralPath $srcPath -PathType Leaf) {
            $destPath = Join-Path $DestinationBin $dllName
            if (Copy-IfNewer $srcPath $destPath) {
                Write-Host "  copied: $dllName"
                $CopiedCount.Value = [int]$CopiedCount.Value + 1
            }
            Add-DllScanTarget $Queue $Known $destPath $TargetLimit
            continue
        }

        $systemPath = Join-Path ([System.Environment]::SystemDirectory) $dllName
        $isApiSet = $dllName -match '^(?i:api-ms-win-|ext-ms-win-)'
        if ($isApiSet -or (Test-Path -LiteralPath $systemPath -PathType Leaf)) { continue }
        throw "Unresolved DLL import $dllName reported for $SourceLabel"
    }
}

function Invoke-WindowsPackageStaging {
    param(
        [pscustomobject]$Layout,
        [pscustomobject]$Tools,
        [string]$Distribution,
        [string]$Application
    )

    New-Item -ItemType Directory -Force $Distribution | Out-Null
    $dist = (Resolve-Path -LiteralPath $Distribution).ProviderPath.TrimEnd([char[]]@('\', '/'))
    Assert-WindowsBundleRootIsNotReparsePoint $dist
    $binDir = Join-Path $dist 'bin'
    $pluginDir = Join-Path $dist 'lib\gstreamer-1.0'
    $scannerDir = Join-Path $dist 'libexec\gstreamer-1.0'
    foreach ($directory in @($binDir, $pluginDir, $scannerDir)) {
        New-Item -ItemType Directory -Force $directory | Out-Null
    }

    # Remove policy members left by an older incremental bundle before copying
    # or inspecting anything. The same policy is asserted again after all copy
    # paths, so this cleanup cannot hide a newly introduced forbidden dependency.
    $staleForbiddenMemberCount = Remove-ForbiddenWindowsBundleMembers $dist
    if ($staleForbiddenMemberCount -gt 0) {
        Write-Info "Removed $staleForbiddenMemberCount forbidden component(s) from the incremental Windows bundle."
    }
    Assert-WindowsBundleComponentPolicy $dist

    # Always copy the executable (just built or explicitly reused).
    $applicationDestination = Join-Path $binDir $DesktopBinaryName
    Copy-WindowsBundleFileForced $Application $applicationDestination

    # Stage exactly the reviewed plugin closure and prune anything else an
    # older incremental tree left in the plugin directory.
    Write-Info 'Staging the capability-derived GStreamer plugin closure...'
    $totalCopied = 0
    $allowedPlugins = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $missingPlugins = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $GStreamerPluginClosure) {
        $null = $allowedPlugins.Add($entry.Plugin)
        $source = Join-Path $Layout.PluginDirectory $entry.Plugin
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
            $missingPlugins.Add("$($entry.Plugin) ($($entry.Provides)) from $MsysPackagePrefix-$($entry.Package)")
            continue
        }
        if (Copy-IfNewer $source (Join-Path $pluginDir $entry.Plugin)) { $totalCopied++ }
    }
    if ($missingPlugins.Count -gt 0) {
        Exit-WithError (
            "The reviewed GStreamer plugin closure is incomplete in $($Layout.PluginDirectory): " +
            ($missingPlugins -join '; ') +
            '. Install the matching packages in MSYS2 CLANG64 and retry.'
        )
    }
    foreach ($member in @(Get-WindowsTreeMembersWithoutReparseTraversal $pluginDir)) {
        if (($member.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Exit-WithError "Refusing to prune a filesystem reparse point in the plugin directory: $($member.FullName)"
        }
        if (($member.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0) {
            Exit-WithError "The staged plugin directory must contain only plugin files: $($member.FullName)"
        }
        if (-not $allowedPlugins.Contains($member.Name)) {
            Write-Info "Pruned stale plugin outside the reviewed closure: $($member.Name)"
            $member.Delete()
        }
    }

    # gst-plugin-scanner is a required part of the packaged GStreamer runtime.
    # Always overwrite it: source timestamps cannot prove binary identity.
    # GStreamer locates it at <prefix>\libexec\gstreamer-1.0 from its own DLL
    # in <prefix>\bin and prepends that DLL directory to the scanner's PATH.
    Copy-WindowsBundleFileForced $Tools.PluginScanner (Join-Path $scannerDir 'gst-plugin-scanner.exe')
    Write-Info 'Bundled gst-plugin-scanner.exe (unconditional overwrite).'

    $loadersSrc = Join-Path $Layout.Prefix 'lib\gdk-pixbuf-2.0'
    if (Test-Path -LiteralPath $loadersSrc -PathType Container) {
        $totalCopied += Sync-Directory $loadersSrc (Join-Path $dist 'lib\gdk-pixbuf-2.0') -SkipForbiddenComponents
    }

    # Resolve every transitive dependency for the executable, the plugins, the
    # scanner, and the pixbuf loaders without loading them. MSYS2's ldd executes
    # each target under its loader and can hang; llvm-readobj inspects the PE
    # import table as data. Every MSYS2 runtime DLL discovered is copied beside
    # the application and enqueued in turn, so transitive dependencies reach
    # closure. DLLs left in bin by an older run that this closure never reached
    # are pruned afterwards.
    Write-Info 'Resolving the DLL closure for the application, plugins, and scanner...'
    $maxDllScanTargets = 4096
    $maxPeInspectorOutputLines = 131072
    $maxPeInspectorBatchTargets = 28
    $maxPeInspectorArgumentCharacters = 24000
    $maxPeInspectorBatchOutputBytes = 8388608
    $peInspectorBatchDeadlineMs = 45000
    $peInspectorClosureDeadlineMs = 300000
    $dllScanQueue = [System.Collections.Queue]::new()
    $knownDllScanTargets = @{}
    $scannedDllTargets = @{}
    foreach ($seed in @(Get-WindowsBundlePeTargets $dist | Select-Object -ExpandProperty FullName)) {
        Add-DllScanTarget $dllScanQueue $knownDllScanTargets $seed $maxDllScanTargets
    }
    $peInspectorOutputLineCount = 0
    $peInspectorClosureClock = [System.Diagnostics.Stopwatch]::StartNew()
    $peInspectorRound = 0
    while ($dllScanQueue.Count -gt 0) {
        if ($peInspectorClosureClock.ElapsedMilliseconds -ge $peInspectorClosureDeadlineMs) {
            Exit-WithError "PE import dependency closure exceeded its $peInspectorClosureDeadlineMs-millisecond deadline."
        }
        $peInspectorRound++
        $roundTargets = [System.Collections.Generic.List[string]]::new()
        while ($dllScanQueue.Count -gt 0) {
            $bin = [string]$dllScanQueue.Dequeue()
            if ($scannedDllTargets.ContainsKey($bin)) { continue }
            $scannedDllTargets[$bin] = $true
            $roundTargets.Add($bin)
        }
        if ($roundTargets.Count -eq 0) { continue }
        Write-Host "  import round ${peInspectorRound}: $($roundTargets.Count) binary target(s)"

        $offset = 0
        $batchNumber = 0
        while ($offset -lt $roundTargets.Count) {
            $batchNumber++
            $batchTargets = [System.Collections.Generic.List[string]]::new()
            $batchArgumentCharacters = '--coff-imports '.Length
            while ($offset -lt $roundTargets.Count -and
                $batchTargets.Count -lt $maxPeInspectorBatchTargets) {
                $candidate = [string]$roundTargets[$offset]
                $candidateCharacters = $candidate.Length + 3
                if (($batchArgumentCharacters + $candidateCharacters) -gt $maxPeInspectorArgumentCharacters) {
                    if ($batchTargets.Count -eq 0) {
                        Exit-WithError "PE import-inspection target exceeds the command-line safety limit: $([System.IO.Path]::GetFileName($candidate))"
                    }
                    break
                }
                $batchTargets.Add($candidate)
                $batchArgumentCharacters += $candidateCharacters
                $offset++
            }

            try {
                $batchLines = @(Invoke-BoundedPeImportBatch `
                    -Inspector $Tools.PeInspector `
                    -TargetBatch $batchTargets `
                    -ClosureClock $peInspectorClosureClock `
                    -ClosureDeadlineMs $peInspectorClosureDeadlineMs `
                    -ProcessDeadlineMs $peInspectorBatchDeadlineMs `
                    -OutputByteLimit $maxPeInspectorBatchOutputBytes `
                    -ArgumentCharacterLimit $maxPeInspectorArgumentCharacters)
                $batchNames = @($batchTargets | Select-Object -First 3 | ForEach-Object {
                    [System.IO.Path]::GetFileName($_)
                }) -join ', '
                if ($batchTargets.Count -gt 3) { $batchNames += ', ...' }
                Add-PeImportDependencies $batchLines $batchNames `
                    $Layout.Bin $binDir $dllScanQueue $knownDllScanTargets `
                    $maxDllScanTargets ([ref]$peInspectorOutputLineCount) `
                    $maxPeInspectorOutputLines ([ref]$totalCopied) $peInspectorClosureClock `
                    $peInspectorClosureDeadlineMs
            }
            catch {
                Exit-WithError "DLL import inspection failed in round $peInspectorRound, batch ${batchNumber}: $($_.Exception.Message)"
            }
        }
    }
    Write-Host "  dependency closure complete: $($scannedDllTargets.Count) binary target(s) in $peInspectorRound round(s)"

    foreach ($member in @(Get-WindowsTreeMembersWithoutReparseTraversal $binDir)) {
        if (($member.Attributes -band [System.IO.FileAttributes]::Directory) -ne 0 -or
            ($member.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            Exit-WithError "The staged bin directory must contain only regular files: $($member.FullName)"
        }
        if (-not $scannedDllTargets.ContainsKey([System.IO.Path]::GetFullPath($member.FullName))) {
            Write-Info "Pruned stale file outside the dependency closure: bin\$($member.Name)"
            $member.Delete()
        }
    }

    # GTK resources: icon themes, the application's own icons, and compiled
    # settings schemas, all located by GLib from its DLL prefix.
    Write-Info 'Syncing GTK icons and schemas (incremental)...'
    foreach ($theme in @('hicolor', 'Adwaita')) {
        $src = Join-Path $Layout.Prefix "share\icons\$theme"
        if (Test-Path -LiteralPath $src -PathType Container) {
            $totalCopied += Sync-Directory $src (Join-Path $dist "share\icons\$theme") -SkipForbiddenComponents
        }
    }
    $appIconsSrc = Join-Path $RepositoryRoot 'data\icons\hicolor'
    if (-not (Test-Path -LiteralPath $appIconsSrc -PathType Container)) {
        Exit-WithError "The application icon set is missing: $appIconsSrc"
    }
    $appIconsDest = Join-Path $dist 'share\icons\hicolor'
    $totalCopied += Sync-Directory $appIconsSrc $appIconsDest -SkipForbiddenComponents
    # Rebuild icon-theme.cache so it indexes the application icon; the cache
    # copied from MSYS2 indexes only system icons.
    $global:LASTEXITCODE = 0
    & $Tools.IconCacheUpdater '-f' '-t' $appIconsDest 2>$null
    if ($LASTEXITCODE -ne 0) {
        Exit-WithError "gtk4-update-icon-cache failed with exit code $LASTEXITCODE."
    }

    $schemasSrc = Join-Path $Layout.Prefix 'share\glib-2.0\schemas'
    $schemasDest = Join-Path $dist 'share\glib-2.0\schemas'
    if (-not (Test-Path -LiteralPath $schemasSrc -PathType Container)) {
        Exit-WithError "The GLib schema directory is missing: $schemasSrc"
    }
    New-Item -ItemType Directory -Force $schemasDest | Out-Null
    foreach ($schema in @(Get-ChildItem -LiteralPath $schemasSrc -File -Force | Where-Object {
        $_.Extension -ieq '.xml' -or $_.Extension -ieq '.override'
    })) {
        if (Copy-IfNewer $schema.FullName (Join-Path $schemasDest $schema.Name)) { $totalCopied++ }
    }
    $global:LASTEXITCODE = 0
    & $Tools.SchemaCompiler $schemasDest
    if ($LASTEXITCODE -ne 0) {
        Exit-WithError "glib-compile-schemas failed with exit code $LASTEXITCODE."
    }

    Write-Info "Incremental sync: $totalCopied file(s) updated."
    Assert-WindowsBundleComponentPolicy $dist
    return $dist
}

# ---------------------------------------------------------------------------
# Packaging: packaged runtime probe
#
# Run the staged executable itself before archiving it. The child receives a
# fresh registry through GST_REGISTRY and nothing else that could redirect
# GStreamer, GIO, or the stream transport; PATH holds only System32, so the
# application and the scanner resolve every DLL from the package. The Rust
# probe rejects any other inherited policy variable, proves the bundled
# scanner starts, plays the synthetic fixture through the production source
# policy and transport, and writes its sentinel only after every check passed.
# ---------------------------------------------------------------------------
function Invoke-PackagedRuntimeProbe {
    param([string]$Distribution)

    Write-Info 'Running the packaged Windows runtime probe...'
    $probeExe = Join-Path $Distribution "bin\$DesktopBinaryName"
    $probeWorkspace = Join-Path ([System.IO.Path]::GetTempPath()) ("Balun Windows Runtime Probe With Spaces " + [Guid]::NewGuid().ToString('N'))
    $probeCache = Join-Path $probeWorkspace 'Fresh Cache With Spaces'
    $probeRegistry = Join-Path $probeCache 'gstreamer\registry.bin'
    $probeStdout = Join-Path $probeWorkspace 'stdout.log'
    $probeStderr = Join-Path $probeWorkspace 'stderr.log'
    $probeSentinel = Join-Path $probeCache $PlatformProbeSentinelName
    $expectedSentinel = [System.Text.Encoding]::UTF8.GetBytes($PlatformProbeSentinel)
    $probeProcess = $null
    $stdoutStream = $null
    $stderrStream = $null
    $stdoutCopy = $null
    $stderrCopy = $null
    $probeFailure = $null

    try {
        New-Item -ItemType Directory -Force $probeCache | Out-Null
        try {
            $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
            $startInfo.FileName = $probeExe
            $startInfo.WorkingDirectory = Split-Path -Parent $probeExe
            $startInfo.UseShellExecute = $false
            $startInfo.CreateNoWindow = $true
            $startInfo.RedirectStandardOutput = $true
            $startInfo.RedirectStandardError = $true
            $startInfo.ArgumentList.Add($PlatformProbeFlag)
            $startInfo.ArgumentList.Add($probeCache)

            # ProcessStartInfo begins with a copy of this process's environment.
            # Remove every policy input the Rust probe refuses to inherit, plus
            # all conventional proxy variables, using case-insensitive
            # comparisons; then set exactly the sanitized PATH and registry.
            foreach ($key in @($startInfo.EnvironmentVariables.Keys)) {
                $normalized = $key.ToUpperInvariant()
                if ($normalized.StartsWith('GST_') -or
                    $normalized -eq 'GIO_EXTRA_MODULES' -or
                    $normalized -eq 'GIO_USE_PROXY_RESOLVER' -or
                    $normalized -match '^(HTTP|HTTPS|ALL|NO)_PROXY$' -or
                    $normalized -eq 'PATH') {
                    $null = $startInfo.EnvironmentVariables.Remove($key)
                }
            }
            $startInfo.EnvironmentVariables['PATH'] = [System.Environment]::SystemDirectory
            $startInfo.EnvironmentVariables['GST_REGISTRY'] = $probeRegistry

            $probeProcess = [System.Diagnostics.Process]::new()
            $probeProcess.StartInfo = $startInfo
            $probeClock = [System.Diagnostics.Stopwatch]::StartNew()
            if (-not $probeProcess.Start()) { throw 'could not start the staged executable' }

            $stdoutStream = [System.IO.File]::Create($probeStdout)
            $stderrStream = [System.IO.File]::Create($probeStderr)
            $stdoutCopy = $probeProcess.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
            $stderrCopy = $probeProcess.StandardError.BaseStream.CopyToAsync($stderrStream)

            while (-not $probeProcess.WaitForExit(50)) {
                if ($probeClock.ElapsedMilliseconds -ge $PlatformProbeDeadlineMs) {
                    throw "packaged runtime probe exceeded its $PlatformProbeDeadlineMs-millisecond deadline"
                }
                $stdoutLength = $stdoutStream.Length
                $stderrLength = $stderrStream.Length
                if ($stdoutLength -gt $PlatformProbeOutputLimit -or
                    $stderrLength -gt $PlatformProbeOutputLimit -or
                    ($stdoutLength + $stderrLength) -gt $PlatformProbeOutputLimit) {
                    throw 'packaged runtime probe output crossed its 1 MiB flood threshold'
                }
            }
            if ($probeClock.ElapsedMilliseconds -ge $PlatformProbeDeadlineMs) {
                throw "packaged runtime probe exceeded its $PlatformProbeDeadlineMs-millisecond deadline"
            }
            $probeProcess.WaitForExit()
            if ($probeProcess.ExitCode -ne 0) {
                throw "staged executable exited with status $($probeProcess.ExitCode)"
            }
        }
        catch {
            $probeFailure = $_.Exception.Message
        }
        finally {
            try {
                Stop-BoundedProcessTree $probeProcess 'packaged runtime probe'
            }
            catch {
                if ($probeFailure) { $probeFailure += "; $($_.Exception.Message)" }
                else { $probeFailure = $_.Exception.Message }
            }
            finally {
                try {
                    $copyTasks = @($stdoutCopy, $stderrCopy) | Where-Object { $null -ne $_ }
                    if ($copyTasks.Count -gt 0) {
                        if (-not [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]$copyTasks, 10000)) {
                            throw 'redirected output exceeded its 10-second drain deadline'
                        }
                    }
                }
                catch {
                    if ($probeFailure) { $probeFailure += "; redirected output did not drain: $($_.Exception.Message)" }
                    else { $probeFailure = "redirected output did not drain: $($_.Exception.Message)" }
                }
                finally {
                    if ($null -ne $stdoutStream) { $stdoutStream.Dispose() }
                    if ($null -ne $stderrStream) { $stderrStream.Dispose() }
                    if ($null -ne $probeProcess) { $probeProcess.Dispose() }
                }
            }
        }

        if ((Test-Path -LiteralPath $probeStdout -PathType Leaf) -and
            (Test-Path -LiteralPath $probeStderr -PathType Leaf)) {
            $stdoutLength = (Get-Item -LiteralPath $probeStdout).Length
            $stderrLength = (Get-Item -LiteralPath $probeStderr).Length
            if ($stdoutLength -gt $PlatformProbeOutputLimit -or
                $stderrLength -gt $PlatformProbeOutputLimit -or
                ($stdoutLength + $stderrLength) -gt $PlatformProbeOutputLimit) {
                if ($probeFailure) { $probeFailure += '; packaged runtime probe output crossed its 1 MiB flood threshold' }
                else { $probeFailure = 'packaged runtime probe output crossed its 1 MiB flood threshold' }
            }
        }

        if (-not $probeFailure) {
            if (-not (Test-Path -LiteralPath $probeSentinel -PathType Leaf)) {
                $probeFailure = 'staged executable did not write the runtime-probe sentinel'
            }
            else {
                $actualSentinel = [System.IO.File]::ReadAllBytes($probeSentinel)
                if ([Convert]::ToBase64String($actualSentinel) -ne [Convert]::ToBase64String($expectedSentinel)) {
                    $probeFailure = 'runtime-probe sentinel content was not exact'
                }
            }
        }
        if (-not $probeFailure) {
            $registryItem = Get-Item -LiteralPath $probeRegistry -Force -ErrorAction SilentlyContinue
            if ($null -eq $registryItem -or $registryItem.Length -le 0) {
                $probeFailure = 'staged executable did not build a fresh GStreamer registry'
            }
        }

        if ($probeFailure) {
            $stdoutDiagnostic = Get-BoundedProbeDiagnostic $probeStdout 'stdout'
            $stderrDiagnostic = Get-BoundedProbeDiagnostic $probeStderr 'stderr'
            $probeFailure += "`n--- bounded stdout ---`n$stdoutDiagnostic`n--- bounded stderr ---`n$stderrDiagnostic"
        }
    }
    finally {
        # Exception-safe cleanup includes the fresh cache, exact sentinel, and
        # bounded diagnostic files; no probe state is shipped in the archive.
        Remove-Item -LiteralPath $probeWorkspace -Recurse -Force -ErrorAction SilentlyContinue
    }

    if ($probeFailure) { Exit-WithError "Packaged Windows runtime probe failed: $probeFailure" }
    Write-Info 'Packaged Windows runtime probe passed.'
}

function Assert-WindowsPackageFinalGates {
    param(
        [string]$Distribution,
        [string]$Inspector,
        [string]$ExpectedVersion
    )
    Assert-WindowsBundleComponentPolicy $Distribution
    try {
        Assert-WindowsBundlePeImportPolicy $Distribution $Inspector
    }
    catch {
        Exit-WithError "Final Windows PE import validation failed: $($_.Exception.Message)"
    }
    try {
        Assert-WindowsApplicationResourceContract `
            -Application (Join-Path $Distribution "bin\$DesktopBinaryName") `
            -Inspector $Inspector `
            -ExpectedVersion $ExpectedVersion
    }
    catch {
        Exit-WithError "Final Windows application resource validation failed: $($_.Exception.Message)"
    }
    Write-Info 'Final Windows PE import and application resource gates passed.'
}

function New-WindowsZip {
    param([string]$Distribution)

    Write-Info 'Creating the ZIP archive...'
    $zipPath = Join-Path (Split-Path -Parent $Distribution) $ZipFileName
    Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
    Compress-Archive -LiteralPath $Distribution -DestinationPath $zipPath
    Assert-WindowsZipMatchesTree $zipPath $Distribution
    Write-Info "Archive created and reopened: $zipPath"
    return $zipPath
}

function Find-InnoSetupCompiler {
    # Tributary's machine-wide locations plus the per-user location that the
    # Inno Setup installer and winget use when installing for one user.
    foreach ($candidate in @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe",
        'C:\Program Files (x86)\Inno Setup 6\ISCC.exe',
        'C:\Program Files\Inno Setup 6\ISCC.exe'
    )) {
        if ([string]::IsNullOrWhiteSpace($candidate)) { continue }
        if ($null -ne (Get-RegularFilePath @($candidate))) { return $candidate }
    }
    $command = Get-Command 'iscc.exe' -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $command) { return $command.Source }
    Exit-WithError (
        'Inno Setup compiler (iscc.exe) not found. Install Inno Setup 6 from ' +
        'https://jrsoftware.org/isinfo.php; this helper never installs it.'
    )
}

function Invoke-InnoSetup {
    param(
        [string]$Distribution,
        [string]$Version,
        [string]$NumericVersion
    )

    $iscc = Find-InnoSetupCompiler
    $issFile = Join-Path $RepositoryRoot $InnoScriptRelativePath
    if (-not (Test-Path -LiteralPath $issFile -PathType Leaf)) {
        Exit-WithError "The Inno Setup recipe is missing: $issFile"
    }
    $outputDir = Split-Path -Parent $Distribution
    $installerPath = Join-Path $outputDir "$InstallerBaseName.exe"
    Remove-Item -LiteralPath $installerPath -Force -ErrorAction SilentlyContinue

    Write-Info 'Running the Inno Setup compiler...'
    $global:LASTEXITCODE = 0
    & $iscc "/DAppVersion=$Version" "/DAppNumericVersion=$NumericVersion" `
        "/DSourceDir=$Distribution" "/DOutputDir=$outputDir" `
        "/DTargetArch=$InnoTargetArchitecture" $issFile
    if ($LASTEXITCODE -ne 0) { Exit-WithError 'Inno Setup compilation failed.' }

    # Reopen the installer's version resource: the payload is the tree that
    # was validated moments ago, and this proves the artifact carries Balun's
    # identity and the exact package version.
    $installerItem = Get-ValidatedBuildOutput $installerPath 'installer'
    Assert-MzHeader $installerItem.FullName 'Completed installer'
    # Inno Setup pads its version-resource strings with trailing blanks, so
    # compare the trimmed values against the exact expected identity.
    $versionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($installerItem.FullName)
    $padding = [char[]]@(' ', [char]0)
    if (([string]$versionInfo.ProductName).Trim($padding) -cne $ProductName) {
        Exit-WithError "Completed installer ProductName metadata must be '$ProductName'."
    }
    if (([string]$versionInfo.ProductVersion).Trim($padding) -cne $Version) {
        Exit-WithError "Completed installer ProductVersion metadata does not match package version $Version."
    }
    if (([string]$versionInfo.FileVersion).Trim($padding) -cne $Version) {
        Exit-WithError "Completed installer FileVersion metadata does not match package version $Version."
    }
    Write-Info "Installer created and reopened: $($installerItem.FullName)"
}

$RepositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$CargoTargetRoot = Join-Path $RepositoryRoot 'target'
$TargetDirectoryArguments = @('--target-dir', $CargoTargetRoot)
$CargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $CargoCommand) {
    Exit-WithError (
        'cargo is unavailable; install Rust from https://rustup.rs (or the ' +
        'Rustlang.Rustup winget package), select it explicitly, then retry.'
    )
}

Push-Location -LiteralPath $RepositoryRoot
try {
    if ($Fmt) {
        Write-Info 'Formatting Balun...'
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments @('fmt', '--all') -Description 'cargo fmt'
        Write-Info 'Formatting complete.'
        exit 0
    }

    $RustTarget = if ($DiagnosticMode) {
        Resolve-DiagnosticRustTarget
    }
    else {
        Resolve-DesktopRustTarget
    }
    $TargetArguments = @('--target', $RustTarget)

    if (-not $DiagnosticMode) {
        Assert-DesktopRustTargetInstalled $RustTarget
        $MsysLayout = Resolve-Msys2Layout
        Initialize-DesktopBuildEnvironment $MsysLayout
        Write-Info "Using MSYS2 CLANG64 at $($MsysLayout.Prefix)."
        Assert-PkgConfigFloor $MsysLayout 'gtk4' '4.16' 'gtk4'
        Assert-PkgConfigFloor $MsysLayout 'libadwaita-1' '1.6' 'libadwaita'
        Assert-PkgConfigFloor $MsysLayout 'gstreamer-1.0' '1.20' 'gstreamer'
        Write-Info (
            'GTK 4.16, libadwaita 1.6, and GStreamer 1.20 ' +
            'development-library checks passed.'
        )
    }

    $FeatureArguments = if ($DiagnosticMode) {
        @()
    }
    else {
        @('--all-features')
    }

    if ($Check) {
        $ModeName = if ($DiagnosticMode) { 'diagnostic' } else { 'desktop' }
        Write-Info "Checking all Balun $ModeName targets with locked dependencies..."
        $CargoArguments = @('check', '--all-targets') +
            $FeatureArguments + @('--locked') + $TargetDirectoryArguments +
            $TargetArguments
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo check'
        Write-Info 'Check passed.'
        exit 0
    }

    if ($Clippy) {
        $ModeName = if ($DiagnosticMode) { 'diagnostic' } else { 'desktop' }
        Write-Info "Linting all Balun $ModeName targets with locked dependencies..."
        $CargoArguments = @('clippy', '--all-targets') +
            $FeatureArguments + @('--locked') + $TargetDirectoryArguments +
            $TargetArguments + @('--', '-D', 'warnings')
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo clippy'
        # Tributary lints both profiles so cfg(debug_assertions)-gated code
        # cannot hide from either configuration.
        Write-Info "Linting all Balun $ModeName targets in the release profile..."
        $CargoArguments = @('clippy', '--release', '--all-targets') +
            $FeatureArguments + @('--locked') + $TargetDirectoryArguments +
            $TargetArguments + @('--', '-D', 'warnings')
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo clippy'
        Write-Info 'Clippy passed.'
        exit 0
    }

    if ($Test) {
        $ModeName = if ($DiagnosticMode) { 'diagnostic' } else { 'desktop' }
        Write-Info "Testing all Balun $ModeName targets with locked dependencies..."
        $CargoArguments = @('test', '--all-targets') +
            $FeatureArguments + @('--locked') + $TargetDirectoryArguments +
            $TargetArguments
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo test'
        Write-Info 'Tests passed.'
        exit 0
    }

    if ($Coverage) {
        $CoverageArtifactRoot = Join-Path $CargoTargetRoot 'llvm-cov-target'
        $env:CARGO_TARGET_DIR = $CargoTargetRoot
        $env:CARGO_LLVM_COV_TARGET_DIR = $CoverageArtifactRoot
        $env:CARGO_LLVM_COV_BUILD_DIR = $CoverageArtifactRoot
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
        $CoverageFeatures = if ($DiagnosticMode) {
            @('--no-default-features')
        }
        else {
            @('--all-features')
        }
        $CargoArguments = @('llvm-cov', '--all-targets') +
            $CoverageFeatures + @('--locked') + $TargetArguments + @('--summary-only')
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo llvm-cov'
        exit 0
    }

    if ($ProbePlayback) {
        # The plugin-file gate names missing packages; the probes then prove
        # the installed runtime satisfies Balun's factory and appsrc contract
        # through the same release dependency graph the desktop build uses.
        Assert-PlaybackRuntime $MsysLayout
        Write-Info 'Probing the installed GStreamer playback runtime (release profile)...'
        foreach ($Probe in @(
            'playback::runtime::tests::installed_runtime_has_the_exact_playback_foundation',
            'playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc',
            'playback::runtime::tests::installed_runtime_reports_the_decoder_and_sink_inventory'
        )) {
            $CargoArguments = @('test', '--release', '--locked', '--features', 'desktop', '--lib') +
                $TargetDirectoryArguments + $TargetArguments +
                @($Probe, '--', '--ignored', '--exact', '--nocapture')
            Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo test'
        }
        Write-Info 'Playback runtime probes passed.'
        exit 0
    }

    if ($DiagnosticMode) {
        $CargoArguments = @(
            'build',
            '--release',
            '--locked',
            '--bin',
            'balun-discover'
        ) + $TargetDirectoryArguments + $TargetArguments
        Write-Info 'Building balun-discover (locked release diagnostic)...'
        Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo build'

        $BinaryPath = Join-Path (
            $CargoTargetRoot
        ) "$RustTarget\release\$DiagnosticBinaryName"
        $BinaryItem = Get-ValidatedBuildOutput $BinaryPath 'diagnostic'
        Write-Info "Application ID: $ApplicationId"
        Write-Info "Diagnostic output: $($BinaryItem.FullName)"

        if ($InspectLocal.IsPresent) {
            Write-Info 'Inspecting local HDHomeRun discovery...'
            $global:LASTEXITCODE = 0
            & $BinaryItem.FullName '--inspect' '--local'
            if ($LASTEXITCODE -ne 0) {
                Exit-WithError (
                    "balun-discover --inspect --local failed with exit code $LASTEXITCODE."
                )
            }
        }
        exit 0
    }

    if ($PackageMode) {
        # Every packaging input is resolved and validated before any build,
        # copy, or probe: the shared policy, the inspection and resource
        # tools, the plugin closure's sources, and the package version.
        $script:ForbiddenBundledComponentTokens = @(Import-ForbiddenBundledComponentPolicy (
            Join-Path $RepositoryRoot 'build-aux\packaging\forbidden-bundled-components.txt'
        ))
        $PackagingTools = Get-PackagingTools $MsysLayout
        Assert-PlaybackRuntime $MsysLayout
        $PackageVersion = Get-CargoPackageVersion (Join-Path $RepositoryRoot 'Cargo.toml')
        $PackageNumericVersion = ConvertTo-NumericVersion $PackageVersion
        $DistributionRoot = Join-Path $RepositoryRoot 'dist'
        $Distribution = Join-Path $DistributionRoot $DistributionName
        Write-Info "Application ID: $ApplicationId"
        Write-Info "Package version: $PackageVersion ($PackageNumericVersion)"

        if ($InnoSetup.IsPresent -and $SkipBundle.IsPresent) {
            # Installer-only mode: the existing tree is untrusted stale input.
            # Its probe receipt must still match the exact application and
            # closure anchors, and every non-executing gate is repeated.
            if (-not (Test-Path -LiteralPath $Distribution -PathType Container)) {
                Exit-WithError "No staged Windows bundle exists at $Distribution; run -Bundle or -Zip first."
            }
            $Distribution = (Resolve-Path -LiteralPath $Distribution).ProviderPath.TrimEnd([char[]]@('\', '/'))
            Assert-WindowsBundleRootIsNotReparsePoint $Distribution
            try {
                Assert-WindowsProbeReceipt $Distribution
            }
            catch {
                Exit-WithError $_.Exception.Message
            }
            Assert-WindowsPackageFinalGates $Distribution $PackagingTools.PeInspector $PackageVersion
            try {
                Assert-WindowsProbeReceipt $Distribution
            }
            catch {
                Exit-WithError $_.Exception.Message
            }
            Invoke-InnoSetup $Distribution $PackageVersion $PackageNumericVersion
            Write-Info 'Done.'
            exit 0
        }

        if ($NoCargoBuild.IsPresent) {
            Write-Info 'Skipping the cargo build (-NoCargoBuild specified).'
        }
        else {
            $CargoArguments = @(
                'build',
                '--release',
                '--locked',
                '--features',
                'desktop',
                '--bin',
                'balun'
            ) + $TargetDirectoryArguments + $TargetArguments
            Write-Info "Building Balun desktop (locked release for $RustTarget)..."
            Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo build'
        }
        $BinaryPath = Join-Path $RepositoryRoot "target\$RustTarget\release\$DesktopBinaryName"
        $BinaryItem = Get-ValidatedBuildOutput $BinaryPath 'desktop application'
        Write-Info "Desktop output: $($BinaryItem.FullName)"
        try {
            Assert-WindowsApplicationResourceContract `
                -Application $BinaryItem.FullName `
                -Inspector $PackagingTools.PeInspector `
                -ExpectedVersion $PackageVersion
        }
        catch {
            Exit-WithError "Built application resource validation failed: $($_.Exception.Message)"
        }
        Write-Info 'Built application icon and version resources passed.'

        Write-Info "Staging the Windows package under $Distribution ..."
        $Distribution = Invoke-WindowsPackageStaging $MsysLayout $PackagingTools $Distribution $BinaryItem.FullName
        Invoke-PackagedRuntimeProbe $Distribution
        try {
            Write-WindowsProbeReceipt $Distribution
        }
        catch {
            Exit-WithError "Could not persist the Windows packaged-runtime probe receipt: $($_.Exception.Message)"
        }
        # Recheck immediately before archiving so the emitted artifact, rather
        # than merely the earlier staging snapshot, is covered by every gate.
        Assert-WindowsPackageFinalGates $Distribution $PackagingTools.PeInspector $PackageVersion
        Write-Info "Staged package: $Distribution"

        if ($Zip.IsPresent -or $InnoSetup.IsPresent) {
            $null = New-WindowsZip $Distribution
        }
        if ($InnoSetup.IsPresent) {
            try {
                Assert-WindowsProbeReceipt $Distribution
            }
            catch {
                Exit-WithError $_.Exception.Message
            }
            Invoke-InnoSetup $Distribution $PackageVersion $PackageNumericVersion
        }
        Write-Info 'Done.'
        exit 0
    }

    if ($SkipBundle.IsPresent) {
        Write-Info 'Build-only run (-SkipBundle specified; packaging needs -Bundle, -Zip, or -InnoSetup).'
    }
    Assert-PlaybackRuntime $MsysLayout
    $DesktopFeatureSet = if ($Run.IsPresent) { 'desktop,windows-console' } else { 'desktop' }
    $CargoArguments = @(
        'build',
        '--release',
        '--locked',
        '--features',
        $DesktopFeatureSet,
        '--bin',
        'balun'
    ) + $TargetDirectoryArguments + $TargetArguments
    Write-Info "Building Balun desktop (locked release for $RustTarget)..."
    Invoke-Cargo -CargoCommand $CargoCommand -Arguments $CargoArguments -Description 'cargo build'

    $BinaryPath = Join-Path $RepositoryRoot "target\$RustTarget\release\$DesktopBinaryName"
    $BinaryItem = Get-ValidatedBuildOutput $BinaryPath 'desktop application'
    Write-Info "Application ID: $ApplicationId"
    Write-Info "Desktop output: $($BinaryItem.FullName)"

    if ($Run.IsPresent) {
        Write-Info "Launching $($BinaryItem.FullName)..."
        $global:LASTEXITCODE = 0
        & $BinaryItem.FullName
        exit $LASTEXITCODE
    }
    exit 0
}
finally {
    Pop-Location
}
