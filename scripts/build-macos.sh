#!/usr/bin/env bash
# Balun — macOS desktop build and packaging helper (.app bundle, optional .dmg)

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
Mach-O component policy. It does not create Balun.app, a DMG, a native package,
or a staged runtime closure. This produces only
target/<native-target>/release/balun and does not launch Balun.

Packaging options:
  --app             Perform release build and assemble the self-contained
                    application bundle dist/Balun.app with transitive dylib
                    closure, plugin closure, ad-hoc codesigning, package policy
                    validation, and relocated runtime probe verification.
  --dmg             After bundling and verifying dist/Balun.app, create the
                    drag-to-Applications disk image dist/Balun.dmg via create-dmg.

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
                    combined with --diagnostic or packaging modes.

Desktop compilation requires preinstalled, pkg-config-visible GTK 4.16,
libadwaita 1.6, and GStreamer 1.20 development libraries. The diagnostic and
format routes do not. Homebrew's pkg-config resolves its own prefix, so the
helper never queries Homebrew for it. The desktop build additionally requires the GStreamer
runtime plugin files that provide playbin3, appsrc, tsdemux, deinterlace, and
gtk4paintablesink, and warns when the libav broadcast decoders are absent.
Every compilation route requires a preinstalled rustc reporting one native
Apple Darwin host tuple. The format route does not.

Unavailable until complete native package recipes land:
  --bundle, --package, --pkg, --installer, --sign, --notarize

This helper never invokes Homebrew, another package manager, an installer, or
downloader.
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

# 21-element macOS GStreamer plugin closure defined and enforced by Balun
readonly GSTREAMER_MACOS_PLUGIN_CLOSURE=(
    libgstcoreelements
    libgstplayback
    libgstapp
    libgsttypefindfunctions
    libgstmpegtsdemux
    libgstdeinterlace
    libgstgtk4
    libgstvideoparsersbad
    libgstaudioparsers
    libgstlibav
    libgstapplemedia
    libgstmpg123
    libgstfaad
    libgstfdkaac
    libgstvideoconvertscale
    libgstvideofilter
    libgstaudioconvert
    libgstaudioresample
    libgstopengl
    libgstautodetect
    libgstosxaudio
)

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

require_packaging_runtime()
{
    require_command otool 'install Xcode Command Line Tools via xcode-select --install'
    require_command install_name_tool 'install Xcode Command Line Tools via xcode-select --install'
    require_command codesign 'install Xcode Command Line Tools via xcode-select --install'
    require_command ditto 'install Xcode Command Line Tools via xcode-select --install'
    require_command iconutil 'install Xcode Command Line Tools via xcode-select --install'

    if $make_dmg; then
        require_command create-dmg 'install create-dmg via brew install create-dmg'
    fi

    local plugin_directory missing
    plugin_directory=$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null) || plugin_directory=
    if [ -z "$plugin_directory" ] || [ ! -d "$plugin_directory" ]; then
        fail 'pkg-config did not report an existing GStreamer plugin directory (pluginsdir); install the GStreamer runtime explicitly and retry.'
    fi

    missing=
    for plugin in "${GSTREAMER_MACOS_PLUGIN_CLOSURE[@]}"; do
        if [ ! -f "$plugin_directory/$plugin.dylib" ]; then
            missing="$missing"$'\n'"  $plugin.dylib"
        fi
    done
    if [ -n "$missing" ]; then
        fail "Required 21-element GStreamer plugin closure is incomplete in $plugin_directory:$missing"$'\n'"Install missing GStreamer plugin formulas via Homebrew and retry."
    fi
}

resolve_scanner_and_loaders()
{
    local brew_prefix candidate
    brew_prefix=$(pkg-config --variable=prefix gstreamer-1.0 2>/dev/null || true)
    if [ -z "$brew_prefix" ]; then
        brew_prefix="/opt/homebrew"
    fi

    gst_scanner_src=""
    for candidate in \
        "$brew_prefix/libexec/gstreamer-1.0/gst-plugin-scanner" \
        "$brew_prefix/Cellar/gstreamer"/*/libexec/gstreamer-1.0/gst-plugin-scanner \
        "$brew_prefix/opt/gstreamer/libexec/gstreamer-1.0/gst-plugin-scanner" \
        "/opt/homebrew/libexec/gstreamer-1.0/gst-plugin-scanner" \
        "/opt/homebrew/Cellar/gstreamer"/*/libexec/gstreamer-1.0/gst-plugin-scanner \
        "/opt/homebrew/opt/gstreamer/libexec/gstreamer-1.0/gst-plugin-scanner" \
        "/usr/local/libexec/gstreamer-1.0/gst-plugin-scanner" \
        ; do
        if [ -f "$candidate" ]; then
            gst_scanner_src="$candidate"
            break
        fi
    done
    if [ -z "$gst_scanner_src" ]; then
        fail "gst-plugin-scanner was not found in any known GStreamer location; verify Homebrew gstreamer installation."
    fi

    pixbuf_query_src=""
    for candidate in \
        "$(pkg-config --variable=prefix gdk-pixbuf-2.0 2>/dev/null || true)/bin/gdk-pixbuf-query-loaders" \
        "$brew_prefix/bin/gdk-pixbuf-query-loaders" \
        "/opt/homebrew/bin/gdk-pixbuf-query-loaders" \
        "/usr/local/bin/gdk-pixbuf-query-loaders" \
        ; do
        if [ -x "$candidate" ]; then
            pixbuf_query_src="$candidate"
            break
        fi
    done
    if [ -z "$pixbuf_query_src" ]; then
        fail "gdk-pixbuf-query-loaders was not found in any known location; verify Homebrew gdk-pixbuf installation."
    fi
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

make_app=false
make_dmg=false
mode=build
mode_option=
show_help=false
diagnostic=false

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
        --app)
            make_app=true
            ;;
        --dmg)
            make_dmg=true
            make_app=true
            ;;
        --bundle|--bundle=*|\
        --package|--package=*|--pkg|--pkg=*|--installer|--installer=*|\
        --sign|--sign=*|--notarize|--notarize=*)
            usage_error "Packaging mode '$argument' is not available yet; no Cargo, tool, install, package, or network work was started."
            ;;
        --app=*|--dmg=*)
            usage_error "Packaging mode '$argument' does not accept a value; no Cargo, tool, install, package, or network work was started."
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

if $diagnostic && ( $make_app || $make_dmg ); then
    usage_error 'Packaging modes (--app, --dmg) build the desktop application bundle and cannot be combined with --diagnostic.'
fi

if [ "$mode" != build ] && ( $make_app || $make_dmg ); then
    usage_error "Quick-exit mode '$mode_option' cannot be combined with packaging modes."
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

if $make_app || $make_dmg; then
    require_packaging_runtime
    resolve_scanner_and_loaders
fi

# shellcheck source=scripts/macos-package-policy.sh
source "$policy_helper"
declare -F macos_package_policy_load >/dev/null 2>&1 \
    || fail 'macOS package-policy helper does not provide macos_package_policy_load.'
declare -F macos_validate_macho_copy_control >/dev/null 2>&1 \
    || fail 'macOS package-policy helper does not provide macos_validate_macho_copy_control.'
declare -F macos_validate_bundle_copy_control >/dev/null 2>&1 \
    || fail 'macOS package-policy helper does not provide macos_validate_bundle_copy_control.'

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

if ! $make_app && ! $make_dmg; then
    exit 0
fi

# ── .app Bundle Staging ──────────────────────────────────────────────────────
APP_NAME="Balun"
BUNDLE_ID="io.github.jm2.Balun"
APP_BUNDLE="dist/${APP_NAME}.app"
CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
RESOURCES_DIR="${APP_BUNDLE}/Contents/Resources"
FRAMEWORKS_DIR="${APP_BUNDLE}/Contents/Frameworks"
MACOS_DIR="${APP_BUNDLE}/Contents/MacOS"
GST_PLUGIN_DEST="${RESOURCES_DIR}/lib/gstreamer-1.0"
PIXBUF_LOADERS_DEST="${RESOURCES_DIR}/lib/gdk-pixbuf-2.0/2.10.0/loaders"
GST_SCANNER_DEST="${MACOS_DIR}/gst-plugin-scanner"
PIXBUF_QUERY_DEST="${MACOS_DIR}/gdk-pixbuf-query-loaders"
BIN_DEST="${MACOS_DIR}/${APP_NAME}"
if [ -d "/opt/homebrew" ]; then
    BREW_PREFIX="/opt/homebrew"
elif [ -d "/usr/local/Cellar" ] || [ -d "/usr/local/opt" ]; then
    BREW_PREFIX="/usr/local"
else
    BREW_PREFIX="$(pkg-config --variable=prefix gstreamer-1.0 2>/dev/null || echo "/opt/homebrew")"
fi

info "Creating ${APP_BUNDLE}..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR" "$FRAMEWORKS_DIR" "$GST_PLUGIN_DEST"

cp "$binary" "${BIN_DEST}-bin"
chmod +x "${BIN_DEST}-bin"

# Staged launcher establishing strict environment blinding and install-keyed cache isolation
cat > "${BIN_DEST}" << 'EOF'
#!/bin/bash
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd -P )"
CONTENTS_DIR="$(dirname "$DIR")"
BUNDLE_ROOT="$CONTENTS_DIR"
APP_ROOT="$(cd "$CONTENTS_DIR/.." && pwd -P)"

# Blind the app to Homebrew by stripping it from PATH
export PATH="/usr/bin:/bin:/usr/sbin:/sbin"

if [[ -z "${DYLD_LIBRARY_PATH+x}" ]]; then
  export DYLD_LIBRARY_PATH="$BUNDLE_ROOT/Frameworks"
fi

if [[ -z "${GST_PLUGIN_SYSTEM_PATH+x}" ]]; then
  export GST_PLUGIN_SYSTEM_PATH=""
fi

if [[ -z "${GST_PLUGIN_PATH+x}" ]]; then
  export GST_PLUGIN_PATH="$BUNDLE_ROOT/Resources/lib/gstreamer-1.0"
fi

if [[ -z "${GST_PLUGIN_SCANNER+x}" ]]; then
  export GST_PLUGIN_SCANNER="$DIR/gst-plugin-scanner"
fi

if [[ -z "${XDG_DATA_DIRS+x}" ]]; then
  export XDG_DATA_DIRS="$BUNDLE_ROOT/Resources/share"
fi

if [[ -z "${GTK_DATA_PREFIX+x}" ]]; then
  export GTK_DATA_PREFIX="$BUNDLE_ROOT/Resources"
fi

if [[ -z "${GSETTINGS_SCHEMA_DIR+x}" ]]; then
  export GSETTINGS_SCHEMA_DIR="$BUNDLE_ROOT/Resources/share/glib-2.0/schemas"
fi

# Check if running under platform runtime probe
PROBE_CACHE=""
PREV=""
for arg in "$@"; do
  if [[ "$PREV" == "--balun-platform-runtime-probe" ]]; then
    PROBE_CACHE="$arg"
    break
  fi
  PREV="$arg"
done

if [[ -n "$PROBE_CACHE" ]]; then
  exec "$DIR/Balun-bin" "$@"
fi

# Calculate 16-hex FNV-1a hash of canonical bundle root path
INSTALL_KEY="$(/usr/bin/perl -e '
  use Math::BigInt;
  my $str = $ARGV[0];
  my $h = Math::BigInt->new("0xcbf29ce484222325");
  my $prime = Math::BigInt->new("0x100000001b3");
  my $mask = (Math::BigInt->new(1) << 64) - 1;
  for my $b (unpack("C*", $str)) {
    $h ^= $b;
    $h = ($h * $prime) & $mask;
  }
  printf("%016s\n", $h->as_hex() =~ s/^0x//r);
' "$APP_ROOT")"
ARCH="$(uname -m)"
if [[ "$ARCH" == "arm64" ]]; then
  ARCH="aarch64"
fi
CACHE_ROOT="$HOME/Library/Caches/balun/runtime/macos-${ARCH}/${INSTALL_KEY}"

mkdir -p "$CACHE_ROOT/gstreamer"
mkdir -p "$CACHE_ROOT/gdk-pixbuf"

if [[ -z "${GST_REGISTRY+x}" ]]; then
  export GST_REGISTRY="$CACHE_ROOT/gstreamer/registry.bin"
fi

if [[ -z "${GDK_PIXBUF_MODULE_FILE+x}" ]]; then
  export GDK_PIXBUF_MODULE_FILE="$CACHE_ROOT/gdk-pixbuf/loaders.cache"
fi

LOADERS_DIR="$BUNDLE_ROOT/Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders"
if [[ ! -f "$GDK_PIXBUF_MODULE_FILE" ]]; then
  if [[ -x "$DIR/gdk-pixbuf-query-loaders" && -d "$LOADERS_DIR" ]]; then
    "$DIR/gdk-pixbuf-query-loaders" "$LOADERS_DIR"/*.dylib > "$GDK_PIXBUF_MODULE_FILE" 2>/dev/null || true
  fi
fi

exec "$DIR/Balun-bin" "$@"
EOF
chmod +x "${BIN_DEST}"

# ── Icons & Schemas ──────────────────────────────────────────────────────────
mkdir -p "${RESOURCES_DIR}/share/icons"
cp -RL "${BREW_PREFIX}/share/icons/hicolor" "${RESOURCES_DIR}/share/icons/" 2>/dev/null || true
cp -RL "${BREW_PREFIX}/share/icons/Adwaita" "${RESOURCES_DIR}/share/icons/" 2>/dev/null || true
if [[ -d "data/icons/hicolor" ]]; then
    cp -R "data/icons/hicolor"/* "${RESOURCES_DIR}/share/icons/hicolor/" 2>/dev/null || true
fi
if command -v gtk4-update-icon-cache &>/dev/null; then
    gtk4-update-icon-cache -f -t "${RESOURCES_DIR}/share/icons/hicolor" 2>/dev/null || true
    gtk4-update-icon-cache -f -t "${RESOURCES_DIR}/share/icons/Adwaita" 2>/dev/null || true
fi

mkdir -p "${RESOURCES_DIR}/share/glib-2.0/schemas"
cp -RL "${BREW_PREFIX}/share/glib-2.0/schemas" "${RESOURCES_DIR}/share/glib-2.0/" 2>/dev/null || true
if command -v glib-compile-schemas &>/dev/null; then
    glib-compile-schemas "${RESOURCES_DIR}/share/glib-2.0/schemas" 2>/dev/null || true
fi

# ── Pixbuf Loaders ───────────────────────────────────────────────────────────
PIXBUF_LOADER_DIR="${BREW_PREFIX}/lib/gdk-pixbuf-2.0"
if [[ -d "$PIXBUF_LOADER_DIR" ]]; then
    mkdir -p "${RESOURCES_DIR}/lib"
    cp -RL "$PIXBUF_LOADER_DIR" "${RESOURCES_DIR}/lib/" 2>/dev/null || true
    rm -f "${RESOURCES_DIR}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
fi
cp "$pixbuf_query_src" "$PIXBUF_QUERY_DEST"
chmod u+w "$PIXBUF_QUERY_DEST"

# ── 21-element GStreamer Plugin Closure ───────────────────────────────────────
plugin_directory=$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null)
info "Staging 21 GStreamer plugins into ${GST_PLUGIN_DEST}..."
for plugin in "${GSTREAMER_MACOS_PLUGIN_CLOSURE[@]}"; do
    src_plugin="${plugin_directory}/${plugin}.dylib"
    [ -f "$src_plugin" ] || fail "Required GStreamer plugin is missing: $src_plugin"
    cp "$src_plugin" "${GST_PLUGIN_DEST}/"
    chmod u+w "${GST_PLUGIN_DEST}/${plugin}.dylib"
done
bundled_plugin_count=$(ls -1 "${GST_PLUGIN_DEST}"/*.dylib 2>/dev/null | wc -l | tr -d ' ')
[ "$bundled_plugin_count" -eq 21 ] || fail "Expected 21 bundled GStreamer plugins, found $bundled_plugin_count"
info "Bundled $bundled_plugin_count GStreamer plugins."

cp "$gst_scanner_src" "$GST_SCANNER_DEST"
chmod u+w "$GST_SCANNER_DEST"

# ── Icon & Info.plist ────────────────────────────────────────────────────────
if [[ -d "data/balun.iconset" ]] && command -v iconutil &>/dev/null; then
    iconutil -c icns -o "${RESOURCES_DIR}/balun.icns" "data/balun.iconset"
    info "App icon created via iconutil."
fi

cat > "${APP_BUNDLE}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>    <string>${APP_NAME}</string>
  <key>CFBundleIdentifier</key>   <string>${BUNDLE_ID}</string>
  <key>CFBundleName</key>         <string>${APP_NAME}</string>
  <key>CFBundleVersion</key>      <string>${CARGO_VERSION}</string>
  <key>CFBundleShortVersionString</key> <string>${CARGO_VERSION}</string>
  <key>CFBundlePackageType</key>  <string>APPL</string>
  <key>CFBundleIconFile</key>     <string>balun</string>
  <key>NSHighResolutionCapable</key> <true/>
  <key>LSMinimumSystemVersion</key>  <string>13.0</string>
</dict>
</plist>
PLIST

# ── Dylib BFS and Rpath Rewriting ───────────────────────────────────────────
info "Bundling dylibs and rewriting rpaths via BFS..."

copy_dylib() {
    local src="$1"
    local basename
    basename="$(basename "$src")"
    local dest="${FRAMEWORKS_DIR}/${basename}"
    [[ -f "$dest" ]] && return 1
    cp -L "$src" "$dest"
    chmod u+w "$dest"
    install_name_tool -id "@executable_path/../Frameworks/${basename}" "$dest" 2>/dev/null || true
    return 0
}

resolve_dylib_source() {
    local libpath="$1"
    local bin="$2"
    local basename
    basename="$(basename "$libpath")"

    if [[ -f "$libpath" ]]; then
        printf '%s' "$libpath"
        return 0
    fi

    local candidate
    for candidate in \
        "${BREW_PREFIX}/lib/${basename}" \
        "${FRAMEWORKS_DIR}/${basename}" \
        "$(dirname "$bin")/${basename}" \
        "${BREW_PREFIX}/opt"/*/lib/"${basename}" \
        "${BREW_PREFIX}/Cellar"/*/*/lib/"${basename}" \
        "/opt/homebrew/lib/${basename}" \
        "/usr/local/lib/${basename}"
    do
        if [[ -f "$candidate" ]]; then
            printf '%s' "$candidate"
            return 0
        fi
    done

    local rpath_dir
    while IFS= read -r rpath_dir; do
        local resolved_rpath="$rpath_dir"
        resolved_rpath="${resolved_rpath//@loader_path/$(dirname "$bin")}"
        resolved_rpath="${resolved_rpath//@executable_path/${APP_BUNDLE}/Contents/MacOS}"
        if [[ -f "${resolved_rpath}/${basename}" ]]; then
            printf '%s' "${resolved_rpath}/${basename}"
            return 0
        fi
    done < <(otool -l "$bin" 2>/dev/null | awk '$1 == "path" {print $2}')

    return 1
}

fix_rpaths() {
    local bin="$1"
    local newly_found=()

    while IFS= read -r libpath; do
        local basename
        basename="$(basename "$libpath")"

        local src_path
        if src_path="$(resolve_dylib_source "$libpath" "$bin")"; then
            if copy_dylib "$src_path"; then
                newly_found+=("${FRAMEWORKS_DIR}/${basename}")
            fi
            install_name_tool -change "$libpath" \
                "@executable_path/../Frameworks/${basename}" "$bin" 2>/dev/null || true
        else
            warn "Could not resolve dylib source for $libpath referenced by $bin"
        fi
    done < <(otool -L "$bin" 2>/dev/null \
        | awk '/\/opt\/homebrew|\/usr\/local|@rpath\/|@loader_path\// {print $1}')

    NEWLY_COPIED=("${newly_found[@]+"${newly_found[@]}"}")
}

info "Seeding binaries and plugins for dylib resolution..."
SEED_BINARIES=()

BIN="${APP_BUNDLE}/Contents/MacOS/${APP_NAME}-bin"
[[ -f "$BIN" ]] && SEED_BINARIES+=("$BIN")

[[ -f "$GST_SCANNER_DEST" ]] && SEED_BINARIES+=("$GST_SCANNER_DEST")
[[ -f "$PIXBUF_QUERY_DEST" ]] && SEED_BINARIES+=("$PIXBUF_QUERY_DEST")

for plugin in "${GST_PLUGIN_DEST}"/*.dylib; do
    [[ -f "$plugin" ]] || continue
    chmod u+w "$plugin"
    install_name_tool -id "@rpath/$(basename "$plugin")" "$plugin" 2>/dev/null || true
    install_name_tool -add_rpath "@loader_path/../../../Frameworks" "$plugin" 2>/dev/null || true
    SEED_BINARIES+=("$plugin")
done

if [[ -d "$PIXBUF_LOADERS_DEST" ]]; then
    for loader in "${PIXBUF_LOADERS_DEST}"/*.so "${PIXBUF_LOADERS_DEST}"/*.dylib; do
        [[ -f "$loader" ]] || continue
        chmod u+w "$loader"
        install_name_tool -id "@rpath/$(basename "$loader")" "$loader" 2>/dev/null || true
        SEED_BINARIES+=("$loader")
    done
fi

QUEUE=()
for seed in "${SEED_BINARIES[@]}"; do
    fix_rpaths "$seed"
    if [[ ${#NEWLY_COPIED[@]} -gt 0 ]]; then
        QUEUE+=("${NEWLY_COPIED[@]}")
    fi
done

PASS=1
while [[ ${#QUEUE[@]} -gt 0 ]]; do
    info "  Dylib pass ${PASS}: processing ${#QUEUE[@]} libraries..."
    NEXT_QUEUE=()
    for lib in "${QUEUE[@]}"; do
        fix_rpaths "$lib"
        if [[ ${#NEWLY_COPIED[@]} -gt 0 ]]; then
            NEXT_QUEUE+=("${NEWLY_COPIED[@]}")
        fi
    done
    QUEUE=("${NEXT_QUEUE[@]+"${NEXT_QUEUE[@]}"}")
    PASS=$((PASS + 1))
    if [[ $PASS -gt 40 ]]; then
        warn "Dylib recursion exceeded 40 passes — stopping."
        break
    fi
done

info "Finalizing dylib install names and cross-references..."
for dylib in "${FRAMEWORKS_DIR}"/*.dylib; do
    [[ -f "$dylib" ]] || continue
    chmod u+w "$dylib"
    install_name_tool -id "@executable_path/../Frameworks/$(basename "$dylib")" "$dylib" 2>/dev/null || true
    while IFS= read -r libpath; do
        local dep_base
        dep_base="$(basename "$libpath")"
        if [[ -f "${FRAMEWORKS_DIR}/${dep_base}" ]]; then
            install_name_tool -change "$libpath" "@executable_path/../Frameworks/${dep_base}" "$dylib" 2>/dev/null || true
        fi
    done < <(otool -L "$dylib" 2>/dev/null | awk '/\/opt\/homebrew|\/usr\/local|@rpath\/|@loader_path\// {print $1}')
done

TOTAL_DYLIBS=$(ls -1 "${FRAMEWORKS_DIR}"/*.dylib 2>/dev/null | wc -l | tr -d ' ')
info "Bundled ${TOTAL_DYLIBS} dylibs into Frameworks/."

info "Verifying bundled library closure..."
missing_deps=0
for bin_check in "$BIN" "$GST_SCANNER_DEST" "$PIXBUF_QUERY_DEST" "${GST_PLUGIN_DEST}"/*.dylib "${FRAMEWORKS_DIR}"/*.dylib; do
    [[ -f "$bin_check" ]] || continue
    own_id=$(otool -D "$bin_check" 2>/dev/null | tail -1 | tr -d ' ' || true)
    while IFS= read -r dep; do
        [[ -n "$dep" ]] || continue
        if [[ -n "$own_id" && "$dep" == "$own_id" ]]; then
            continue
        fi
        case "$dep" in
            @executable_path/../Frameworks/*)
                target="${FRAMEWORKS_DIR}/$(basename "$dep")"
                if [[ ! -f "$target" ]]; then
                    warn "Missing bundled dylib dependency: $dep referenced by $(basename "$bin_check")"
                    missing_deps=$((missing_deps + 1))
                fi
                ;;
            /opt/homebrew/*|/usr/local/*)
                warn "Unpatched external dependency remains in $(basename "$bin_check"): $dep"
                missing_deps=$((missing_deps + 1))
                ;;
        esac
    done < <(otool -L "$bin_check" 2>/dev/null | awk 'NR > 1 {print $1}')
done
if [[ $missing_deps -gt 0 ]]; then
    fail "Bundled dylib closure has $missing_deps unsatisfied or unpatched dependencies."
fi
info "Bundled dylib closure verified with 0 unpatched or missing references."

rm -f "${RESOURCES_DIR}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
rm -f "${APP_BUNDLE}/Contents/MacOS/gst-registry.bin"

# ── Package Policy Validation ────────────────────────────────────────────────
info "Validating package against macOS copy-control policy..."
if ! macos_validate_bundle_copy_control "$APP_BUNDLE"; then
    fail "Application bundle failed macOS component policy: $MACOS_PACKAGE_POLICY_REASON"
fi
info "macOS component policy passed for $APP_BUNDLE."

# ── Ad-hoc Code Signing ─────────────────────────────────────────────────────
info "Ad-hoc code signing the bundle..."
while IFS= read -r dylib; do
    codesign --force --sign - "$dylib"
done < <(find "${FRAMEWORKS_DIR}" -name '*.dylib' -type f)

while IFS= read -r plugin; do
    codesign --force --sign - "$plugin"
done < <(find "${GST_PLUGIN_DEST}" -name '*.dylib' -type f)

if [[ -d "$PIXBUF_LOADERS_DEST" ]]; then
    while IFS= read -r loader; do
        codesign --force --sign - "$loader"
    done < <(find "$PIXBUF_LOADERS_DEST" \( -name '*.so' -o -name '*.dylib' \) -type f)
fi

codesign --force --sign - "$GST_SCANNER_DEST"
codesign --force --sign - "$PIXBUF_QUERY_DEST"
codesign --force --sign - "$BIN"
codesign --force --sign - "${BIN_DEST}"
codesign --force --deep --sign - "$APP_BUNDLE"

codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"
info "Code signature verified before runtime probe."

# ── Relocated Read-Only Runtime Probe ────────────────────────────────────────
PROBE_PARENT="dist/Balun Runtime Probe With Spaces"
PROBE_APP="${PROBE_PARENT}/${APP_NAME}.app"
PROBE_CACHE="$(mktemp -d "${TMPDIR:-/tmp}/Balun Runtime Cache With Spaces.XXXXXX")"
cleanup_probe() {
    chmod -R u+w "$PROBE_PARENT" 2>/dev/null || true
    rm -rf "$PROBE_PARENT" "$PROBE_CACHE"
}
trap cleanup_probe EXIT
rm -rf "$PROBE_PARENT"
mkdir -p "$PROBE_PARENT"
ditto "$APP_BUNDLE" "$PROBE_APP"
chmod -R a-w "$PROBE_APP"

info "Running relocated read-only runtime probe loopback..."
GST_REGISTRY="$PROBE_CACHE/gstreamer/registry.bin" \
env -u GST_REGISTRY_1_0 \
    -u GDK_PIXBUF_MODULE_FILE \
    -u GST_PLUGIN_PATH \
    -u GST_PLUGIN_PATH_1_0 \
    -u GST_PLUGIN_SYSTEM_PATH \
    -u GST_PLUGIN_SYSTEM_PATH_1_0 \
    -u GST_PLUGIN_SCANNER \
    -u GST_PLUGIN_SCANNER_1_0 \
    -u GDK_PIXBUF_MODULEDIR \
    -u XDG_DATA_DIRS \
    -u GTK_DATA_PREFIX \
    -u GSETTINGS_SCHEMA_DIR \
    -u GTK_PATH \
    -u DYLD_LIBRARY_PATH \
    "$PROBE_APP/Contents/MacOS/${APP_NAME}" \
    --balun-platform-runtime-probe "$PROBE_CACHE"

SENTINEL_FILE="$PROBE_CACHE/balun-platform-runtime-probe.ok"
[[ -f "$SENTINEL_FILE" ]] || fail "runtime probe did not write sentinel"
SENTINEL_CONTENT="$(cat "$SENTINEL_FILE")"
[[ "$SENTINEL_CONTENT" == "balun-macos-runtime-probe-v1" ]] || fail "runtime probe sentinel content mismatch: $SENTINEL_CONTENT"

PROBE_GST_CACHE="$(find "$PROBE_CACHE" -type f -name 'registry.bin' -print | sed -n '1p')"
[[ -n "$PROBE_GST_CACHE" ]] || fail "runtime probe did not create the GStreamer user cache"
if [[ -e "$PROBE_APP/Contents/MacOS/gst-registry.bin" \
   || -e "$PROBE_APP/Contents/Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache" ]]; then
    fail "runtime probe wrote a mutable cache inside the signed app"
fi
codesign --verify --deep --strict --verbose=2 "$PROBE_APP"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"
info "Signed runtime probe and final signature verification passed."
trap - EXIT
cleanup_probe

# ── DMG Creation ─────────────────────────────────────────────────────────────
if $make_dmg; then
    info "Creating .dmg disk image via create-dmg..."
    mkdir -p dist
    rm -f "dist/${APP_NAME}.dmg"
    create-dmg \
        --volname "${APP_NAME}" \
        --window-pos 200 120 \
        --window-size 600 400 \
        --icon-size 100 \
        --app-drop-link 450 185 \
        --skip-jenkins \
        "dist/${APP_NAME}.dmg" \
        "${APP_BUNDLE}"
    [ -s "dist/${APP_NAME}.dmg" ] || fail "create-dmg did not produce a non-empty DMG"
    info "DMG created: $(pwd)/dist/${APP_NAME}.dmg"
fi

info "Done."
