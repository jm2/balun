#!/usr/bin/env bash
# Deterministic command-routing tests for scripts/build-macos.sh. The fixture
# substitutes Cargo and the policy helper, so no compiler, package manager,
# installer, network access, or real Mach-O inspection is used.

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
printf '\n' >> "$BALUN_TEST_LOG"

if [ "${1-}" = llvm-cov ] && [ "${2-}" = --version ]; then
    printf '%s\n' "$BALUN_FAKE_COVERAGE_VERSION"
fi

if [ "$BALUN_FAKE_CARGO_STATUS" -ne 0 ]; then
    exit "$BALUN_FAKE_CARGO_STATUS"
fi

if [ "${1-}" = build ] && [ "$BALUN_FAKE_SKIP_BINARY" -eq 0 ]; then
    mkdir -p target/release
    if [ "$BALUN_FAKE_EMPTY_BINARY" -eq 1 ]; then
        : > target/release/balun-discover
    else
        printf 'synthetic Mach-O\n' > target/release/balun-discover
    fi
fi
EOF

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
    "$fake_bin/uname"

# Restrict PATH to the reviewed route's commands. Unexpected direct package
# managers, downloaders, or installers therefore cannot reach host tools.
for utility in bash cat dirname mkdir; do
    utility_path=$(command -v "$utility")
    [ -n "$utility_path" ] || {
        printf 'build-macos policy test requires %s\n' "$utility" >&2
        exit 2
    }
    ln -s "$utility_path" "$fake_bin/$utility"
done

fake_host_system=Darwin
fake_coverage_version='cargo-llvm-cov 0.8.7'
fake_cargo_status=0
fake_policy_status=0
fake_macho_status=0
fake_skip_binary=0
fake_empty_binary=0
status=0
output=

run_helper()
{
    : > "$command_log"
    set +e
    output=$(
        cd "$temp_dir"
        PATH="$fake_bin" \
        CDPATH="$temp_dir/hostile-cdpath" \
        MACOS_SHA256_COMMAND="$fake_bin/hostile-sha" \
        MACOS_PERL_COMMAND="$fake_bin/hostile-perl" \
        MACOS_OTOOL_COMMAND="$fake_bin/hostile-otool" \
        BALUN_TEST_LOG="$command_log" \
        BALUN_FAKE_HOST_SYSTEM="$fake_host_system" \
        BALUN_FAKE_COVERAGE_VERSION="$fake_coverage_version" \
        BALUN_FAKE_CARGO_STATUS="$fake_cargo_status" \
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
expect_output 'does not create Balun.app'
expect_output 'never invokes Homebrew'
expect_output 'Cargo may fetch locked dependencies unless cached.'
expect_output 'may also fetch the selected Rust toolchain.'
expect_empty_log

run_helper --help --help
expect_status 0
expect_output 'macOS headless diagnostic build helper'
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
    expect_output 'macOS headless diagnostic build helper'
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

run_helper --fmt
expect_status 0
expect_log 'cargo <fmt> <--all>'

run_helper --check
expect_status 0
expect_log 'cargo <check> <--all-targets> <--locked>'

run_helper --clippy
expect_status 0
expect_log 'cargo <clippy> <--all-targets> <--locked> <--> <-D> <warnings>'

run_helper --coverage
expect_status 0
expect_log $'cargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--all-features> <--locked> <--summary-only>'

fake_coverage_version='cargo-llvm-cov 9.9.9'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_output 'will not install or replace tools'
expect_log 'cargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

fake_coverage_version=$'cargo-llvm-cov 0.8.7\nunexpected second line'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_log $'cargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

fake_cargo_status=26
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_log 'cargo <llvm-cov> <--version>'
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
expect_log 'cargo <check> <--all-targets> <--locked>'
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
expect_empty_log
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
expect_output 'Mach-O component policy passed for expected diagnostic path:'
expect_log $'policy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--bin> <balun-discover>\nmacho-inspect <'"$fixture"$'/target/release/balun-discover> <false>'

fake_policy_status=2
run_helper
expect_status 1
expect_output 'Pinned macOS component policy could not be loaded: synthetic policy failure'
expect_log 'policy-load <'"$fixture"'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>'
fake_policy_status=0

fake_cargo_status=24
run_helper
expect_status 24
expect_log $'policy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--bin> <balun-discover>'
fake_cargo_status=0

rm -f -- "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty regular, non-symlink binary'
expect_log $'policy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--bin> <balun-discover>'
fake_skip_binary=0

: > "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty regular, non-symlink binary'
fake_skip_binary=0

rm -f -- "$fixture/target/release/balun-discover"
mkdir "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty regular, non-symlink binary'
fake_skip_binary=0
rmdir "$fixture/target/release/balun-discover"

mkfifo "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty regular, non-symlink binary'
fake_skip_binary=0
rm -f -- "$fixture/target/release/balun-discover"

rm -f -- "$fixture/target/release/balun-discover"
: > "$temp_dir/outside-symlink-target"
ln -s "$temp_dir/outside-symlink-target" \
    "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'expected nonempty regular, non-symlink binary'
fake_skip_binary=0
rm -f -- "$fixture/target/release/balun-discover"

fake_macho_status=2
run_helper
expect_status 1
expect_output 'failed macOS Mach-O component-policy inspection'
expect_output 'synthetic Mach-O policy failure'
expect_log $'policy-load <'"$fixture"$'/build-aux/packaging/forbidden-bundled-components.txt> sha <balun_macos_sha256> perl </usr/bin/perl> otool </usr/bin/otool>\ncargo <build> <--release> <--locked> <--bin> <balun-discover>\nmacho-inspect <'"$fixture"$'/target/release/balun-discover> <false>'
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
