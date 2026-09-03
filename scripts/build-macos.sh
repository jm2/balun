#!/usr/bin/env bash
# Balun — macOS desktop build helper
#
# This intentionally stops at a native executable. It does not create an
# application bundle, disk image, native package, or staged runtime closure.

set -euo pipefail

usage()
{
    while IFS= read -r usage_line; do
        printf '%s\n' "$usage_line"
    done <<'EOF'
Balun — macOS desktop build helper.
A lightweight cross-platform HDHomeRun live TV viewer
Application ID: io.github.jm2.Balun

Usage:
  ./scripts/build-macos.sh [options]

With no options, builds the native Balun desktop executable with Cargo's locked
release dependency graph and the desktop feature, then applies Balun's pinned
Mach-O component policy. This produces only
target/<native-target>/release/balun and does not launch Balun.
It does not create Balun.app, a DMG, a native package, or a staged runtime
closure.

Build selector:
  --diagnostic      Use the GTK-free balun-discover diagnostic instead of the
                    desktop application. May be combined with a quick mode.

Quick-exit modes (choose at most one):
  --fmt             Run cargo fmt across the workspace.
  --check           Check all targets with desktop features by default.
  --clippy          Lint all targets with desktop features and warnings denied,
                    in both the debug and release profiles.
  --coverage        Print an all-target coverage summary with desktop features
                    by default; requires cargo-llvm-cov 0.8.7 installed already.
  --probe-playback  Run the installed-runtime playback probes in the release
                    profile: the exact structural factory snapshot and the
                    constant-URI appsrc contract. Requires the desktop
                    development libraries and runtime plugins; it cannot be
                    combined with --diagnostic.

Desktop compilation requires preinstalled, pkg-config-visible GTK 4.16,
libadwaita 1.6, and GStreamer 1.20 development libraries. The diagnostic and
format routes do not. Homebrew's pkg-config resolves its own prefix, so the
helper never queries Homebrew for it. The desktop build additionally requires the GStreamer
runtime plugin files that provide playbin3, appsrc, tsdemux, deinterlace, and
gtk4paintablesink, and warns when the libav broadcast decoders are absent.
Every compilation route requires a preinstalled rustc reporting one native
Apple Darwin host tuple. The format route does not.

Unavailable until complete app-bundle recipes and final-artifact gates land:
  --dmg, --app, --bundle, --package, --pkg, --installer, --sign, --notarize

This helper never invokes Homebrew, another package manager, an installer, a
downloader, or runtime-copy staging.
Cargo may fetch locked dependencies unless cached.
A rustup-managed Cargo or rustc may also fetch the selected Rust toolchain.
This helper never installs or updates either one itself.

Other:
  -h, --help        Show this help and exit.
EOF
}

info()
{
    printf '[balun] %s\n' "$*"
}

fail()
{
    printf '[balun] %s\n' "$*" >&2
    exit 1
}

warn()
{
    printf '[balun] warning: %s\n' "$*" >&2
}

usage_error()
{
    printf '[balun] %s\n' "$*" >&2
    usage >&2
    exit 2
}

require_command()
{
    command -v "$1" >/dev/null 2>&1 || \
        fail "Required command '$1' is unavailable; ${2:-install it explicitly} and retry."
}

require_desktop_dependencies()
{
    require_command pkg-config 'install the Homebrew pkgconf formula'

    pkg-config --atleast-version=4.16 gtk4 >/dev/null 2>&1 || \
        fail 'gtk4 >= 4.16 was not found through pkg-config; install its development package explicitly (Homebrew formula gtk4) and retry.'
    pkg-config --atleast-version=1.6 libadwaita-1 >/dev/null 2>&1 || \
        fail 'libadwaita-1 >= 1.6 was not found through pkg-config; install its development package explicitly (Homebrew formula libadwaita) and retry.'
    pkg-config --atleast-version=1.20 gstreamer-1.0 >/dev/null 2>&1 || \
        fail 'gstreamer-1.0 >= 1.20 was not found through pkg-config; install its development package explicitly (Homebrew formula gstreamer) and retry.'
    info 'GTK 4.16, libadwaita 1.6, and GStreamer 1.20 development-library checks passed.'
}

# Runtime GStreamer plugins are invisible to pkg-config, and the desktop
# executable checks the same structural factories at startup. Fail before a
# desktop build whose only outcome would be "playback components unavailable".
require_playback_runtime()
{
    local plugin_directory missing plugin factories
    plugin_directory=$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null) \
        || plugin_directory=
    if [ -z "$plugin_directory" ] || [ ! -d "$plugin_directory" ]; then
        fail 'pkg-config did not report an existing GStreamer plugin directory (pluginsdir); install the GStreamer runtime explicitly and retry.'
    fi
    missing=
    while IFS='|' read -r plugin factories; do
        [ -n "$plugin" ] || continue
        [ -f "$plugin_directory/$plugin.dylib" ] || \
            missing="$missing"$'\n'"  $plugin.dylib ($factories)"
    done <<'PLUGINS'
libgstcoreelements|core elements
libgstplayback|playbin3, uridecodebin3, decodebin3
libgstapp|appsrc
libgsttypefindfunctions|stream type detection
libgstdeinterlace|deinterlace
libgstmpegtsdemux|tsdemux
libgstgtk4|gtk4paintablesink
PLUGINS
    if [ -n "$missing" ]; then
        fail "Required GStreamer playback runtime is incomplete in $plugin_directory:$missing"$'\n'"Install or update the Homebrew gstreamer formula, which supplies the base, good, bad, and gst-plugins-rs (gtk4) plugins, then retry."
    fi
    if [ ! -f "$plugin_directory/libgstlibav.dylib" ]; then
        warn "libgstlibav.dylib is missing from $plugin_directory; MPEG-2, H.264, AC-3, and AAC broadcast decoding commonly needs the libav plugin that the Homebrew gstreamer formula includes. The build continues, but live channels may report a missing codec."
    fi
    info 'GStreamer runtime plugin checks passed for the structural playback factories.'
}

resolve_native_target()
{
    local LC_ALL=C
    local rustc_target_status=0
    export LC_ALL

    require_command rustc 'install Rust from https://rustup.rs'

    native_target=$(rustc --print host-tuple 2>/dev/null) \
        || rustc_target_status=$?
    if [ "$rustc_target_status" -ne 0 ]; then
        fail 'rustc could not report its native host tuple; select a preinstalled macOS Rust toolchain and retry.'
    fi
    if [ -z "$native_target" ] || [ "${#native_target}" -gt 128 ] \
        || [[ ! "$native_target" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*-apple-darwin$ ]]; then
        fail 'rustc host tuple must be one bounded Apple Darwin target; no Cargo build was started.'
    fi
}

mode=build
mode_option=
show_help=false
diagnostic=false

# Reject every recognized package-producing route while argument parsing is
# still the only work performed. In particular, --help cannot mask --dmg.
for argument in "$@"; do
    case "$argument" in
        -h|--help)
            show_help=true
            ;;
        --diagnostic)
            diagnostic=true
            ;;
        --fmt|--check|--clippy|--coverage|--probe-playback)
            if [ "$mode" != build ]; then
                usage_error "Quick-exit modes cannot be combined ('$mode_option' and '$argument')."
            fi
            mode=${argument#--}
            mode_option=$argument
            ;;
        --dmg|--dmg=*|--app|--app=*|--bundle|--bundle=*|\
        --package|--package=*|--pkg|--pkg=*|--installer|--installer=*|\
        --sign|--sign=*|--notarize|--notarize=*)
            usage_error "Packaging mode '$argument' is not available yet; no Cargo, tool, install, package, or network work was started."
            ;;
        *)
            usage_error "Unknown option: $argument"
            ;;
    esac
done

if $show_help; then
    usage
    exit 0
fi

if $diagnostic && [ "$mode" = probe-playback ]; then
    usage_error '--probe-playback exercises the desktop playback runtime and cannot be combined with --diagnostic.'
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
target_directory="$repository_root/target"
coverage_target_directory="$target_directory/llvm-cov-target"
policy_helper="$script_dir/macos-package-policy.sh"
policy_file="$repository_root/build-aux/packaging/forbidden-bundled-components.txt"
coverage_version='cargo-llvm-cov 0.8.7'
application_id='io.github.jm2.Balun'

if $diagnostic; then
    binary_name=balun-discover
    mode_label=diagnostic
    artifact_label='balun-discover diagnostic'
else
    binary_name=balun
    mode_label=desktop
    artifact_label='Balun desktop'
fi

cd "$repository_root"
require_command cargo 'install Rust from https://rustup.rs'

if [ "$mode" = fmt ]; then
    info 'Formatting Balun...'
    cargo fmt --all
    info 'Formatting complete.'
    exit 0
fi

if [ "$mode" = build ]; then
    host_system=$(uname -s 2>/dev/null || true)
    [ "$host_system" = Darwin ] || \
        fail 'The default build route requires a native macOS host; no Cargo build was started.'

    [ -f "$policy_helper" ] && [ ! -L "$policy_helper" ] || \
        fail "Required macOS package-policy helper is unavailable or unsafe: $policy_helper"
    [ -f "$policy_file" ] && [ ! -L "$policy_file" ] || \
        fail "Pinned macOS component policy is unavailable or unsafe: $policy_file"
fi

resolve_native_target
binary="$target_directory/$native_target/release/$binary_name"

case "$mode" in
    check)
        if $diagnostic; then
            info "Checking all Balun $mode_label targets with locked dependencies..."
            cargo check --all-targets --locked \
                --target "$native_target" --target-dir "$target_directory"
        else
            require_desktop_dependencies
            info "Checking all Balun $mode_label targets with locked dependencies..."
            cargo check --all-targets --all-features --locked \
                --target "$native_target" --target-dir "$target_directory"
        fi
        info 'Check passed.'
        exit 0
        ;;
    clippy)
        # Tributary lints both profiles so cfg(debug_assertions)-gated code
        # cannot hide from either configuration.
        if $diagnostic; then
            info "Linting all Balun $mode_label targets with locked dependencies..."
            cargo clippy --all-targets --locked \
                --target "$native_target" --target-dir "$target_directory" \
                -- -D warnings
            info "Linting all Balun $mode_label targets in the release profile..."
            cargo clippy --release --all-targets --locked \
                --target "$native_target" --target-dir "$target_directory" \
                -- -D warnings
        else
            require_desktop_dependencies
            info "Linting all Balun $mode_label targets with locked dependencies..."
            cargo clippy --all-targets --all-features --locked \
                --target "$native_target" --target-dir "$target_directory" \
                -- -D warnings
            info "Linting all Balun $mode_label targets in the release profile..."
            cargo clippy --release --all-targets --all-features --locked \
                --target "$native_target" --target-dir "$target_directory" \
                -- -D warnings
        fi
        info 'Clippy passed.'
        exit 0
        ;;
    coverage)
        if ! $diagnostic; then
            require_desktop_dependencies
        fi
        coverage_version_status=0
        installed_coverage_version=$(cargo llvm-cov --version 2>/dev/null) \
            || coverage_version_status=$?
        if [ "$coverage_version_status" -ne 0 ] \
            || [ "$installed_coverage_version" != "$coverage_version" ]; then
            fail "Coverage requires preinstalled $coverage_version exactly; this helper will not install or replace tools."
        fi
        info "Running informational coverage with $coverage_version..."
        if $diagnostic; then
            CARGO_TARGET_DIR="$target_directory" \
                CARGO_LLVM_COV_TARGET_DIR="$coverage_target_directory" \
                CARGO_LLVM_COV_BUILD_DIR="$coverage_target_directory" \
                cargo llvm-cov --all-targets --no-default-features --locked \
                --target "$native_target" --summary-only
        else
            CARGO_TARGET_DIR="$target_directory" \
                CARGO_LLVM_COV_TARGET_DIR="$coverage_target_directory" \
                CARGO_LLVM_COV_BUILD_DIR="$coverage_target_directory" \
                cargo llvm-cov --all-targets --all-features --locked \
                --target "$native_target" --summary-only
        fi
        exit 0
        ;;
    probe-playback)
        # The plugin-file gate names missing packages; the probes then prove
        # the installed runtime satisfies Balun's factory and appsrc contract
        # through the same release dependency graph the desktop build uses.
        require_desktop_dependencies
        require_playback_runtime
        info 'Probing the installed GStreamer playback runtime (release profile)...'
        for probe in \
            playback::runtime::tests::installed_runtime_has_the_exact_playback_foundation \
            playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc \
            playback::runtime::tests::installed_runtime_reports_the_decoder_and_sink_inventory
        do
            cargo test --release --locked --features desktop --lib \
                --target "$native_target" --target-dir "$target_directory" \
                "$probe" -- --ignored --exact --nocapture
        done
        info 'Playback runtime probes passed.'
        exit 0
        ;;
    build)
        ;;
    *)
        fail "Internal error: unhandled build mode '$mode'."
        ;;
esac

if ! $diagnostic; then
    require_desktop_dependencies
    require_playback_runtime
fi

# shellcheck source=scripts/macos-package-policy.sh
source "$policy_helper"
declare -F macos_package_policy_load >/dev/null 2>&1 \
    || fail 'macOS package-policy helper does not provide macos_package_policy_load.'
declare -F macos_validate_macho_copy_control >/dev/null 2>&1 \
    || fail 'macOS package-policy helper does not provide macos_validate_macho_copy_control.'

# Production builds use macOS system inspection tools even if a parent shell
# exported one of the policy helper's test hooks.
balun_macos_sha256()
{
    /usr/bin/shasum -a 256 "$1"
}
readonly MACOS_SHA256_COMMAND=balun_macos_sha256
readonly MACOS_PERL_COMMAND=/usr/bin/perl
readonly MACOS_OTOOL_COMMAND=/usr/bin/otool

if ! macos_package_policy_load "$policy_file"; then
    fail "Pinned macOS component policy could not be loaded: $MACOS_PACKAGE_POLICY_REASON"
fi
if [ "$MACOS_PACKAGE_POLICY_RESULT" != loaded ] \
    || [[ ! "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" =~ ^[1-9][0-9]*$ ]]; then
    fail 'Pinned macOS component policy returned success without an active enforcement set.'
fi
info "Loaded $MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT forbidden bundle filename tokens."

if $diagnostic; then
    info 'Building balun-discover (native locked release diagnostic)...'
    cargo build --release --locked --bin balun-discover \
        --target "$native_target" --target-dir "$target_directory"
else
    info 'Building Balun desktop (native locked release)...'
    cargo build --release --locked --features desktop --bin balun \
        --target "$native_target" --target-dir "$target_directory"
fi

[ -f "$binary" ] && [ ! -L "$binary" ] && [ -s "$binary" ] \
    && [ -x "$binary" ] || \
    fail "Cargo did not produce the expected nonempty, executable, regular, non-symlink binary: $binary"

if ! macos_validate_macho_copy_control "$binary" false; then
    fail "$artifact_label failed macOS Mach-O component-policy inspection: $MACOS_PACKAGE_POLICY_REASON"
fi
[ "$MACOS_PACKAGE_POLICY_RESULT" = allowed ] || \
    fail 'macOS Mach-O policy returned success without marking the build output allowed.'

info "Application ID: $application_id"
info "Mach-O component policy passed for expected $artifact_label path: $binary"
