#!/usr/bin/env bash
# Deterministic command-routing tests for scripts/build-linux.sh. The fixture
# substitutes Cargo and both policy validators, so no compiler, package manager,
# installer, network access, or real artifact inspection is used.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
script_under_test="$script_dir/build-linux.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf -- "$temp_dir"' EXIT HUP INT TERM

fixture="$temp_dir/repository with spaces"
fake_bin="$fixture/fake-bin"
command_log="$temp_dir/commands.log"

mkdir -p "$fixture/scripts" "$fixture/build-aux/linux" "$fake_bin"
cp "$script_under_test" "$fixture/scripts/build-linux.sh"

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
    : > target/release/balun-discover
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
    "$fake_bin/readelf" \
    "$fixture/build-aux/linux/validate-package-metadata.sh" \
    "$fixture/build-aux/linux/validate-package-compliance.sh"

# Give the helper only the external commands its current, reviewed routes need.
# A future direct downloader or package-manager invocation therefore cannot
# silently reach a host tool before the static policy check below catches it.
for utility in bash cat dirname mkdir; do
    utility_path=$(command -v "$utility")
    [ -n "$utility_path" ] || {
        printf 'build-linux policy test requires %s\n' "$utility" >&2
        exit 2
    }
    ln -s "$utility_path" "$fake_bin/$utility"
done

fake_coverage_version='cargo-llvm-cov 0.8.7'
fake_cargo_status=0
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
        BALUN_TEST_LOG="$command_log" \
        BALUN_FAKE_COVERAGE_VERSION="$fake_coverage_version" \
        BALUN_FAKE_CARGO_STATUS="$fake_cargo_status" \
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
expect_output 'This helper never invokes tool or package installers.'
expect_output 'Cargo may fetch locked dependencies unless cached.'
expect_output 'may also fetch the selected Rust toolchain.'
expect_empty_log

run_helper --not-a-mode
expect_status 2
expect_output 'Unknown option: --not-a-mode'
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
expect_log $'cargo <llvm-cov> <--version>\ncargo <llvm-cov> <--all-targets> <--no-default-features> <--locked> <--summary-only>'

fake_coverage_version='cargo-llvm-cov 9.9.9'
run_helper --coverage
expect_status 1
expect_output 'requires preinstalled cargo-llvm-cov 0.8.7 exactly'
expect_output 'will not install or replace tools'
expect_log 'cargo <llvm-cov> <--version>'
fake_coverage_version='cargo-llvm-cov 0.8.7'

run_helper
expect_status 0
expect_log $'metadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover>\ncompliance <--elf> <'"$fixture"$'/target/release/balun-discover>'

fake_metadata_status=23
run_helper
expect_status 23
expect_log 'metadata'
fake_metadata_status=0

fake_cargo_status=24
run_helper
expect_status 24
expect_log $'metadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover>'
fake_cargo_status=0

rm -f -- "$fixture/target/release/balun-discover"
fake_skip_binary=1
run_helper
expect_status 1
expect_output 'did not produce the expected regular, non-symlink binary'
expect_log $'metadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover>'
fake_skip_binary=0

rm -f -- "$fixture/target/release/balun-discover"
: > "$temp_dir/outside-symlink-target"
ln -s "$temp_dir/outside-symlink-target" \
    "$fixture/target/release/balun-discover"
run_helper
expect_status 1
expect_output 'did not produce the expected regular, non-symlink binary'
expect_log $'metadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover>'
rm -f -- "$fixture/target/release/balun-discover"

fake_artifact_status=25
run_helper
expect_status 25
expect_log $'metadata\ncargo <build> <--release> <--locked> <--bin> <balun-discover>\ncompliance <--elf> <'"$fixture"$'/target/release/balun-discover>'

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
