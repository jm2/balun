#!/usr/bin/env bash
# Deterministic policy/validator coverage. No package manager, network, media
# runtime, or prohibited component is required.

set -euo pipefail
set -f
export LC_ALL=C

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
validator="$script_dir/validate-release-components.sh"
policy="$script_dir/forbidden-bundled-components.txt"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

expect_status()
{
    local expected actual
    expected=$1
    shift
    set +e
    "$@" >/dev/null 2>&1
    actual=$?
    set -e
    [ "$actual" -eq "$expected" ] || {
        echo "Expected status $expected, got $actual: $*" >&2
        exit 1
    }
}

first_token=$(awk '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print tolower($0); exit }
' "$policy")
[ -n "$first_token" ] || {
    echo "Shared release component policy unexpectedly has no tokens" >&2
    exit 1
}

# The checked-in repository, including every untracked non-ignored input in a
# developer checkout, must pass its own validator.
"$validator" --repository >/dev/null

# Ordinary broadcast codecs, media containers, TLS, and general-purpose
# cryptography are intentionally outside this narrow denied-token policy.
allowed_input="$temp_dir/allowed-input.toml"
printf '%s\n' \
    'dependencies = [gstreamer, libavcodec, libbluray, rustls, libcrypto]' \
    'formats = [mpegts, h264, hevc, ac3, aac]' > "$allowed_input"
"$validator" --inputs "$allowed_input" >/dev/null

# An inspector error after an allowed partial read is a setup failure, not the
# same as a clean no-match result.
real_grep=$(command -v grep)
mkdir -p "$temp_dir/failing-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'if [ "$1" = -Fqi ]; then exit 7; fi' \
    'exec "$TEST_REAL_GREP" "$@"' \
    > "$temp_dir/failing-tools/grep"
chmod +x "$temp_dir/failing-tools/grep"
expect_status 2 env PATH="$temp_dir/failing-tools:$PATH" \
    TEST_REAL_GREP="$real_grep" "$validator" --inputs "$allowed_input"

# The bounded UTF-8 inspector is also mandatory; merely finding an executable
# named perl must not turn an inspector failure into a clean result.
printf '%s\n' '#!/bin/sh' 'exit 7' > "$temp_dir/failing-tools/perl"
chmod +x "$temp_dir/failing-tools/perl"
expect_status 2 env PATH="$temp_dir/failing-tools:$PATH" \
    "$validator" --inputs "$allowed_input"

# Reference normalization is Bash-owned ASCII lowercasing, so a broken
# external `tr` cannot silently turn a denied path into an allowed one.
mkdir -p "$temp_dir/no-normalizer-tool"
printf '%s\n' '#!/bin/sh' 'exit 7' > "$temp_dir/no-normalizer-tool/tr"
chmod +x "$temp_dir/no-normalizer-tool/tr"
normalizer_probe="$temp_dir/prefix-${first_token^^}-suffix.so"
: > "$normalizer_probe"
expect_status 1 env PATH="$temp_dir/no-normalizer-tool:$PATH" \
    "$validator" --inputs "$normalizer_probe"

# Every shared token is enforced in both filenames and textual packaging
# inputs, and matching is ASCII case-insensitive.
mkdir -p "$temp_dir/rejected-path"
while IFS= read -r line || [ -n "$line" ]; do
    token=${line%$'\r'}
    token=$(printf '%s' "$token" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    case "$token" in
        '' | \#*) continue ;;
    esac

    rejected_path="$temp_dir/rejected-path/prefix-${token}-suffix.so"
    : > "$rejected_path"
    expect_status 1 "$validator" --inputs "$rejected_path"
    rm -f "$rejected_path"

    rejected_input="$temp_dir/rejected-input.toml"
    printf 'native_dependency = "prefix-%s-suffix"\n' "$token" > "$rejected_input"
    expect_status 1 "$validator" --inputs "$rejected_input"
done < "$policy"

uppercase=$(printf '%s' "$first_token" | tr '[:lower:]' '[:upper:]')
printf 'native_dependency = "%s"\n' "$uppercase" > "$temp_dir/uppercase-input.toml"
expect_status 1 "$validator" --inputs "$temp_dir/uppercase-input.toml"

ln -s "../prefix-${first_token}-suffix.dll" "$temp_dir/innocent-link.toml"
expect_status 1 "$validator" --inputs "$temp_dir/innocent-link.toml"

# Explicit packaging inputs must be readable, NUL-free UTF-8 regular files. An
# absent, binary, malformed, or symlinked input is a setup failure rather than
# an implicit pass.
expect_status 2 "$validator" --inputs "$temp_dir/missing-input.toml"
printf '\000binary' > "$temp_dir/binary-input.toml"
expect_status 2 "$validator" --inputs "$temp_dir/binary-input.toml"
perl -e 'print "allowed = true\n", "x" x 131072, "\0denied = $ARGV[0]\n"' \
    "$first_token" > "$temp_dir/late-nul-input.toml"
expect_status 2 "$validator" --inputs "$temp_dir/late-nul-input.toml"
perl -e 'binmode STDOUT; print "allowed = true\n\xFF\n"' \
    > "$temp_dir/invalid-utf8-input.toml"
expect_status 2 "$validator" --inputs "$temp_dir/invalid-utf8-input.toml"
ln -s "$allowed_input" "$temp_dir/allowed-link.toml"
expect_status 2 "$validator" --inputs "$temp_dir/allowed-link.toml"
ln -s $'../allowed\nmisleading-log-line' "$temp_dir/control-target.toml"
expect_status 2 "$validator" --inputs "$temp_dir/control-target.toml"

# File count and cumulative byte limits are independent. Repeating one valid
# file exercises both limits without allocating thousands of files or a large
# on-disk fixture.
too_many_inputs=()
for ((index = 0; index < 4097; index++)); do
    too_many_inputs+=("$allowed_input")
done
expect_status 2 "$validator" --inputs "${too_many_inputs[@]}"

cumulative_input="$temp_dir/cumulative-input.toml"
perl -e 'print "x" x (4 * 1024 * 1024)' > "$cumulative_input"
cumulative_inputs=()
for ((index = 0; index < 17; index++)); do
    cumulative_inputs+=("$cumulative_input")
done
expect_status 2 "$validator" --inputs "${cumulative_inputs[@]}"

oversized_input="$temp_dir/oversized-input.toml"
truncate -s 67108865 "$oversized_input"
expect_status 2 "$validator" --inputs "$oversized_input"

# Repository mode discovers dependency and packaging inputs instead of relying
# on callers to enumerate them. A normal source document is not content-scanned,
# but its path is still subject to the shared filename policy.
fixture_repository="$temp_dir/repository-fixture"
mkdir -p "$fixture_repository/build-aux/packaging" "$fixture_repository/docs"
cp "$validator" "$fixture_repository/build-aux/packaging/validate-release-components.sh"
cp "$policy" "$fixture_repository/build-aux/packaging/forbidden-bundled-components.txt"
printf 'dependency = "prefix-%s-suffix"\n' "$first_token" \
    > "$fixture_repository/Cargo.toml"
printf 'Design discussion: prefix-%s-suffix\n' "$first_token" \
    > "$fixture_repository/docs/note.md"
git -C "$fixture_repository" init -q
git -C "$fixture_repository" add .
expect_status 1 \
    "$fixture_repository/build-aux/packaging/validate-release-components.sh" \
    --repository
printf 'dependency = "gstreamer"\n' > "$fixture_repository/Cargo.toml"
"$fixture_repository/build-aux/packaging/validate-release-components.sh" \
    --repository >/dev/null

mkdir -p "$fixture_repository/src"
rejected_repository_path="src/prefix-${first_token}-suffix.rs"
: > "$fixture_repository/$rejected_repository_path"
git -C "$fixture_repository" add "$rejected_repository_path"
expect_status 1 \
    "$fixture_repository/build-aux/packaging/validate-release-components.sh" \
    --repository
git -C "$fixture_repository" rm -q -f "$rejected_repository_path"

# Native-link declarations and standard build/package entry points are scanned
# even when their own filename is innocent. The extensionless executable also
# pins the generic executable-helper boundary.
classified_inputs=(
    build.rs
    .cargo/config.toml
    src/native_link.rs
    scripts/build.sh
    tools/assemble
    Makefile
    CMakeLists.txt
    meson.build
    debian/control
    snap/snapcraft.yaml
    installer.nsi
)
for classified_input in "${classified_inputs[@]}"; do
    mkdir -p "$fixture_repository/$(dirname -- "$classified_input")"
    printf 'native_dependency = "prefix-%s-suffix"\n' "$first_token" \
        > "$fixture_repository/$classified_input"
    if [ "$classified_input" = tools/assemble ]; then
        chmod +x "$fixture_repository/$classified_input"
    fi
    git -C "$fixture_repository" add "$classified_input"
    expect_status 1 \
        "$fixture_repository/build-aux/packaging/validate-release-components.sh" \
        --repository
    git -C "$fixture_repository" rm -q -f "$classified_input"
done

# Missing, empty, comments-only, binary, oversized, overlong, malformed, and
# duplicate policies all fail closed before an otherwise allowed input can be
# accepted.
for fixture in \
    missing empty comments binary oversized too-many-lines overlong-line \
    malformed duplicate missing-required-token
do
    fixture_dir="$temp_dir/policy-$fixture/build-aux/packaging"
    mkdir -p "$fixture_dir"
    cp "$validator" "$fixture_dir/validate-release-components.sh"
    fixture_policy="$fixture_dir/forbidden-bundled-components.txt"
    case "$fixture" in
        missing)
            ;;
        empty)
            : > "$fixture_policy"
            ;;
        comments)
            printf '# no active policy entries\n' > "$fixture_policy"
            ;;
        binary)
            printf '\000binary\n' > "$fixture_policy"
            ;;
        oversized)
            awk 'BEGIN {
                for (line = 0; line < 65; line++) {
                    printf "#"
                    for (column = 0; column < 1022; column++) printf "x"
                    printf "\n"
                }
            }' > "$fixture_policy"
            ;;
        too-many-lines)
            awk 'BEGIN {
                for (line = 0; line < 1025; line++) print "# comment"
            }' > "$fixture_policy"
            ;;
        overlong-line)
            awk 'BEGIN {
                printf "#"
                for (column = 0; column < 1024; column++) printf "x"
                printf "\n"
            }' > "$fixture_policy"
            ;;
        malformed)
            printf 'valid-token\nbad token\n' > "$fixture_policy"
            ;;
        duplicate)
            printf 'duplicate\nDUPLICATE\n' > "$fixture_policy"
            ;;
        missing-required-token)
            awk '
                !removed && $0 !~ /^[[:space:]]*(#|$)/ {
                    removed = 1
                    next
                }
                { print }
                END { if (!removed) exit 2 }
            ' "$policy" > "$fixture_policy"
            ;;
    esac
    expect_status 2 "$fixture_dir/validate-release-components.sh" \
        --inputs "$allowed_input"
done

echo "Release component input policy tests passed."
