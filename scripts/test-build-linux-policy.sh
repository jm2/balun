#!/usr/bin/env bash
# Deterministic command-routing tests for scripts/build-linux.sh. The fixture
# substitutes Cargo, rustc, pkg-config, and both policy validators, so no
# compiler, package manager, installer, network access, or real artifact
# inspection is used.

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

mkdir -p "$fixture/scripts" "$fixture/build-aux/linux" "$fake_bin"
cp "$script_under_test" "$fixture/scripts/build-linux.sh"

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
    printf 'synthetic ELF\n' > \
        "$cargo_target_dir/$cargo_build_target/release/$binary_name"
    chmod +x "$cargo_target_dir/$cargo_build_target/release/$binary_name"
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
EOF

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
exit "$BALUN_FAKE_ARTIFACT_STATUS"
EOF

chmod +x \
    "$fixture/scripts/build-linux.sh" \
    "$fake_bin/cargo" \
    "$fake_bin/rustc" \
    "$fake_bin/pkg-config" \
    "$fake_bin/readelf" \
    "$fixture/build-aux/linux/validate-package-metadata.sh" \
    "$fixture/build-aux/linux/validate-package-compliance.sh"

# Give the helper only the external commands its current, reviewed routes need.
# A future direct downloader or package-manager invocation therefore cannot
# silently reach a host tool before the static policy check below catches it.
for utility in bash cat chmod dirname mkdir; do
    utility_path=$(command -v "$utility")
    [ -n "$utility_path" ] || {
        printf 'build-linux policy test requires %s\n' "$utility" >&2
        exit 2
    }
    ln -s "$utility_path" "$fake_bin/$utility"
done

fake_coverage_version='cargo-llvm-cov 0.8.7'
fake_cargo_status=0
fake_rustc_status=0
valid_native_target='x86_64-unknown-linux-gnu'
fake_rustc_target=$valid_native_target
native_release_directory="$fixture/target/$valid_native_target/release"
fake_pkg_config_failure=
fake_metadata_status=0
fake_artifact_status=0
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
        BALUN_FAKE_CARGO_STATUS="$fake_cargo_status" \
        BALUN_FAKE_RUSTC_STATUS="$fake_rustc_status" \
        BALUN_FAKE_RUSTC_TARGET="$fake_rustc_target" \
        BALUN_FAKE_PKG_CONFIG_FAILURE="$fake_pkg_config_failure" \
        BALUN_FAKE_METADATA_STATUS="$fake_metadata_status" \
        BALUN_FAKE_ARTIFACT_STATUS="$fake_artifact_status" \
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

run_helper --help
expect_status 0
expect_output 'A lightweight cross-platform HDHomeRun live TV viewer'
expect_output 'Application ID: io.github.jm2.Balun'
expect_output 'Linux desktop build helper'
expect_output 'builds only and never launches'
expect_output '--diagnostic'
expect_output 'This helper never invokes tool or package installers.'
expect_output 'Cargo may fetch locked dependencies unless cached.'
expect_output 'may also fetch the selected Rust toolchain.'
expect_empty_log

run_helper --not-a-mode
expect_status 2
expect_output 'Unknown option: --not-a-mode'
expect_empty_log

# Tributary has no Linux --run flag. Keep the helper build-only and reject a
# launch spelling before resolving build or desktop dependencies.
run_helper --run
expect_status 2
expect_output 'Unknown option: --run'
expect_empty_log

for package_mode in --flatpak --deb --rpm --arch-pkg; do
    run_helper "$package_mode"
    expect_status 2
    expect_output "Packaging mode '$package_mode' is not available yet"
    expect_output 'no build, install, or network work was started'
    expect_empty_log

    run_helper --help "$package_mode"
    expect_status 2
    expect_output "Packaging mode '$package_mode' is not available yet"
    expect_empty_log
done

run_helper --fmt --check
expect_status 2
expect_output 'Quick-exit modes cannot be combined'
expect_empty_log

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
fake_pkg_config_failure=

run_helper --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'

run_helper --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\ncargo <check> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

run_helper --diagnostic --check
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <check> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

run_helper --clippy
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\ncargo <clippy> <--all-targets> <--all-features> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>'

run_helper --diagnostic --clippy
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <clippy> <--all-targets> <--locked> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'> <--> <-D> <warnings>'

run_helper --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--all-features> <--locked> <--target> <'"$valid_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

run_helper --diagnostic --coverage
expect_status 0
expect_log $'rustc <--print> <host-tuple>\ncargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--no-default-features> <--locked> <--target> <'"$valid_native_target"$'> <--summary-only> <CARGO_TARGET_DIR='"$fixture"$'/target> <CARGO_LLVM_COV_TARGET_DIR='"$fixture"$'/target/llvm-cov-target> <CARGO_LLVM_COV_BUILD_DIR='"$fixture"$'/target/llvm-cov-target>'

fake_coverage_version='cargo-llvm-cov 9.9.9'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_output 'will not install or replace tools'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\ncargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

run_helper
expect_status 0
expect_output 'Desktop output:'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun>'
[ ! -e "$hostile_target_directory/$valid_native_target/release/balun" ] || \
    fail_test 'hostile CARGO_TARGET_DIR received the desktop output'
[ ! -e "$fixture/target/$hostile_build_target/release/balun" ] || \
    fail_test 'hostile CARGO_BUILD_TARGET received the desktop output'

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
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata'
fake_metadata_status=0

fake_cargo_status=24
run_helper
expect_status 24
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_cargo_status=0

rm -f -- "$native_release_directory/balun"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_skip_binary=0

rm -f -- "$native_release_directory/balun"
: > "$temp_dir/outside-symlink-target"
ln -s "$temp_dir/outside-symlink-target" \
    "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rm -f -- "$native_release_directory/balun"

: > "$native_release_directory/balun"
chmod +x "$native_release_directory/balun"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'

rm -f -- "$native_release_directory/balun"
mkdir "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rmdir "$native_release_directory/balun"

mkfifo "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
rm -f -- "$native_release_directory/balun"

printf 'synthetic ELF\n' > "$native_release_directory/balun"
chmod -x "$native_release_directory/balun"
run_helper
expect_status 1
expect_output 'did not produce the expected nonempty, executable, regular, non-symlink binary'
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>'
fake_skip_binary=0
rm -f -- "$native_release_directory/balun"

fake_artifact_status=25
run_helper
expect_status 25
expect_log $'rustc <--print> <host-tuple>\npkg-config <--atleast-version=4.16> <gtk4>\npkg-config <--atleast-version=1.6> <libadwaita-1>\nmetadata\ncargo <build> <--release> <--locked> <--features> <desktop> <--bin> <balun> <--target-dir> <'"$fixture"$'/target> <--target> <'"$valid_native_target"$'>\ncompliance <--elf> <'"$native_release_directory"$'/balun>'

forbidden_command_pattern='^[[:space:]]*([^[:space:]]*/)?(sudo|curl|wget|git|rustup|apt|apt-get|dnf|yum|pacman|zypper|apk|snap|flatpak|flatpak-builder|brew|port|rpm|dpkg|makepkg)([[:space:]]|$)'
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

printf 'build-linux policy tests passed\n'
