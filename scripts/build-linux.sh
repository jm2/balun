#!/usr/bin/env bash
# Balun — Linux desktop build helper
#
# The default route builds the reviewable GTK4/libadwaita/GStreamer desktop
# application
# without launching it. Native/Flatpak package modes remain unavailable until
# their recipes, assets, runtime closure, and artifact gates land together.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
target_directory="$repository_root/target"
coverage_target_directory="$target_directory/llvm-cov-target"
metadata_validator="$repository_root/build-aux/linux/validate-package-metadata.sh"
artifact_validator="$repository_root/build-aux/linux/validate-package-compliance.sh"
coverage_version="cargo-llvm-cov 0.8.7"
application_id='io.github.jm2.Balun'

usage()
{
    cat <<'EOF'
Balun — Linux desktop build helper.
A lightweight cross-platform HDHomeRun live TV viewer
Application ID: io.github.jm2.Balun

Usage:
  ./scripts/build-linux.sh [MODE] [--diagnostic]

With no options, builds the Balun GTK4/libadwaita/GStreamer desktop application
with Cargo's locked release dependency graph, then applies Balun's repository-
metadata and Linux ELF policy gates. The helper builds only and never launches
the application. Before building, it requires the GStreamer runtime plugin
files that provide playbin3, appsrc, tsdemux, deinterlace, and
gtk4paintablesink, and it warns when the libav broadcast decoders are absent.

Quick-exit modes (choose at most one):
  --fmt             Run cargo fmt across the workspace.
  --check           Check all desktop targets with locked dependencies.
  --clippy          Lint all desktop targets with warnings denied and locked
                    dependencies, in both the debug and release profiles.
  --coverage        Print an all-target desktop coverage summary; requires
                    cargo-llvm-cov 0.8.7 to be installed already.
  --probe-playback  Run the installed-runtime playback probes in the release
                    profile: the exact structural factory snapshot and the
                    constant-URI appsrc contract. Requires the desktop
                    development libraries and runtime plugins; it cannot be
                    combined with --diagnostic.

Build selection:
  --diagnostic      Select the GTK-free balun-discover route instead of the
                    desktop application. This also makes check, Clippy, and
                    coverage GTK-free.

Unavailable until their complete recipes and policy gates land:
  --flatpak, --deb, --rpm, --arch-pkg

This helper never invokes tool or package installers.
Cargo may fetch locked dependencies unless cached. A rustup-managed Cargo
invocation may also fetch the selected Rust toolchain.

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
    require_command pkg-config 'install pkg-config (Debian/Ubuntu: pkg-config, Fedora: pkgconf-pkg-config, Arch: pkgconf)'

    pkg-config --atleast-version=4.16 gtk4 >/dev/null 2>&1 || \
        fail 'gtk4 >= 4.16 was not found through pkg-config; install its development package explicitly (Debian/Ubuntu: libgtk-4-dev, Fedora: gtk4-devel, Arch: gtk4) and retry.'
    pkg-config --atleast-version=1.6 libadwaita-1 >/dev/null 2>&1 || \
        fail 'libadwaita-1 >= 1.6 was not found through pkg-config; install its development package explicitly (Debian/Ubuntu: libadwaita-1-dev, Fedora: libadwaita-devel, Arch: libadwaita) and retry.'
    pkg-config --atleast-version=1.20 gstreamer-1.0 >/dev/null 2>&1 || \
        fail 'gstreamer-1.0 >= 1.20 was not found through pkg-config; install its development package explicitly (Debian/Ubuntu: libgstreamer1.0-dev, Fedora: gstreamer1-devel, Arch: gstreamer) and retry.'
    info 'GTK 4.16, libadwaita 1.6, and GStreamer 1.20 development-library checks passed.'
}

# Runtime GStreamer plugins are invisible to pkg-config, and the desktop
# executable checks the same structural factories at startup. Fail before a
# desktop build whose only outcome would be "playback components unavailable".
# Package names are Fedora's reference names; the plugin filenames are the
# portable contract.
require_playback_runtime()
{
    local plugin_directory missing plugin factories package
    plugin_directory=$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null) \
        || plugin_directory=
    if [ -z "$plugin_directory" ] || [ ! -d "$plugin_directory" ]; then
        fail 'pkg-config did not report an existing GStreamer plugin directory (pluginsdir); install the GStreamer runtime explicitly and retry.'
    fi
    missing=
    while IFS='|' read -r plugin factories package; do
        [ -n "$plugin" ] || continue
        [ -f "$plugin_directory/$plugin.so" ] || \
            missing="$missing"$'\n'"  $plugin.so ($factories) from $package"
    done <<'PLUGINS'
libgstcoreelements|core elements|gstreamer1
libgstplayback|playbin3, uridecodebin3, decodebin3|gstreamer1-plugins-base
libgstapp|appsrc|gstreamer1-plugins-base
libgsttypefindfunctions|stream type detection|gstreamer1-plugins-base
libgstdeinterlace|deinterlace|gstreamer1-plugins-good
libgstmpegtsdemux|tsdemux|gstreamer1-plugins-bad-free
libgstgtk4|gtk4paintablesink|gstreamer1-plugin-gtk4
PLUGINS
    if [ -n "$missing" ]; then
        fail "Required GStreamer playback runtime is incomplete in $plugin_directory:$missing"$'\n'"Install your distribution's equivalent base, good, bad, and gtk4 (gst-plugins-rs) plugin packages explicitly and retry."
    fi
    if [ ! -f "$plugin_directory/libgstlibav.so" ]; then
        warn "libgstlibav.so is missing from $plugin_directory; MPEG-2, H.264, AC-3, and AAC broadcast decoding commonly needs gstreamer1-plugin-libav or your distribution's equivalent. The build continues, but live channels may report a missing codec."
    fi
    info 'GStreamer runtime plugin checks passed for the structural playback factories.'
}

mode=build
mode_option=
diagnostic=false
show_help=false

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
        --flatpak|--deb|--rpm|--arch-pkg)
            usage_error "Packaging mode '$argument' is not available yet; no build, install, or network work was started."
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

cd "$repository_root"
require_command cargo 'install Rust from https://rustup.rs'

if [ "$mode" = fmt ]; then
    info 'Formatting Balun...'
    cargo fmt --all
    info 'Formatting complete.'
    exit 0
fi

if [ "$mode" = build ]; then
    [ -x "$metadata_validator" ] || \
        fail "Required repository metadata validator is unavailable or not executable: $metadata_validator"
    [ -x "$artifact_validator" ] || \
        fail "Required Linux artifact validator is unavailable or not executable: $artifact_validator"
    require_command readelf 'install GNU binutils (Debian/Ubuntu, Fedora, and Arch: binutils); elfutils eu-readelf is not a substitute'
fi

require_command rustc 'install Rust from https://rustup.rs'
native_target_status=0
native_target=$(rustc --print host-tuple 2>/dev/null) || native_target_status=$?
if [ "$native_target_status" -ne 0 ] \
    || [ -z "$native_target" ] \
    || [ "${#native_target}" -gt 128 ] \
    || [[ ! "$native_target" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]] \
    || [[ "$native_target" != *-linux-* ]]; then
    fail 'rustc did not report one bounded native Linux host target.'
fi
release_directory="$target_directory/$native_target/release"
desktop_binary="$release_directory/balun"
diagnostic_binary="$release_directory/balun-discover"

if ! $diagnostic; then
    require_desktop_dependencies
fi

case "$mode" in
    check)
        if $diagnostic; then
            info 'Checking all Balun diagnostic targets with locked dependencies...'
            cargo check --all-targets --locked \
                --target-dir "$target_directory" --target "$native_target"
        else
            info 'Checking all Balun desktop targets with locked dependencies...'
            cargo check --all-targets --all-features --locked \
                --target-dir "$target_directory" --target "$native_target"
        fi
        info 'Check passed.'
        exit 0
        ;;
    clippy)
        # Tributary lints both profiles so cfg(debug_assertions)-gated code
        # cannot hide from either configuration.
        if $diagnostic; then
            info 'Linting all Balun diagnostic targets with locked dependencies...'
            cargo clippy --all-targets --locked \
                --target-dir "$target_directory" --target "$native_target" \
                -- -D warnings
            info 'Linting all Balun diagnostic targets in the release profile...'
            cargo clippy --release --all-targets --locked \
                --target-dir "$target_directory" --target "$native_target" \
                -- -D warnings
        else
            info 'Linting all Balun desktop targets with locked dependencies...'
            cargo clippy --all-targets --all-features --locked \
                --target-dir "$target_directory" --target "$native_target" \
                -- -D warnings
            info 'Linting all Balun desktop targets in the release profile...'
            cargo clippy --release --all-targets --all-features --locked \
                --target-dir "$target_directory" --target "$native_target" \
                -- -D warnings
        fi
        info 'Clippy passed.'
        exit 0
        ;;
    coverage)
        installed_coverage_version=$(cargo llvm-cov --version 2>/dev/null || true)
        if [ "$installed_coverage_version" != "$coverage_version" ]; then
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
        require_playback_runtime
        info 'Probing the installed GStreamer playback runtime (release profile)...'
        for probe in \
            playback::runtime::tests::installed_runtime_has_the_exact_playback_foundation \
            playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc \
            playback::runtime::tests::installed_runtime_reports_the_decoder_and_sink_inventory
        do
            cargo test --release --locked --features desktop --lib \
                --target-dir "$target_directory" --target "$native_target" \
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
    require_playback_runtime
fi

info 'Validating locked repository metadata...'
"$metadata_validator"

if $diagnostic; then
    info 'Building balun-discover (locked release diagnostic)...'
    cargo build --release --locked --bin balun-discover \
        --target-dir "$target_directory" --target "$native_target"
    binary=$diagnostic_binary
    artifact_label='balun-discover diagnostic'
else
    info 'Building Balun desktop (locked release)...'
    cargo build --release --locked --features desktop --bin balun \
        --target-dir "$target_directory" --target "$native_target"
    binary=$desktop_binary
    artifact_label='Balun desktop'
fi

[ -f "$binary" ] && [ ! -L "$binary" ] && [ -s "$binary" ] && [ -x "$binary" ] || \
    fail "Cargo did not produce the expected nonempty, executable, regular, non-symlink binary: $binary"

info "Validating $artifact_label Linux ELF policy..."
"$artifact_validator" --elf "$binary"
info "Application ID: $application_id"
if $diagnostic; then
    info "Diagnostic output: $binary"
else
    info "Desktop output: $binary"
fi
