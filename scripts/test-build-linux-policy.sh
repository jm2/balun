#!/usr/bin/env bash
# Deterministic command-routing tests for scripts/build-linux.sh. The fixture
# substitutes Cargo, rustc, pkg-config, native packagers, and both policy
# validators, so no compiler, package manager, installer, network access, or
# real artifact inspection is used.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
script_under_test="$script_dir/build-linux.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf -- "$temp_dir"' EXIT HUP INT TERM

fixture="$temp_dir/repository with spaces"
fake_bin="$fixture/fake-bin"
command_log="$temp_dir/commands.log"
hostile_target_directory="$temp_dir/hostile cargo target"
hostile_coverage_directory="$temp_dir/hostile coverage target"
hostile_build_target='aarch64-unknown-freebsd'
arch_recipe="$script_dir/../build-aux/arch/PKGBUILD"

mkdir -p \
    "$fixture/scripts" \
    "$fixture/build-aux/linux" \
    "$fixture/build-aux/arch" \
    "$fake_bin"
cp "$script_under_test" "$fixture/scripts/build-linux.sh"
cp "$arch_recipe" "$fixture/build-aux/arch/PKGBUILD"

cat > "$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'cargo' >> "$BALUN_TEST_LOG"
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
done
if [ "${1-}" = llvm-cov ] && [ "${2-}" != --version ]; then
    printf ' <CARGO_TARGET_DIR=%s>' "$CARGO_TARGET_DIR" >> "$BALUN_TEST_LOG"
    printf ' <CARGO_LLVM_COV_TARGET_DIR=%s>' \
        "$CARGO_LLVM_COV_TARGET_DIR" >> "$BALUN_TEST_LOG"
    printf ' <CARGO_LLVM_COV_BUILD_DIR=%s>' \
        "$CARGO_LLVM_COV_BUILD_DIR" >> "$BALUN_TEST_LOG"
fi
if [ "${1-}" = deb ]; then
    printf ' <CARGO_TARGET_DIR=%s>' "$CARGO_TARGET_DIR" >> "$BALUN_TEST_LOG"
fi
printf '\n' >> "$BALUN_TEST_LOG"

if [ "${1-}" = llvm-cov ] && [ "${2-}" = --version ]; then
    printf '%s\n' "$BALUN_FAKE_COVERAGE_VERSION"
fi

if [ "$BALUN_FAKE_CARGO_STATUS" -ne 0 ]; then
    exit "$BALUN_FAKE_CARGO_STATUS"
fi

case "${1-}" in
    deb|generate-rpm)
        if [ "$BALUN_FAKE_PACKAGER_STATUS" -ne 0 ]; then
            exit "$BALUN_FAKE_PACKAGER_STATUS"
        fi
        if [ "$BALUN_FAKE_SKIP_PACKAGE" -eq 0 ]; then
            package=
            previous_argument=
            for argument in "$@"; do
                if [ "$previous_argument" = --output ]; then
                    package=$argument
                fi
                previous_argument=$argument
            done
            [ -n "$package" ] || exit 96
            mkdir -p "$(dirname -- "$package")"
            printf 'synthetic package\n' > "$package"
        fi
        ;;
esac

if [ "${1-}" = build ] && [ "$BALUN_FAKE_SKIP_BINARY" -eq 0 ]; then
    binary_name=balun
    cargo_target_dir=$CARGO_TARGET_DIR
    cargo_build_target=$CARGO_BUILD_TARGET
    previous_argument=
    for argument in "$@"; do
        if [ "$previous_argument" = --target-dir ]; then
            cargo_target_dir=$argument
        fi
        if [ "$previous_argument" = --target ]; then
            cargo_build_target=$argument
        fi
        if [ "$argument" = balun-discover ]; then
            binary_name=balun-discover
        fi
        previous_argument=$argument
    done
    mkdir -p "$cargo_target_dir/$cargo_build_target/release"
    printf '%s\n' '#!/usr/bin/env bash' 'printf "%s launched\n" "${0##*/}"' > \
        "$cargo_target_dir/$cargo_build_target/release/$binary_name"
    chmod +x "$cargo_target_dir/$cargo_build_target/release/$binary_name"
fi
EOF

cat > "$fake_bin/makepkg" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'makepkg' >> "$BALUN_TEST_LOG"
packagelist=false
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
    [ "$argument" != --packagelist ] || packagelist=true
done
printf ' <PKGEXT=%s> <PKGDEST=%s> <BUILDDIR=%s>\n' \
    "$PKGEXT" "$PKGDEST" "$BUILDDIR" >> "$BALUN_TEST_LOG"

if [ "$BALUN_FAKE_PACKAGER_STATUS" -ne 0 ]; then
    exit "$BALUN_FAKE_PACKAGER_STATUS"
fi
if $packagelist; then
    printf '%s\n' "$BALUN_FAKE_ARCH_PACKAGE"
elif [ "$BALUN_FAKE_SKIP_PACKAGE" -eq 0 ]; then
    mkdir -p "$(dirname -- "$BALUN_FAKE_ARCH_PACKAGE")"
    printf 'synthetic Arch package\n' > "$BALUN_FAKE_ARCH_PACKAGE"
fi
EOF

for packaging_tool in cargo-deb cargo-generate-rpm; do
    cat > "$fake_bin/$packaging_tool" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

tool=${0##*/}
printf '%s' "$tool" >> "$BALUN_TEST_LOG"
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
done
printf '\n' >> "$BALUN_TEST_LOG"

case "$tool" in
    cargo-deb) printf '%s\n' "$BALUN_FAKE_CARGO_DEB_VERSION" ;;
    cargo-generate-rpm) printf '%s\n' "$BALUN_FAKE_CARGO_GENERATE_RPM_VERSION" ;;
esac
EOF
done
for inspector in bsdtar dpkg-deb rpm rpm2cpio cpio; do
    cat > "$fake_bin/$inspector" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
done

cat > "$fake_bin/rustc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'rustc' >> "$BALUN_TEST_LOG"
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
done
printf '\n' >> "$BALUN_TEST_LOG"

if [ "$BALUN_FAKE_RUSTC_STATUS" -ne 0 ]; then
    exit "$BALUN_FAKE_RUSTC_STATUS"
fi
if [ "${1-}" = --print ] && [ "${2-}" = host-tuple ]; then
    printf '%s\n' "$BALUN_FAKE_RUSTC_TARGET"
fi
EOF

cat > "$fake_bin/pkg-config" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'pkg-config' >> "$BALUN_TEST_LOG"
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
done
printf '\n' >> "$BALUN_TEST_LOG"

if [ "${2-}" = "$BALUN_FAKE_PKG_CONFIG_FAILURE" ]; then
    exit 1
fi
if [ "${1-}" = --variable=pluginsdir ]; then
    printf '%s\n' "$BALUN_FAKE_PLUGIN_DIRECTORY"
fi
EOF

plugin_directory="$fixture/gstreamer-plugins"
mkdir -p "$plugin_directory"
for plugin in libgstcoreelements libgstplayback libgstapp libgsttypefindfunctions \
    libgstdeinterlace libgstmpegtsdemux libgstgtk4 libgstlibav; do
    : > "$plugin_directory/$plugin.so"
done

cat > "$fake_bin/readelf" <<'EOF'
#!/usr/bin/env bash
printf 'readelf unexpectedly executed\n' >> "$BALUN_TEST_LOG"
exit 97
EOF

cat > "$fixture/build-aux/linux/validate-package-metadata.sh" <<'EOF'
#!/usr/bin/env bash
printf 'metadata\n' >> "$BALUN_TEST_LOG"
exit "$BALUN_FAKE_METADATA_STATUS"
EOF

cat > "$fixture/build-aux/linux/validate-package-compliance.sh" <<'EOF'
#!/usr/bin/env bash
printf 'compliance' >> "$BALUN_TEST_LOG"
for argument in "$@"; do
    printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
done
printf '\n' >> "$BALUN_TEST_LOG"
if [ "${1-}" = --elf ]; then
    exit "$BALUN_FAKE_ARTIFACT_STATUS"
fi
exit "$BALUN_FAKE_PACKAGE_ARTIFACT_STATUS"
EOF

chmod +x \
    "$fixture/scripts/build-linux.sh" \
    "$fake_bin/cargo" \
    "$fake_bin/cargo-deb" \
    "$fake_bin/cargo-generate-rpm" \
    "$fake_bin/makepkg" \
    "$fake_bin/bsdtar" \
    "$fake_bin/dpkg-deb" \
    "$fake_bin/rpm" \
    "$fake_bin/rpm2cpio" \
    "$fake_bin/cpio" \
    "$fake_bin/rustc" \
    "$fake_bin/pkg-config" \
    "$fake_bin/readelf" \
    "$fixture/build-aux/linux/validate-package-metadata.sh" \
    "$fixture/build-aux/linux/validate-package-compliance.sh"

# Give the helper only the external commands its current, reviewed routes need.
# A future direct downloader or package-manager invocation therefore cannot
# silently reach a host tool before the static policy check below catches it.
for utility in bash cat chmod cp dirname mkdir; do
    utility_path=$(command -v "$utility")
    [ -n "$utility_path" ] || {
        printf 'build-linux policy test requires %s\n' "$utility" >&2
        exit 2
    }
    ln -s "$utility_path" "$fake_bin/$utility"
done

fake_coverage_version='cargo-llvm-cov 0.8.7'
fake_cargo_deb_version='cargo-deb 3.7.0'
fake_cargo_generate_rpm_version='cargo-generate-rpm 0.21.0'
fake_cargo_status=0
fake_rustc_status=0
valid_native_target='x86_64-unknown-linux-gnu'
fake_rustc_target=$valid_native_target
native_release_directory="$fixture/target/$valid_native_target/release"
fake_pkg_config_failure=
fake_plugin_directory=$plugin_directory
fake_metadata_status=0
fake_artifact_status=0
fake_package_artifact_status=0
fake_packager_status=0
fake_skip_package=0
fake_arch_package="$fixture/target/arch/balun-0.1.0-1-x86_64.pkg.tar.zst"
fake_skip_binary=0
status=0
output=

run_helper()
{
    : > "$command_log"
    set +e
    output=$(
        cd "$fixture"
        PATH="$fake_bin" \
        CARGO_TARGET_DIR="$hostile_target_directory" \
        CARGO_BUILD_TARGET="$hostile_build_target" \
        CARGO_LLVM_COV_TARGET_DIR="$hostile_coverage_directory" \
        CARGO_LLVM_COV_BUILD_DIR="$hostile_coverage_directory" \
        BALUN_TEST_LOG="$command_log" \
        BALUN_FAKE_COVERAGE_VERSION="$fake_coverage_version" \
        BALUN_FAKE_CARGO_DEB_VERSION="$fake_cargo_deb_version" \
        BALUN_FAKE_CARGO_GENERATE_RPM_VERSION="$fake_cargo_generate_rpm_version" \
        BALUN_FAKE_CARGO_STATUS="$fake_cargo_status" \
        BALUN_FAKE_RUSTC_STATUS="$fake_rustc_status" \
        BALUN_FAKE_RUSTC_TARGET="$fake_rustc_target" \
        BALUN_FAKE_PKG_CONFIG_FAILURE="$fake_pkg_config_failure" \
        BALUN_FAKE_PLUGIN_DIRECTORY="$fake_plugin_directory" \
        BALUN_FAKE_METADATA_STATUS="$fake_metadata_status" \
        BALUN_FAKE_ARTIFACT_STATUS="$fake_artifact_status" \
        BALUN_FAKE_PACKAGE_ARTIFACT_STATUS="$fake_package_artifact_status" \
        BALUN_FAKE_PACKAGER_STATUS="$fake_packager_status" \
        BALUN_FAKE_SKIP_PACKAGE="$fake_skip_package" \
        BALUN_FAKE_ARCH_PACKAGE="$fake_arch_package" \
        BALUN_FAKE_SKIP_BINARY="$fake_skip_binary" \
            /bin/bash "$fixture/scripts/build-linux.sh" "$@" 2>&1
    )
    status=$?
    set -e
}

fail_test()
{
    printf 'build-linux policy test failed: %s\n' "$*" >&2
    printf 'status: %s\noutput:\n%s\ncommands:\n' "$status" "$output" >&2
    sed -n '1,80p' "$command_log" >&2
    exit 1
}

expect_status()
{
    [ "$status" -eq "$1" ] || fail_test "expected status $1"
}

expect_output()
{
    case "$output" in
        *"$1"*) ;;
        *) fail_test "expected output containing: $1" ;;
    esac
}

expect_log()
{
    expected=$1
    actual=$(cat "$command_log")
    [ "$actual" = "$expected" ] || fail_test "unexpected command routing"
}

expect_empty_log()
{
    [ ! -s "$command_log" ] || fail_test "expected no external build or policy commands"
}

desktop_metadata_log()
{
    printf '%s\n' \
        'rustc <--print> <host-tuple>' \
        'pkg-config <--atleast-version=4.16> <gtk4>' \
        'pkg-config <--atleast-version=1.6> <libadwaita-1>' \
        'pkg-config <--atleast-version=1.20> <gstreamer-1.0>' \
        'pkg-config <--variable=pluginsdir> <gstreamer-1.0>' \
        'metadata'
}

desktop_package_log()
{
    local target=$1
    printf '%s\n' \
        "$(desktop_metadata_log)" \
        "cargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <$fixture/target> <--target> <$target>" \
        "compliance <--elf> <$fixture/target/$target/release/balun>"
}

run_helper --help
expect_status 0
expect_output 'A lightweight cross-platform HDHomeRun live TV viewer'
expect_output 'Application ID: io.github.jm2.Balun'
expect_output 'Linux desktop build helper'
expect_output 'builds only unless --run is'
expect_output '--diagnostic'
expect_output 'cargo-deb'
expect_output '3.7.0'
expect_output 'cargo-generate-rpm 0.21.0'
expect_output 'This helper never invokes tool or package installers.'
expect_output 'Cargo may fetch locked dependencies unless cached.'
expect_output 'may also fetch the selected Rust toolchain.'
expect_empty_log

run_helper --not-a-mode
expect_status 2
expect_output 'Unknown option: --not-a-mode'
expect_empty_log

# --run launches only the plain desktop build; every other selection conflicts
# before any build or desktop dependency is resolved.
run_helper --run --check
expect_status 2
expect_output "--run cannot be combined with '--check'"
expect_empty_log

run_helper --run --diagnostic
expect_status 2
expect_output '--run launches only the desktop application and cannot be combined with --diagnostic'
expect_empty_log

run_helper --deb --run
expect_status 2
expect_output "--run cannot be combined with '--deb'"
expect_empty_log

run_helper --flatpak
expect_status 2
expect_output "Packaging mode '--flatpak' is not available yet"
expect_output 'no build, install, or network work was started'
expect_empty_log

run_helper --help --flatpak
expect_status 2
expect_output "Packaging mode '--flatpak' is not available yet"
expect_empty_log

for package_mode in --deb --rpm --arch-pkg; do
    run_helper --help "$package_mode"
    expect_status 0
    expect_output "$package_mode"
    expect_empty_log
done

run_helper --fmt --check
expect_status 2
expect_output 'Quick-exit modes cannot be combined'
expect_empty_log

run_helper --probe-playback --check
expect_status 2
expect_output 'Quick-exit modes cannot be combined'
expect_empty_log

run_helper --diagnostic --probe-playback
expect_status 2
expect_output '--probe-playback exercises the desktop playback runtime and cannot be combined with --diagnostic'
expect_empty_log

run_helper --deb --rpm
expect_status 2
expect_output "Packaging modes cannot be combined ('--deb' and '--rpm')"
expect_empty_log

run_helper --deb --check
expect_status 2
expect_output "Quick-exit modes cannot be combined ('--deb' and '--check')"
expect_empty_log

run_helper --check --deb
expect_status 2
expect_output "Packaging modes cannot be combined ('--check' and '--deb')"
expect_empty_log

for package_mode in --deb --rpm --arch-pkg; do
    run_helper --diagnostic "$package_mode"
    expect_status 2
    expect_output 'Native package modes build the desktop application and cannot be combined with --diagnostic'
    expect_empty_log
done

metadata_validator="$fixture/build-aux/linux/validate-package-metadata.sh"
artifact_validator="$fixture/build-aux/linux/validate-package-compliance.sh"

mv "$metadata_validator" "$metadata_validator.saved"
run_helper
expect_status 1
expect_output 'Required repository metadata validator is unavailable or not executable'
expect_empty_log
mv "$metadata_validator.saved" "$metadata_validator"

chmod -x "$metadata_validator"
run_helper
expect_status 1
expect_output 'Required repository metadata validator is unavailable or not executable'
expect_empty_log
chmod +x "$metadata_validator"

mv "$artifact_validator" "$artifact_validator.saved"
run_helper
expect_status 1
expect_output 'Required Linux artifact validator is unavailable or not executable'
expect_empty_log
mv "$artifact_validator.saved" "$artifact_validator"

chmod -x "$artifact_validator"
run_helper
expect_status 1
expect_output 'Required Linux artifact validator is unavailable or not executable'
expect_empty_log
chmod +x "$artifact_validator"

mv "$fake_bin/readelf" "$fake_bin/readelf.saved"
run_helper
expect_status 1
expect_output "Required command 'readelf' is unavailable"
expect_empty_log
mv "$fake_bin/readelf.saved" "$fake_bin/readelf"

for tool_and_mode in \
    'cargo-deb --deb' \
    'dpkg-deb --deb' \
    'cargo-generate-rpm --rpm' \
    'rpm --rpm' \
    'rpm2cpio --rpm' \
    'cpio --rpm' \
    'makepkg --arch-pkg' \
    'bsdtar --arch-pkg'
do
    tool=${tool_and_mode%% *}
    package_mode=${tool_and_mode#* }
    mv "$fake_bin/$tool" "$fake_bin/$tool.saved"
    run_helper "$package_mode"
    expect_status 1
    expect_output "Required command '$tool' is unavailable"
    expect_empty_log
    mv "$fake_bin/$tool.saved" "$fake_bin/$tool"
done

fake_cargo_deb_version='cargo-deb 9.9.9'
run_helper --deb
expect_status 1
expect_output 'requires preinstalled cargo-deb 3.7.0 exactly'
expect_output 'will not install or replace tools'
expect_log 'cargo-deb <--version>'
fake_cargo_deb_version='cargo-deb 3.7.0'

fake_cargo_generate_rpm_version='cargo-generate-rpm 9.9.9'
run_helper --rpm
expect_status 1
expect_output 'requires preinstalled cargo-generate-rpm 0.21.0 exactly'
expect_output 'will not install or replace tools'
expect_log 'cargo-generate-rpm <--version>'
fake_cargo_generate_rpm_version='cargo-generate-rpm 0.21.0'

mv "$fake_bin/rustc" "$fake_bin/rustc.saved"
run_helper
expect_status 1
expect_output "Required command 'rustc' is unavailable"
expect_empty_log
mv "$fake_bin/rustc.saved" "$fake_bin/rustc"

fake_rustc_status=19
run_helper --check
expect_status 1
expect_output 'rustc did not report one bounded native Linux host target'
expect_log 'rustc <--print> <host-tuple>'
fake_rustc_status=0

fake_rustc_target='x86_64-apple-darwin'
run_helper --check
expect_status 1
expect_output 'rustc did not report one bounded native Linux host target'
expect_log 'rustc <--print> <host-tuple>'

fake_rustc_target=$'x86_64-unknown-linux-gnu\nsecond-line'
run_helper --check
expect_status 1
expect_output 'rustc did not report one bounded native Linux host target'
expect_log 'rustc <--print> <host-tuple>'

printf -v overlong_target_component '%0130d' 0
fake_rustc_target="x86_64-$overlong_target_component-linux-gnu"
run_helper --check
expect_status 1
expect_output 'rustc did not report one bounded native Linux host target'
expect_log 'rustc <--print> <host-tuple>'
fake_rustc_target=$valid_native_target

mv "$fake_bin/pkg-config" "$fake_bin/pkg-config.saved"
run_helper
expect_status 1
expect_output "Required command 'pkg-config' is unavailable"
expect_log 'rustc <--print> <host-tuple>'
mv "$fake_bin/pkg-config.saved" "$fake_bin/pkg-config"

fake_pkg_config_failure=gtk4
run_helper --check
expect_status 1
expect_output 'gtk4 >= 4.16 was not found through pkg-config'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>'

fake_pkg_config_failure=libadwaita-1
run_helper --check
expect_status 1
expect_output 'libadwaita-1 >= 1.6 was not found through pkg-config'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>'

fake_pkg_config_failure=gstreamer-1.0
run_helper --check
expect_status 1
expect_output 'gstreamer-1.0 >= 1.20 was not found through pkg-config'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>'

run_helper --diagnostic --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_pkg_config_failure=

run_helper --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'

run_helper --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

run_helper --diagnostic --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

run_helper --clippy
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <clippy> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>\ncargo <clippy> <--release> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>'

run_helper --diagnostic --clippy
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <clippy> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>\ncargo <clippy> <--release> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>'

run_helper --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--all-features> <--locked> <--target> <'"$valid_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

run_helper --diagnostic --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--no-default-features> <--locked> <--target> <'"$valid_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

fake_coverage_version='cargo-llvm-cov 9.9.9'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_output 'will not install or replace tools'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

run_helper
expect_status 0
expect_output 'Desktop output:'
expect_output 'GStreamer runtime plugin checks passed'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun>'
[ ! -e "$hostile_target_directory/$valid_native_target/release/balun" ] || \
    fail_test 'hostile CARGO_TARGET_DIR received the desktop output'
[ ! -e "$fixture/target/$hostile_build_target/release/balun" ] || \
    fail_test 'hostile CARGO_BUILD_TARGET received the desktop output'

# --run replaces the helper with the built desktop after the same gates.
run_helper --run
expect_status 0
expect_output 'Desktop output:'
expect_output 'Launching Balun desktop'
expect_output 'balun launched'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun>'

# Every native package route is architecture-bounded, reuses the reviewed
# desktop build, and hands the completed archive to the existing validator.
run_helper --deb
expect_status 0
expect_output "Debian package: $fixture/dist/balun-amd64.deb"
expect_log 'cargo-deb <--version>'$'\n'"$(desktop_package_log "$valid_native_target")"$'\n'"cargo <deb> <--locked> <--no-build> <--target> <$valid_native_target> <--output> <$fixture/dist/balun-amd64.deb> <CARGO_TARGET_DIR=$fixture/target>"$'\n'"compliance <--deb> <$fixture/dist/balun-amd64.deb>"

run_helper --rpm
expect_status 0
expect_output "RPM package: $fixture/dist/balun-x86_64.rpm"
expect_log 'cargo-generate-rpm <--version>'$'\n'"$(desktop_package_log "$valid_native_target")"$'\n'"cargo <generate-rpm> <--target-dir> <$fixture/target> <--target> <$valid_native_target> <--output> <$fixture/dist/balun-x86_64.rpm>"$'\n'"compliance <--rpm> <$fixture/dist/balun-x86_64.rpm>"

run_helper --arch-pkg
expect_status 0
expect_output "Arch package: $fixture/dist/balun-x86_64.pkg.tar.zst"
expect_log "$(desktop_metadata_log)"$'\n'"makepkg <--dir> <$fixture/build-aux/arch> <--packagelist> <PKGEXT=.pkg.tar.zst> <PKGDEST=$fixture/target/arch> <BUILDDIR=$fixture/target/arch/build>"$'\n'"makepkg <--dir> <$fixture/build-aux/arch> <--force> <--clean> <--noconfirm> <PKGEXT=.pkg.tar.zst> <PKGDEST=$fixture/target/arch> <BUILDDIR=$fixture/target/arch/build>"$'\n'"compliance <--arch> <$fixture/dist/balun-x86_64.pkg.tar.zst>"

arm_native_target='aarch64-unknown-linux-gnu'
fake_rustc_target=$arm_native_target
run_helper --deb
expect_status 0
expect_output "Debian package: $fixture/dist/balun-arm64.deb"
expect_log 'cargo-deb <--version>'$'\n'"$(desktop_package_log "$arm_native_target")"$'\n'"cargo <deb> <--locked> <--no-build> <--target> <$arm_native_target> <--output> <$fixture/dist/balun-arm64.deb> <CARGO_TARGET_DIR=$fixture/target>"$'\n'"compliance <--deb> <$fixture/dist/balun-arm64.deb>"

run_helper --rpm
expect_status 0
expect_output "RPM package: $fixture/dist/balun-aarch64.rpm"
expect_log 'cargo-generate-rpm <--version>'$'\n'"$(desktop_package_log "$arm_native_target")"$'\n'"cargo <generate-rpm> <--target-dir> <$fixture/target> <--target> <$arm_native_target> <--output> <$fixture/dist/balun-aarch64.rpm>"$'\n'"compliance <--rpm> <$fixture/dist/balun-aarch64.rpm>"

run_helper --arch-pkg
expect_status 1
expect_output "supports only an x86_64 GNU/Linux host; rustc reported $arm_native_target"
expect_log 'rustc <--print> <host-tuple>'

fake_rustc_target='x86_64-unknown-linux-musl'
run_helper --deb
expect_status 1
expect_output "supports only x86_64 and aarch64 GNU/Linux hosts; rustc reported $fake_rustc_target"
expect_log $'cargo-deb <--version>\nrustc <--print> <host-tuple>'
fake_rustc_target=$valid_native_target

rm -f -- "$fixture/dist/balun-amd64.deb"
fake_skip_package=1
run_helper --deb
expect_status 1
expect_output 'cargo-deb did not produce the expected nonempty, regular, non-symlink package'
expect_log 'cargo-deb <--version>'$'\n'"$(desktop_package_log "$valid_native_target")"$'\n'"cargo <deb> <--locked> <--no-build> <--target> <$valid_native_target> <--output> <$fixture/dist/balun-amd64.deb> <CARGO_TARGET_DIR=$fixture/target>"
fake_skip_package=0

fake_packager_status=31
run_helper --rpm
expect_status 31
expect_log 'cargo-generate-rpm <--version>'$'\n'"$(desktop_package_log "$valid_native_target")"$'\n'"cargo <generate-rpm> <--target-dir> <$fixture/target> <--target> <$valid_native_target> <--output> <$fixture/dist/balun-x86_64.rpm>"
fake_packager_status=0

fake_package_artifact_status=32
run_helper --deb
expect_status 32
expect_log 'cargo-deb <--version>'$'\n'"$(desktop_package_log "$valid_native_target")"$'\n'"cargo <deb> <--locked> <--no-build> <--target> <$valid_native_target> <--output> <$fixture/dist/balun-amd64.deb> <CARGO_TARGET_DIR=$fixture/target>"$'\n'"compliance <--deb> <$fixture/dist/balun-amd64.deb>"
fake_package_artifact_status=0

fake_arch_package="$fixture/outside/balun-0.1.0-1-x86_64.pkg.tar.zst"
run_helper --arch-pkg
expect_status 1
expect_output 'makepkg reported an output outside the reviewed build directory'
expect_log "$(desktop_metadata_log)"$'\n'"makepkg <--dir> <$fixture/build-aux/arch> <--packagelist> <PKGEXT=.pkg.tar.zst> <PKGDEST=$fixture/target/arch> <BUILDDIR=$fixture/target/arch/build>"
fake_arch_package="$fixture/target/arch/balun-0.1.0-1-x86_64.pkg.tar.zst"

run_helper --probe-playback
expect_status 0
expect_output 'Playback runtime probes passed'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\ncargo <test> <--release> <--locked> <--features> <desktop> <--lib> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <playback::runtime::tests::installed_runtime_has_the_exact_playback_foundation> <--> <--ignored> <--exact> <--nocapture>\ncargo <test> <--release> <--locked> <--features> <desktop> <--lib> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <playback::source_policy::tests::installed_runtime_maps_the_constant_uri_to_exact_appsrc> <--> <--ignored> <--exact> <--nocapture>\ncargo <test> <--release> <--locked> <--features> <desktop> <--lib> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <playback::runtime::tests::installed_runtime_reports_the_decoder_and_sink_inventory> <--> <--ignored> <--exact> <--nocapture>'

# The desktop build and the runtime probes fail closed before any Cargo work
# when a structural runtime plugin is missing; other quick modes never
# consult runtime plugins.
rm -f -- "$plugin_directory/libgstgtk4.so"
run_helper --probe-playback
expect_status 1
expect_output 'Required GStreamer playback runtime is incomplete'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>'
run_helper
expect_status 1
expect_output 'Required GStreamer playback runtime is incomplete'
expect_output 'libgstgtk4.so (gtk4paintablesink) from gstreamer1-plugin-gtk4'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>'
run_helper --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
run_helper --diagnostic
expect_status 0
expect_output 'Diagnostic output:'
: > "$plugin_directory/libgstgtk4.so"

rm -f -- "$plugin_directory/libgstmpegtsdemux.so" "$plugin_directory/libgstplayback.so"
run_helper
expect_status 1
expect_output 'libgstplayback.so (playbin3, uridecodebin3, decodebin3) from gstreamer1-plugins-base'
expect_output 'libgstmpegtsdemux.so (tsdemux) from gstreamer1-plugins-bad-free'
: > "$plugin_directory/libgstmpegtsdemux.so"
: > "$plugin_directory/libgstplayback.so"

rm -f -- "$plugin_directory/libgstlibav.so"
run_helper
expect_status 0
expect_output 'warning: libgstlibav.so is missing'
expect_output 'Desktop output:'
: > "$plugin_directory/libgstlibav.so"

fake_plugin_directory="$fixture/missing-plugins"
run_helper
expect_status 1
expect_output 'did not report an existing GStreamer plugin directory'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>'
fake_plugin_directory=$plugin_directory

run_helper --diagnostic
expect_status 0
expect_output 'Diagnostic output:'
expect_log $'rustc <--print> <host-tuple>\nmetadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun-discover>'
[ ! -e "$hostile_target_directory/$valid_native_target/release/balun-discover" ] || \
    fail_test 'hostile CARGO_TARGET_DIR received the diagnostic output'
[ ! -e "$fixture/target/$hostile_build_target/release/balun-discover" ] || \
    fail_test 'hostile CARGO_BUILD_TARGET received the diagnostic output'

fake_metadata_status=23
run_helper
expect_status 23
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata'
fake_metadata_status=0

fake_cargo_status=24
run_helper
expect_status 24
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_cargo_status=0

rm -f -- "$native_release_directory/balun"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_skip_binary=0

rm -f -- "$native_release_directory/balun"
: > "$temp_dir/outside-symlink-target"
ln -s "$temp_dir/outside-symlink-target" \
    "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rm -f -- "$native_release_directory/balun"

: > "$native_release_directory/balun"
chmod +x "$native_release_directory/balun"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

rm -f -- "$native_release_directory/balun"
mkdir "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rmdir "$native_release_directory/balun"

mkfifo "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rm -f -- "$native_release_directory/balun"

printf 'synthetic ELF\n' > "$native_release_directory/balun"
chmod -x "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_skip_binary=0
rm -f -- "$native_release_directory/balun"

fake_artifact_status=25
run_helper
expect_status 25
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun>'

forbidden_command_pattern='^[[:space:]]*([^[:space:]]*/)?(sudo|curl|wget|git|rustup|apt|apt-get|dnf|yum|pacman|zypper|apk|snap|flatpak|flatpak-builder|brew|port|rpm|dpkg)([[:space:]]|$)'
forbidden_cargo_install_pattern='^[[:space:]]*([^[:space:]]*/)?cargo[[:space:]]+(\+[^[:space:]]+[[:space:]]+)?install([[:space:]]|$)'

contains_forbidden_invocation()
{
    grep -Eq \
        "$forbidden_command_pattern|$forbidden_cargo_install_pattern" "$1"
}

for invocation in \
    'curl https://example.invalid/archive' \
    '/usr/bin/wget https://example.invalid/archive' \
    'git clone https://example.invalid/repository' \
    'cargo install cargo-example --locked' \
    'cargo +stable install cargo-example --locked' \
    'sudo apt-get install example' \
    'dnf install example' \
    'flatpak-builder --install-deps-from=flathub build manifest.yml'
do
    printf '%s\n' "$invocation" > "$temp_dir/forbidden-invocation"
    contains_forbidden_invocation "$temp_dir/forbidden-invocation" || \
        fail_test "static policy missed direct invocation: $invocation"
done

if contains_forbidden_invocation "$script_under_test"; then
    fail_test 'helper contains a direct downloader, git, installer, or package-manager command'
fi
if grep -Eq 'makepkg[[:space:]].*[[:space:]](-s|--syncdeps)([[:space:]]|$)' \
    "$script_under_test"; then
    fail_test 'helper asks makepkg to install build dependencies'
fi

printf 'build-linux policy tests passed\n'
