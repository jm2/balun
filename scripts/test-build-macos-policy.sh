#!/usr/bin/env bash
# Deterministic command-routing tests for scripts/build-macos.sh. The fixture
# substitutes Cargo, rustc, and the policy helper, so no compiler, package
# manager, installer, network access, or real Mach-O inspection is used.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
script_under_test="$script_dir/build-macos.sh"
temp_dir=$(mktemp -d)
# macOS exposes /var through /private/var. Match the helper's physical-path
# normalization so exact routing assertions remain stable across that alias.
temp_dir=$(CDPATH= cd -- "$temp_dir" && pwd -P)
trap 'rm -rf -- "$temp_dir"' EXIT HUP INT TERM

fixture="$temp_dir/repository with spaces"
fake_bin="$fixture/fake-bin"
command_log="$temp_dir/commands.log"
hostile_target_directory="$temp_dir/hostile cargo target"
hostile_coverage_directory="$temp_dir/hostile coverage target"
hostile_build_target=wasm32-unknown-unknown

mkdir -p \
    "$fixture/scripts" \
    "$fixture/build-aux/packaging" \
    "$fake_bin"
cp "$script_under_test" "$fixture/scripts/build-macos.sh"
: > "$fixture/build-aux/packaging/forbidden-bundled-components.txt"

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
printf '\n' >> "$BALUN_TEST_LOG"

if [ "${1-}" = llvm-cov ] && [ "${2-}" = --version ]; then
    printf '%s\n' "$BALUN_FAKE_COVERAGE_VERSION"
fi

if [ "$BALUN_FAKE_CARGO_STATUS" -ne 0 ]; then
    exit "$BALUN_FAKE_CARGO_STATUS"
fi

if [ "${1-}" = build ] && [ "$BALUN_FAKE_SKIP_BINARY" -eq 0 ]; then
    output_name=
    cargo_target_dir=$CARGO_TARGET_DIR
    cargo_build_target=$CARGO_BUILD_TARGET
    previous_argument=
    for argument in "$@"; do
        if [ "$previous_argument" = --bin ]; then
            output_name=$argument
        fi
        if [ "$previous_argument" = --target-dir ]; then
            cargo_target_dir=$argument
        fi
        if [ "$previous_argument" = --target ]; then
            cargo_build_target=$argument
        fi
        previous_argument=$argument
    done
    [ -n "$output_name" ] || exit 98
    mkdir -p "$cargo_target_dir/$cargo_build_target/release"
    if [ "$BALUN_FAKE_EMPTY_BINARY" -eq 1 ]; then
        : > "$cargo_target_dir/$cargo_build_target/release/$output_name"
    else
        printf 'synthetic Mach-O\n' \
            > "$cargo_target_dir/$cargo_build_target/release/$output_name"
    fi
    chmod +x "$cargo_target_dir/$cargo_build_target/release/$output_name"
fi
EOF

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
printf '%s\n' "$BALUN_FAKE_RUSTC_TARGET"
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
    : > "$plugin_directory/$plugin.dylib"
done

cat > "$fake_bin/uname" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$BALUN_FAKE_HOST_SYSTEM"
EOF

cat > "$fixture/scripts/macos-package-policy.sh" <<'EOF'
#!/usr/bin/env bash
MACOS_PACKAGE_POLICY_REASON=''
MACOS_PACKAGE_POLICY_RESULT=''
MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0

macos_package_policy_load()
{
    printf 'policy-load <%s> sha <%s> perl <%s> otool <%s>\n' \
        "$1" "$MACOS_SHA256_COMMAND" "$MACOS_PERL_COMMAND" \
        "$MACOS_OTOOL_COMMAND" >> "$BALUN_TEST_LOG"
    if [ "$BALUN_FAKE_POLICY_STATUS" -ne 0 ]; then
        MACOS_PACKAGE_POLICY_REASON='synthetic policy failure'
        MACOS_PACKAGE_POLICY_RESULT=error
        return "$BALUN_FAKE_POLICY_STATUS"
    fi
    MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=20
    MACOS_PACKAGE_POLICY_RESULT=loaded
}

macos_validate_macho_copy_control()
{
    printf 'macho-inspect' >> "$BALUN_TEST_LOG"
    for argument in "$@"; do
        printf ' <%s>' "$argument" >> "$BALUN_TEST_LOG"
    done
    printf '\n' >> "$BALUN_TEST_LOG"
    if [ "$BALUN_FAKE_MACHO_STATUS" -ne 0 ]; then
        MACOS_PACKAGE_POLICY_REASON='synthetic Mach-O policy failure'
        MACOS_PACKAGE_POLICY_RESULT=uninspectable
        return "$BALUN_FAKE_MACHO_STATUS"
    fi
    MACOS_PACKAGE_POLICY_RESULT=allowed
}
EOF

chmod +x \
    "$fixture/scripts/build-macos.sh" \
    "$fake_bin/cargo" \
    "$fake_bin/pkg-config" \
    "$fake_bin/rustc" \
    "$fake_bin/uname"

# Restrict PATH to the reviewed route's commands. Unexpected direct package
# managers, downloaders, or installers therefore cannot reach host tools.
for utility in bash cat chmod dirname mkdir; do
    utility_path=$(command -v "$utility")
    [ -n "$utility_path" ] || {
        printf 'build-macos policy test requires %s\n' "$utility" >&2
        exit 2
    }
    ln -s "$utility_path" "$fake_bin/$utility"
done

fake_host_system=Darwin
fake_native_target=aarch64-apple-darwin
fake_coverage_version='cargo-llvm-cov 0.8.7'
fake_cargo_status=0
fake_rustc_status=0
fake_pkg_config_failure=
fake_plugin_directory=$plugin_directory
fake_policy_status=0
fake_macho_status=0
fake_skip_binary=0
fake_empty_binary=0
status=0
output=
desktop_output="$fixture/target/$fake_native_target/release/balun"
diagnostic_output="$fixture/target/$fake_native_target/release/balun-discover"

run_helper()
{
    : > "$command_log"
    set +e
    output=$(
        cd "$temp_dir"
        PATH="$fake_bin" \
        CDPATH="$temp_dir/hostile-cdpath" \
        CARGO_TARGET_DIR="$hostile_target_directory" \
        CARGO_BUILD_TARGET="$hostile_build_target" \
        CARGO_LLVM_COV_TARGET_DIR="$hostile_coverage_directory" \
        CARGO_LLVM_COV_BUILD_DIR="$hostile_coverage_directory" \
        MACOS_SHA256_COMMAND="$fake_bin/hostile-sha" \
        MACOS_PERL_COMMAND="$fake_bin/hostile-perl" \
        MACOS_OTOOL_COMMAND="$fake_bin/hostile-otool" \
        BALUN_TEST_LOG="$command_log" \
        BALUN_FAKE_HOST_SYSTEM="$fake_host_system" \
        BALUN_FAKE_COVERAGE_VERSION="$fake_coverage_version" \
        BALUN_FAKE_CARGO_STATUS="$fake_cargo_status" \
        BALUN_FAKE_RUSTC_STATUS="$fake_rustc_status" \
        BALUN_FAKE_RUSTC_TARGET="$fake_native_target" \
        BALUN_FAKE_PKG_CONFIG_FAILURE="$fake_pkg_config_failure" \
        BALUN_FAKE_PLUGIN_DIRECTORY="$fake_plugin_directory" \
        BALUN_FAKE_POLICY_STATUS="$fake_policy_status" \
        BALUN_FAKE_MACHO_STATUS="$fake_macho_status" \
        BALUN_FAKE_SKIP_BINARY="$fake_skip_binary" \
        BALUN_FAKE_EMPTY_BINARY="$fake_empty_binary" \
            /bin/bash "$fixture/scripts/build-macos.sh" "$@" 2>&1
    )
    status=$?
    set -e
}

fail_test()
{
    printf 'build-macos policy test failed: %s\n' "$*" >&2
    printf 'status: %s\noutput:\n%s\ncommands:\n' "$status" "$output" >&2
    sed -n '1,100p' "$command_log" >&2
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
    [ "$actual" = "$expected" ] || fail_test 'unexpected command routing'
}

expect_empty_log()
{
    [ ! -s "$command_log" ] || fail_test 'expected no Cargo or policy commands'
}

run_helper --help
expect_status 0
expect_output 'A lightweight cross-platform HDHomeRun live TV viewer'
expect_output 'Application ID: io.github.jm2.Balun'
expect_output 'With no options, builds the native Balun desktop executable'
expect_output 'target/<native-target>/release/balun'
expect_output '--diagnostic'
expect_output 'pkg-config-visible GTK 4.16'
expect_output 'preinstalled rustc reporting one native'
expect_output 'does not create Balun.app'
expect_output 'does not'
expect_output 'launch Balun'
expect_output 'never invokes Homebrew'
expect_output 'Cargo may fetch locked dependencies unless cached.'
expect_output 'may also fetch the selected Rust toolchain.'
expect_empty_log

run_helper --help --help
expect_status 0
expect_output 'macOS desktop build helper'
expect_empty_log

run_helper --diagnostic --help
expect_status 0
expect_output 'macOS desktop build helper'
expect_empty_log

# Packaging rejection itself uses only Bash builtins and therefore still wins
# with no PATH at all; it cannot accidentally probe Cargo, uname, or a packager.
set +e
early_output=$(PATH="$temp_dir/no-such-path" \
    /bin/bash "$script_under_test" --dmg 2>&1)
early_status=$?
set -e
[ "$early_status" -eq 2 ] \
    || fail_test 'packaging rejection did not preserve usage-error status without PATH'
case "$early_output" in
    *"Packaging mode '--dmg' is not available yet"*) ;;
    *) fail_test 'packaging rejection without PATH lost its diagnostic' ;;
esac

run_helper --unknown
expect_status 2
expect_output 'Unknown option: --unknown'
expect_empty_log

# Tributary's macOS helper has no run selector. Keep this preparatory helper
# build-only and reject a launch spelling before any dependency probe.
run_helper --run
expect_status 2
expect_output 'Unknown option: --run'
expect_empty_log

run_helper --help --unknown
expect_status 2
expect_output 'Unknown option: --unknown'
expect_empty_log

run_helper --unknown --help
expect_status 2
expect_output 'Unknown option: --unknown'
expect_empty_log

for package_mode in \
    --dmg --app --bundle --package --pkg --installer --sign --notarize
do
    run_helper "$package_mode"
    expect_status 2
    expect_output "Packaging mode '$package_mode' is not available yet"
    expect_output 'no Cargo, tool, install, package, or network work was started'
    expect_empty_log

    run_helper --help "$package_mode"
    expect_status 2
    expect_output "Packaging mode '$package_mode' is not available yet"
    expect_empty_log

    run_helper --diagnostic "$package_mode"
    expect_status 2
    expect_output "Packaging mode '$package_mode' is not available yet"
    expect_empty_log
done

for valued_package_mode in --dmg=output.dmg --app=Balun.app --package=pkg; do
    run_helper "$valued_package_mode"
    expect_status 2
    expect_output "Packaging mode '$valued_package_mode' is not available yet"
    expect_empty_log
done

for quick_mode in --fmt --check --clippy --coverage; do
    run_helper "$quick_mode" --dmg
    expect_status 2
    expect_output "Packaging mode '--dmg' is not available yet"
    expect_empty_log

    run_helper --dmg "$quick_mode"
    expect_status 2
    expect_output "Packaging mode '--dmg' is not available yet"
    expect_empty_log

    run_helper --help "$quick_mode"
    expect_status 0
    expect_output 'macOS desktop build helper'
    expect_empty_log
done

for first_quick_mode in --fmt --check --clippy --coverage; do
    for second_quick_mode in --fmt --check --clippy --coverage; do
        run_helper "$first_quick_mode" "$second_quick_mode"
        expect_status 2
        expect_output 'Quick-exit modes cannot be combined'
        expect_empty_log
    done
done

mv "$fake_bin/rustc" "$fake_bin/rustc.saved"
run_helper --check
expect_status 1
expect_output "Required command 'rustc' is unavailable"
expect_empty_log
mv "$fake_bin/rustc.saved" "$fake_bin/rustc"

fake_rustc_status=27
run_helper --check
expect_status 1
expect_output 'rustc could not report its native host tuple'
expect_log 'rustc <--print> <host-tuple>'
fake_rustc_status=0

fake_native_target=x86_64-unknown-linux-gnu
run_helper --check
expect_status 1
expect_output 'rustc host tuple must be one bounded Apple Darwin target'
expect_log 'rustc <--print> <host-tuple>'

fake_native_target='invalid target-apple-darwin'
run_helper --check
expect_status 1
expect_output 'rustc host tuple must be one bounded Apple Darwin target'
expect_log 'rustc <--print> <host-tuple>'

overlong_target=
while [ "${#overlong_target}" -le 128 ]; do
    overlong_target="${overlong_target}a"
done
fake_native_target="${overlong_target}-apple-darwin"
run_helper --check
expect_status 1
expect_output 'rustc host tuple must be one bounded Apple Darwin target'
expect_log 'rustc <--print> <host-tuple>'

fake_native_target=aarch64-apple-darwin

mv "$fake_bin/pkg-config" "$fake_bin/pkg-config.saved"
run_helper --check
expect_status 1
expect_output "Required command 'pkg-config' is unavailable"
expect_log 'rustc <--print> <host-tuple>'

run_helper --diagnostic --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
mv "$fake_bin/pkg-config.saved" "$fake_bin/pkg-config"

fake_pkg_config_failure=gtk4
run_helper --check
expect_status 1
expect_output 'gtk4 >= 4.16 was not found through pkg-config'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>'

run_helper
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
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
fake_pkg_config_failure=

run_helper --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'

run_helper --diagnostic --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'

run_helper --check
expect_status 0
expect_output 'Checking all Balun desktop targets'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'

run_helper --diagnostic --check
expect_status 0
expect_output 'Checking all Balun diagnostic targets'
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'

run_helper --clippy
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <clippy> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target> <--> <-D> <warnings>\ncargo <clippy> <--release> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target> <--> <-D> <warnings>'

run_helper --clippy --diagnostic
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <clippy> <--all-targets> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target> <--> <-D> <warnings>\ncargo <clippy> <--release> <--all-targets> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target> <--> <-D> <warnings>'

run_helper --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

run_helper --diagnostic --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--no-default-features> <--locked> <--target> <'"$fake_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

fake_coverage_version='cargo-llvm-cov 9.9.9'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_output 'will not install or replace tools'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

fake_coverage_version=$'cargo-llvm-cov 0.8.7\nunexpected second line'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

fake_cargo_status=26
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <llvm-cov> <--version>'
fake_cargo_status=0

mv "$fake_bin/cargo" "$fake_bin/cargo.saved"
run_helper --check
expect_status 1
expect_output "Required command 'cargo' is unavailable"
expect_empty_log
mv "$fake_bin/cargo.saved" "$fake_bin/cargo"

fake_host_system=Linux
run_helper
expect_status 1
expect_output 'requires a native macOS host; no Cargo build was started'
expect_empty_log
fake_host_system=Darwin

policy_helper="$fixture/scripts/macos-package-policy.sh"
mv "$policy_helper" "$policy_helper.saved"
run_helper --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
run_helper
expect_status 1
expect_output 'package-policy helper is unavailable or unsafe'
expect_empty_log
mv "$policy_helper.saved" "$policy_helper"

mv "$policy_helper" "$policy_helper.saved"
ln -s "$policy_helper.saved" "$policy_helper"
run_helper
expect_status 1
expect_output 'package-policy helper is unavailable or unsafe'
expect_empty_log
rm -f -- "$policy_helper"
mv "$policy_helper.saved" "$policy_helper"

mv "$policy_helper" "$policy_helper.saved"
printf '%s\n' \
    "MACOS_PACKAGE_POLICY_REASON=''" \
    "MACOS_PACKAGE_POLICY_RESULT=''" \
    'MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0' \
    'macos_package_policy_load() { return 0; }' \
    > "$policy_helper"
run_helper
expect_status 1
expect_output 'does not provide macos_validate_macho_copy_control'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>'
rm -f -- "$policy_helper"
mv "$policy_helper.saved" "$policy_helper"

policy_file="$fixture/build-aux/packaging/forbidden-bundled-components.txt"
mv "$policy_file" "$policy_file.saved"
run_helper --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'
run_helper
expect_status 1
expect_output 'Pinned macOS component policy is unavailable or unsafe'
expect_empty_log
mv "$policy_file.saved" "$policy_file"

mv "$policy_file" "$policy_file.saved"
ln -s "$policy_file.saved" "$policy_file"
run_helper
expect_status 1
expect_output 'Pinned macOS component policy is unavailable or unsafe'
expect_empty_log
rm -f -- "$policy_file"
mv "$policy_file.saved" "$policy_file"

run_helper
expect_status 0
expect_output 'Application ID: io.github.jm2.Balun'
expect_output 'GStreamer runtime plugin checks passed'
expect_output 'Mach-O component policy passed for expected Balun desktop path:'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target>\nmacho-inspect <'"$desktop_output"$'> <false>'
[ ! -e "$hostile_target_directory/$fake_native_target/release/balun" ] || \
    fail_test 'hostile CARGO_TARGET_DIR received the desktop output'
[ ! -e "$fixture/target/$hostile_build_target/release/balun" ] || \
    fail_test 'hostile CARGO_BUILD_TARGET received the desktop output'

# The desktop build fails closed before policy loading or Cargo work when a
# structural runtime plugin is missing; quick modes never consult plugins.
rm -f -- "$plugin_directory/libgstgtk4.dylib"
run_helper
expect_status 1
expect_output 'Required GStreamer playback runtime is incomplete'
expect_output 'libgstgtk4.dylib (gtk4paintablesink)'
expect_output 'Homebrew gstreamer formula'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>'
run_helper --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
: > "$plugin_directory/libgstgtk4.dylib"

rm -f -- "$plugin_directory/libgstlibav.dylib"
run_helper
expect_status 0
expect_output 'warning: libgstlibav.dylib is missing'
: > "$plugin_directory/libgstlibav.dylib"

fake_plugin_directory="$fixture/missing-plugins"
run_helper
expect_status 1
expect_output 'did not report an existing GStreamer plugin directory'
fake_plugin_directory=$plugin_directory

run_helper --diagnostic
expect_status 0
expect_output 'Mach-O component policy passed for expected balun-discover diagnostic path:'
expect_log $'rustc <--print> <host-tuple>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--bin> <balun-discover> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target>\nmacho-inspect <'"$diagnostic_output"$'> <false>'
[ ! -e "$hostile_target_directory/$fake_native_target/release/balun-discover" ] || \
    fail_test 'hostile CARGO_TARGET_DIR received the diagnostic output'
[ ! -e "$fixture/target/$hostile_build_target/release/balun-discover" ] || \
    fail_test 'hostile CARGO_BUILD_TARGET received the diagnostic output'

fake_policy_status=2
run_helper
expect_status 1
expect_output 'Pinned macOS component policy could not be loaded: synthetic policy failure'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>'
fake_policy_status=0

fake_cargo_status=24
run_helper
expect_status 24
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
fake_cargo_status=0

rm -f -- "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
fake_skip_binary=0

: > "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
fake_skip_binary=0

printf 'synthetic non-executable Mach-O\n' > "$desktop_output"
chmod -x "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"'/target>'
fake_skip_binary=0

rm -f -- "$desktop_output"
mkdir "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
fake_skip_binary=0
rmdir "$desktop_output"

mkfifo "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
fake_skip_binary=0
rm -f -- "$desktop_output"

rm -f -- "$desktop_output"
: > "$temp_dir/outside-symlink-target"
ln -s "$temp_dir/outside-symlink-target" \
    "$desktop_output"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty, executable, regular, non-symlink binary'
fake_skip_binary=0
rm -f -- "$desktop_output"

fake_macho_status=2
run_helper
expect_status 1
expect_output 'failed macOS Mach-O component-policy inspection'
expect_output 'synthetic Mach-O policy failure'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\npkg-config <--atleast-version=1.20> <gstreamer-1.0>\npkg-config <--variable=pluginsdir> <gstreamer-1.0>\npolicy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target> <'"$fake_native_target"$'> <--target-dir> <'"$fixture"$'/target>\nmacho-inspect <'"$desktop_output"$'> <false>'
fake_macho_status=0

forbidden_command_pattern='^[[:space:]]*([^[:space:]]*/)?(sudo|curl|wget|git|rustup|brew|port|apt|apt-get|dnf|yum|pacman|zypper|apk|snap|flatpak|hdiutil|create-dmg|productbuild|pkgbuild|codesign|xcrun)([[:space:]]|$)'
forbidden_cargo_install_pattern='^[[:space:]]*([^[:space:]]*/)?cargo[[:space:]]+(\+[^[:space:]]+[[:space:]]+)?install([[:space:]]|$)'
forbidden_runtime_copy_pattern='^[[:space:]]*([^[:space:]]*/)?(cp|ditto|rsync)[[:space:]].*(Frameworks|PlugIns|Resources|\.app)([[:space:]/]|$)'

if grep -Eq \
    "$forbidden_command_pattern|$forbidden_cargo_install_pattern|$forbidden_runtime_copy_pattern" \
    "$script_under_test"; then
    fail_test 'helper contains a direct installer, downloader, package tool, or runtime-copy route'
fi

case "$(cat "$script_under_test")" in
    *'A lightweight cross-platform HDHomeRun live TV viewer'*) ;;
    *) fail_test 'helper is missing the exact product tagline' ;;
esac
case "$(cat "$script_under_test")" in
    *'io.github.jm2.Balun'*) ;;
    *) fail_test 'helper is missing the exact application ID' ;;
esac

printf 'build-macos command-routing tests passed\n'
