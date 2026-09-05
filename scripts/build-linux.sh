#!/usr/bin/env bash
# Balun — Linux desktop build helper
#
# The default route builds the reviewable GTK4/libadwaita/GStreamer desktop
# application without launching it; --run launches the built application once
# its gates pass. Native package modes reuse the same locked build and policy
# gates before producing distribution-owned payloads.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
target_directory="$repository_root/target"
coverage_target_directory="$target_directory/llvm-cov-target"
dist_directory="$repository_root/dist"
metadata_validator="$repository_root/build-aux/linux/validate-package-metadata.sh"
artifact_validator="$repository_root/build-aux/linux/validate-package-compliance.sh"
arch_recipe_directory="$repository_root/build-aux/arch"
arch_recipe="$arch_recipe_directory/PKGBUILD"
coverage_version="cargo-llvm-cov 0.8.7"
cargo_deb_version="cargo-deb 3.7.0"
cargo_generate_rpm_version="cargo-generate-rpm 0.21.0"
application_id='io.github.jm2.Balun'

usage()
{
    cat <<'EOF'
Balun — Linux desktop build helper.
A lightweight cross-platform HDHomeRun live TV viewer
Application ID: io.github.jm2.Balun

Usage:
  ./scripts/build-linux.sh [MODE] [--diagnostic] [--run]

With no options, builds the Balun GTK4/libadwaita/GStreamer desktop application
with Cargo's locked release dependency graph, then applies Balun's repository-
metadata and Linux ELF policy gates. The helper builds only unless --run is
given. Before building, it requires the GStreamer runtime plugin
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
  --deb             Build a native Debian package with preinstalled cargo-deb
                    3.7.0 and reopen it with dpkg-deb. Supports amd64 and
                    arm64 GNU/Linux hosts.
  --rpm             Build a native RPM package with preinstalled
                    cargo-generate-rpm 0.21.0 and reopen it with rpm, rpm2cpio,
                    and cpio. Supports x86_64 and aarch64.
  --arch-pkg        Build an x86_64 Arch package with preinstalled makepkg and
                    reopen it with bsdtar.

Launch:
  --run             After the desktop build and its gates pass, replace this
                    helper with the built application so its log stays in this
                    terminal. Cannot be combined with quick-exit, --diagnostic,
                    or packaging modes.

Unavailable through this helper:
  --flatpak          The release workflow owns the reviewed Flatpak route.

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

validate_package_artifact()
{
    local package=$1 producer=$2
    [ -f "$package" ] && [ ! -L "$package" ] && [ -s "$package" ] || \
        fail "$producer did not produce the expected nonempty, regular, non-symlink package: $package"
}

build_debian_package()
{
    local package="$dist_directory/balun-$package_arch.deb"
    mkdir -p "$dist_directory"
    info "Building native Debian $package_arch package..."
    CARGO_TARGET_DIR="$target_directory" \
        cargo deb --locked --no-build --target "$native_target" \
        --output "$package"
    validate_package_artifact "$package" cargo-deb
    info 'Validating completed Debian package policy...'
    "$artifact_validator" --deb "$package"
    info "Debian package: $package"
}

build_rpm_package()
{
    local package="$dist_directory/balun-$package_arch.rpm"
    mkdir -p "$dist_directory"
    info "Building native $package_arch RPM package..."
    cargo generate-rpm \
        --target-dir "$target_directory" --target "$native_target" \
        --output "$package"
    validate_package_artifact "$package" cargo-generate-rpm
    info 'Validating completed RPM package policy...'
    "$artifact_validator" --rpm "$package"
    info "RPM package: $package"
}

build_arch_package()
{
    local build_directory built_package built_name package
    build_directory="$target_directory/arch"
    package="$dist_directory/balun-x86_64.pkg.tar.zst"
    mkdir -p "$build_directory" "$dist_directory"

    built_package=$(
        LC_ALL=C PKGEXT='.pkg.tar.zst' PKGDEST="$build_directory" \
            BUILDDIR="$build_directory/build" \
            makepkg --dir "$arch_recipe_directory" --packagelist
    )
    case "$built_package" in
        '' | *$'\n'*)
            fail 'makepkg did not report exactly one Arch package output path.'
            ;;
    esac
    [ "${built_package%/*}" = "$build_directory" ] || \
        fail "makepkg reported an output outside the reviewed build directory: $built_package"
    built_name=${built_package##*/}
    case "$built_name" in
        balun-*-x86_64.pkg.tar.zst) ;;
        *) fail "makepkg reported an unexpected Arch package filename: $built_name" ;;
    esac

    info 'Building native x86_64 Arch package...'
    LC_ALL=C PKGEXT='.pkg.tar.zst' PKGDEST="$build_directory" \
        BUILDDIR="$build_directory/build" \
        makepkg --dir "$arch_recipe_directory" --force --clean --noconfirm
    validate_package_artifact "$built_package" makepkg
    cp -- "$built_package" "$package"
    validate_package_artifact "$package" makepkg
    info 'Validating completed Arch package policy...'
    "$artifact_validator" --arch "$package"
    info "Arch package: $package"
}

mode=build
mode_option=
run=false
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
        --deb|--rpm|--arch-pkg)
            if [ "$mode" != build ]; then
                usage_error "Packaging modes cannot be combined ('$mode_option' and '$argument')."
            fi
            mode=${argument#--}
            mode_option=$argument
            ;;
        --run)
            run=true
            ;;
        --flatpak)
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
if $diagnostic; then
    case "$mode" in
        deb|rpm|arch-pkg)
            usage_error 'Native package modes build the desktop application and cannot be combined with --diagnostic.'
            ;;
    esac
fi
if $run && $diagnostic; then
    usage_error '--run launches only the desktop application and cannot be combined with --diagnostic.'
fi
if $run && [ "$mode" != build ]; then
    usage_error "--run cannot be combined with '$mode_option'; it launches only the plain desktop build."
fi

cd "$repository_root"
require_command cargo 'install Rust from https://rustup.rs'

if [ "$mode" = fmt ]; then
    info 'Formatting Balun...'
    cargo fmt --all
    info 'Formatting complete.'
    exit 0
fi

case "$mode" in
build|deb|rpm|arch-pkg)
    [ -x "$metadata_validator" ] || \
        fail "Required repository metadata validator is unavailable or not executable: $metadata_validator"
    [ -x "$artifact_validator" ] || \
        fail "Required Linux artifact validator is unavailable or not executable: $artifact_validator"
    require_command readelf 'install GNU binutils (Debian/Ubuntu, Fedora, and Arch: binutils); elfutils eu-readelf is not a substitute'
    case "$mode" in
        deb)
            require_command cargo-deb 'install the reviewed cargo-deb version explicitly'
            require_command dpkg-deb 'install dpkg explicitly; the completed package is reopened with it'
            installed_packager_version=$(cargo-deb --version 2>/dev/null || true)
            [ "$installed_packager_version" = "$cargo_deb_version" ] || \
                fail "Native Debian packaging requires preinstalled $cargo_deb_version exactly; this helper will not install or replace tools."
            ;;
        rpm)
            require_command cargo-generate-rpm 'install the reviewed cargo-generate-rpm version explicitly'
            require_command rpm 'install rpm explicitly; the completed package is reopened with it'
            require_command rpm2cpio 'install rpm explicitly; the completed package is reopened with it'
            require_command cpio 'install cpio explicitly; the completed package is reopened with it'
            installed_packager_version=$(cargo-generate-rpm --version 2>/dev/null || true)
            [ "$installed_packager_version" = "$cargo_generate_rpm_version" ] || \
                fail "Native RPM packaging requires preinstalled $cargo_generate_rpm_version exactly; this helper will not install or replace tools."
            ;;
        arch-pkg)
            require_command makepkg 'use an Arch Linux build host with base-devel installed'
            require_command bsdtar 'install Arch libarchive explicitly'
            require_command cp 'install GNU coreutils explicitly'
            [ -f "$arch_recipe" ] && [ ! -L "$arch_recipe" ] || \
                fail "Required Arch package recipe is unavailable or is not a regular file: $arch_recipe"
            ;;
    esac
    ;;
esac

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
package_arch=
case "$mode:$native_target" in
    deb:x86_64-unknown-linux-gnu) package_arch=amd64 ;;
    deb:aarch64-unknown-linux-gnu) package_arch=arm64 ;;
    rpm:x86_64-unknown-linux-gnu) package_arch=x86_64 ;;
    rpm:aarch64-unknown-linux-gnu) package_arch=aarch64 ;;
    arch-pkg:x86_64-unknown-linux-gnu) package_arch=x86_64 ;;
    deb:*|rpm:*)
        fail "Native package mode '$mode_option' supports only x86_64 and aarch64 GNU/Linux hosts; rustc reported $native_target."
        ;;
    arch-pkg:*)
        fail "Native package mode '--arch-pkg' supports only an x86_64 GNU/Linux host; rustc reported $native_target."
        ;;
esac

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
    build|deb|rpm|arch-pkg)
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

if [ "$mode" = arch-pkg ]; then
    build_arch_package
    exit 0
fi

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
    case "$mode" in
        build)
            info "Desktop output: $binary"
            if $run; then
                info 'Launching Balun desktop...'
                exec "$binary"
            fi
            ;;
        deb) build_debian_package ;;
        rpm) build_rpm_package ;;
    esac
fi
