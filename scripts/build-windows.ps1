<#
.SYNOPSIS
    Balun - Windows desktop build helper

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

.PARAMETER Bundle
    Unavailable until Balun has a reviewed packaged-GUI runtime closure.

.PARAMETER Zip
    Unavailable until Balun has a reviewed packaged-GUI runtime closure.

.PARAMETER InnoSetup
    Unavailable until Balun has a reviewed installer recipe and artifact gates.

.PARAMETER Package
    Unavailable until Balun has a reviewed GUI package implementation.

.PARAMETER Installer
    Unavailable until Balun has a reviewed installer recipe and artifact gates.

.PARAMETER SkipBundle
    Obsolete because the current default action is already build-only.

.PARAMETER NoCargoBuild
    Unavailable because this helper has no post-build package operation.

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
    path. This cannot be combined with Diagnostic, InspectLocal, or a quick-exit
    mode.

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
    [switch]$Bundle,
    [switch]$Zip,
    [switch]$InnoSetup,
    [switch]$Package,
    [switch]$Installer,
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
$DesktopBinaryName = 'balun.exe'
$DiagnosticBinaryName = 'balun-discover.exe'
$DesktopRustTarget = 'x86_64-pc-windows-gnullvm'
$MsysEnvironment = 'clang64'
$MsysPackagePrefix = 'mingw-w64-clang-x86_64'
$RequiredCoverageVersion = 'cargo-llvm-cov 0.8.7'
$Msys2RootSpecified = $PSBoundParameters.ContainsKey('Msys2Root')

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
# build, install, package, launch, or network work.
$UnavailableOptions = @()
foreach ($option in @(
    @{ Name = '-Bundle'; Enabled = $PSBoundParameters.ContainsKey('Bundle') },
    @{ Name = '-Zip'; Enabled = $PSBoundParameters.ContainsKey('Zip') },
    @{ Name = '-InnoSetup'; Enabled = $PSBoundParameters.ContainsKey('InnoSetup') },
    @{ Name = '-Package'; Enabled = $PSBoundParameters.ContainsKey('Package') },
    @{ Name = '-Installer'; Enabled = $PSBoundParameters.ContainsKey('Installer') },
    @{ Name = '-SkipBundle'; Enabled = $PSBoundParameters.ContainsKey('SkipBundle') },
    @{ Name = '-NoCargoBuild'; Enabled = $PSBoundParameters.ContainsKey('NoCargoBuild') },
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
        'Balun currently supports build, check, test, and explicit desktop ' +
        'launch operations only; no external work was started.'
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

    Assert-PlaybackRuntime $MsysLayout
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
