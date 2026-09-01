#!/usr/bin/env bash
# Balun — Linux headless diagnostic build helper
#
# This intentionally stops at the current, reviewable Balun deliverable:
# balun-discover. GUI and native/Flatpak package modes remain unavailable until
# their recipes, assets, runtime closure, and artifact gates land together.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
metadata_validator="$repository_root/build-aux/linux/validate-package-metadata.sh"
artifact_validator="$repository_root/build-aux/linux/validate-package-compliance.sh"
binary="$repository_root/target/release/balun-discover"
coverage_version="cargo-llvm-cov 0.8.7"

usage()
{
    cat <<'EOF'
Balun — Linux headless build helper.
A lightweight cross-platform HDHomeRun live TV viewer

Usage:
  ./scripts/build-linux.sh [MODE]

With no mode, builds balun-discover with Cargo's locked release dependency
graph, then applies Balun's repository-metadata and Linux ELF policy gates.

Quick-exit modes (choose at most one):
  --fmt             Run cargo fmt across the workspace.
  --check           Check all targets with the locked dependency graph.
  --clippy          Lint all targets with warnings denied and dependencies locked.
  --coverage        Print an all-target/all-feature coverage summary; requires
                    cargo-llvm-cov 0.8.7 to be installed already.

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

cd "$repository_root"
require_command cargo

case "$mode" in
    fmt)
        info "Formatting Balun..."
        cargo fmt --all
        info "Formatting complete."
        exit 0
        ;;
    check)
        info "Checking all Balun targets with locked dependencies..."
        cargo check --all-targets --locked
        info "Check passed."
        exit 0
        ;;
    clippy)
        info "Linting all Balun targets with locked dependencies..."
        cargo clippy --all-targets --locked -- -D warnings
        info "Clippy passed."
        exit 0
        ;;
    coverage)
        installed_coverage_version=$(cargo llvm-cov --version 2>/dev/null || true)
        if [ "$installed_coverage_version" != "$coverage_version" ]; then
            fail "Coverage requires preinstalled $coverage_version exactly; this helper will not install or replace tools."
        fi
        info "Running informational coverage with $coverage_version..."
        cargo llvm-cov --all-targets --all-features --locked --summary-only
        exit 0
        ;;
    build)
        ;;
    *)
        fail "Internal error: unhandled build mode '$mode'."
        ;;
esac

[ -x "$metadata_validator" ] || \
    fail "Required repository metadata validator is unavailable or not executable: $metadata_validator"
[ -x "$artifact_validator" ] || \
    fail "Required Linux artifact validator is unavailable or not executable: $artifact_validator"
require_command readelf

info "Validating locked repository metadata..."
"$metadata_validator"

info "Building balun-discover (locked release)..."
cargo build --release --locked --bin balun-discover

[ -f "$binary" ] && [ ! -L "$binary" ] || \
    fail "Cargo did not produce the expected regular, non-symlink binary: $binary"

info "Validating balun-discover Linux ELF policy..."
"$artifact_validator" --elf "$binary"
info "Binary: $binary"
