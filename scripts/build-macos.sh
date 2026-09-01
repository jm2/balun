#!/usr/bin/env bash
# Balun — macOS headless diagnostic build helper
#
# This intentionally stops at the current, reviewable Balun deliverable:
# balun-discover. It does not create an application bundle, disk image, native
# package, or staged runtime closure.

set -euo pipefail

usage()
{
    while IFS= read -r usage_line; do
        printf '%s\n' "$usage_line"
    done <<'EOF'
Balun — macOS headless diagnostic build helper.
A lightweight cross-platform HDHomeRun live TV viewer
Application ID: io.github.jm2.Balun

Usage:
  ./scripts/build-macos.sh [MODE]

With no mode, builds the native balun-discover executable with Cargo's locked
release dependency graph, then applies Balun's pinned Mach-O component policy.
This produces only target/release/balun-discover. It does not create Balun.app,
a DMG, a native package, or a staged runtime closure.

Quick-exit modes (choose at most one):
  --fmt             Run cargo fmt across the workspace.
  --check           Check all targets with the locked dependency graph.
  --clippy          Lint all targets with warnings denied and dependencies locked.
  --coverage        Print a GTK-free all-target coverage summary; requires
                    cargo-llvm-cov 0.8.7 to be installed already.

Unavailable until complete app-bundle recipes and final-artifact gates land:
  --dmg, --app, --bundle, --package, --pkg, --installer, --sign, --notarize

This helper never invokes Homebrew, another package manager, an installer, a
downloader, or runtime-copy staging.
Cargo may fetch locked dependencies unless cached.
A rustup-managed Cargo invocation may also fetch the selected Rust toolchain.
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

usage_error()
{
    printf '[balun] %s\n' "$*" >&2
    usage >&2
    exit 2
}

require_command()
{
    command -v "$1" >/dev/null 2>&1 || \
        fail "Required command '$1' is unavailable; install it explicitly and retry."
}

mode=build
mode_option=
show_help=false

# Reject every recognized package-producing route while argument parsing is
# still the only work performed. In particular, --help cannot mask --dmg.
for argument in "$@"; do
    case "$argument" in
        -h|--help)
            show_help=true
            ;;
        --fmt|--check|--clippy|--coverage)
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

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
binary="$repository_root/target/release/balun-discover"
policy_helper="$script_dir/macos-package-policy.sh"
policy_file="$repository_root/build-aux/packaging/forbidden-bundled-components.txt"
coverage_version='cargo-llvm-cov 0.8.7'
application_id='io.github.jm2.Balun'

cd "$repository_root"
require_command cargo

case "$mode" in
    fmt)
        info 'Formatting Balun...'
        cargo fmt --all
        info 'Formatting complete.'
        exit 0
        ;;
    check)
        info 'Checking all Balun targets with locked dependencies...'
        cargo check --all-targets --locked
        info 'Check passed.'
        exit 0
        ;;
    clippy)
        info 'Linting all Balun targets with locked dependencies...'
        cargo clippy --all-targets --locked -- -D warnings
        info 'Clippy passed.'
        exit 0
        ;;
    coverage)
        coverage_version_status=0
        installed_coverage_version=$(cargo llvm-cov --version 2>/dev/null) \
            || coverage_version_status=$?
        if [ "$coverage_version_status" -ne 0 ] \
            || [ "$installed_coverage_version" != "$coverage_version" ]; then
            fail "Coverage requires preinstalled $coverage_version exactly; this helper will not install or replace tools."
        fi
        info "Running informational coverage with $coverage_version..."
        cargo llvm-cov --all-targets --no-default-features --locked --summary-only
        exit 0
        ;;
    build)
        ;;
    *)
        fail "Internal error: unhandled build mode '$mode'."
        ;;
esac

host_system=$(uname -s 2>/dev/null || true)
[ "$host_system" = Darwin ] || \
    fail 'The default build route requires a native macOS host; no Cargo build was started.'

[ -f "$policy_helper" ] && [ ! -L "$policy_helper" ] || \
    fail "Required macOS package-policy helper is unavailable or unsafe: $policy_helper"
[ -f "$policy_file" ] && [ ! -L "$policy_file" ] || \
    fail "Pinned macOS component policy is unavailable or unsafe: $policy_file"

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

info 'Building balun-discover (native locked release)...'
cargo build --release --locked --bin balun-discover

[ -f "$binary" ] && [ ! -L "$binary" ] && [ -s "$binary" ] || \
    fail "Cargo did not produce the expected nonempty regular, non-symlink binary: $binary"

if ! macos_validate_macho_copy_control "$binary" false; then
    fail "balun-discover failed macOS Mach-O component-policy inspection: $MACOS_PACKAGE_POLICY_REASON"
fi
[ "$MACOS_PACKAGE_POLICY_RESULT" = allowed ] || \
    fail 'macOS Mach-O policy returned success without marking the diagnostic allowed.'

info "Application ID: $application_id"
info "Mach-O component policy passed for expected diagnostic path: $binary"
