#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/macos-package-policy.sh
source "${SCRIPT_DIR}/macos-package-policy.sh"

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/balun-macos-package-policy.XXXXXX")"
POLICY_TMPDIR="${TEST_ROOT}/Policy Temp"
mkdir -p "$POLICY_TMPDIR"
TMPDIR="$POLICY_TMPDIR"
export TMPDIR
cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

fail() {
  echo "not ok - $*" >&2
  exit 1
}

assert_status() {
  local expected="$1"
  shift
  local actual=0
  "$@" || actual=$?
  [[ "$actual" -eq "$expected" ]] \
    || fail "expected status ${expected}, got ${actual}: $*; result=${MACOS_PACKAGE_POLICY_RESULT-<unset>}; reason=${MACOS_PACKAGE_POLICY_REASON-<unset>}"
}

assert_reason_contains() {
  [[ "$MACOS_PACKAGE_POLICY_REASON" == *"$1"* ]] \
    || fail "diagnostic '${MACOS_PACKAGE_POLICY_REASON}' did not contain '$1'"
}

assert_no_policy_temporaries() {
  local temporary
  for temporary in \
    "$POLICY_TMPDIR"/balun-macos-component-policy.* \
    "$POLICY_TMPDIR"/balun-macos-macho-policy.* \
    "$POLICY_TMPDIR"/balun-macos-bundle-policy.*; do
    [[ ! -e "$temporary" ]] \
      || fail "macOS policy temporary was not cleaned up: ${temporary}"
  done
}

assert_prohibited_name() {
  macos_copy_control_path_is_prohibited "$1" \
    || fail "expected a denied filename derived from the shared policy: $1"
}

assert_allowed_name() {
  if macos_copy_control_path_is_prohibited "$1"; then
    fail "ordinary runtime was overmatched by '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}': $1"
  fi
}

POLICY_FILE="$(macos_package_policy_default_file)"
# macOS ships Bash 3.2, where expanding an empty indexed array under nounset
# aborts the shell. Exercise the unloaded matcher before any token exists.
macos_package_policy_reset
assert_status 1 macos_copy_control_path_is_prohibited 'libordinary.dylib'
assert_status 0 macos_package_policy_load "$POLICY_FILE"
[[ "$MACOS_PACKAGE_POLICY_RESULT" == loaded ]] \
  || fail "reviewed policy was not marked loaded"
[[ "$MACOS_PACKAGE_POLICY_DIGEST" == "$MACOS_PACKAGE_POLICY_EXPECTED_SHA256" ]] \
  || fail "loaded policy digest did not match the pinned digest"
[[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -gt 0 ]] \
  || fail "reviewed policy did not provide any tokens"
DENIED_TOKEN="${MACOS_FORBIDDEN_COMPONENT_TOKENS[0]}"
DENIED_UPPER="$(printf '%s' "$DENIED_TOKEN" | tr '[:lower:]' '[:upper:]')"
assert_no_policy_temporaries

# BSD wc pads redirected scalar counts with leading spaces. Keep this wrapper
# active for the rest of the suite so policy, tool-output, capture, and manifest
# measurements all exercise strict trimming of otherwise valid numeric output.
PADDED_WC_DIR="${TEST_ROOT}/padded-wc"
mkdir -p "$PADDED_WC_DIR"
TEST_REAL_WC="$(command -v wc)"
cat > "$PADDED_WC_DIR/wc" <<'PADDED_WC_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
count="$($TEST_REAL_WC "$@")"
printf '   %s   \n' "$count"
PADDED_WC_SCRIPT
chmod +x "$PADDED_WC_DIR/wc"
TEST_REAL_WC="$TEST_REAL_WC"
export TEST_REAL_WC
PATH="${PADDED_WC_DIR}:${PATH}"
export PATH
assert_status 0 macos_package_policy_load "$POLICY_FILE"
[[ "$MACOS_PACKAGE_POLICY_RESULT" == loaded ]] \
  || fail 'padded wc output prevented the reviewed policy from loading'

# Every negative fixture below is harmless synthetic text or is derived from
# the reviewed policy at runtime. No denied component name is duplicated in
# this test source.
MISSING_POLICY="${TEST_ROOT}/missing-policy.txt"
EMPTY_POLICY="${TEST_ROOT}/empty-policy.txt"
COMMENTS_POLICY="${TEST_ROOT}/comments-policy.txt"
NUL_POLICY="${TEST_ROOT}/nul-policy.txt"
INVALID_UTF8_POLICY="${TEST_ROOT}/invalid-utf8-policy.txt"
OVERSIZED_POLICY="${TEST_ROOT}/oversized-policy.txt"
TOO_MANY_LINES_POLICY="${TEST_ROOT}/too-many-lines-policy.txt"
OVERLONG_LINE_POLICY="${TEST_ROOT}/overlong-line-policy.txt"
MALFORMED_POLICY="${TEST_ROOT}/malformed-policy.txt"
DUPLICATE_POLICY="${TEST_ROOT}/duplicate-policy.txt"
APPENDED_POLICY="${TEST_ROOT}/appended-policy.txt"
TRUNCATED_POLICY="${TEST_ROOT}/truncated-policy.txt"
SYMLINK_POLICY="${TEST_ROOT}/symlink-policy.txt"

: > "$EMPTY_POLICY"
printf '# comments do not constitute a policy\n\n' > "$COMMENTS_POLICY"
printf 'safe\000suffix\n' > "$NUL_POLICY"
printf '\377\n' > "$INVALID_UTF8_POLICY"
awk 'BEGIN { for (i = 0; i < 65537; i++) printf "x"; printf "\n" }' \
  > "$OVERSIZED_POLICY"
awk 'BEGIN { for (i = 0; i < 1025; i++) print "# line" }' \
  > "$TOO_MANY_LINES_POLICY"
awk 'BEGIN { for (i = 0; i < 1025; i++) printf "x"; printf "\n" }' \
  > "$OVERLONG_LINE_POLICY"
printf 'valid-token\nbad token\n' > "$MALFORMED_POLICY"
printf 'duplicate\nDUPLICATE\n' > "$DUPLICATE_POLICY"
cp "$POLICY_FILE" "$APPENDED_POLICY"
printf '# unreviewed mutation\n' >> "$APPENDED_POLICY"
sed '$d' "$POLICY_FILE" > "$TRUNCATED_POLICY"
ln -s "$POLICY_FILE" "$SYMLINK_POLICY"

for bad_policy in \
  "$MISSING_POLICY" "$EMPTY_POLICY" "$COMMENTS_POLICY" "$NUL_POLICY" \
  "$INVALID_UTF8_POLICY" "$OVERSIZED_POLICY" "$TOO_MANY_LINES_POLICY" \
  "$OVERLONG_LINE_POLICY" "$MALFORMED_POLICY" "$DUPLICATE_POLICY" \
  "$APPENDED_POLICY" "$TRUNCATED_POLICY" "$SYMLINK_POLICY"; do
  assert_status 2 macos_package_policy_load "$bad_policy"
  [[ "$MACOS_PACKAGE_POLICY_RESULT" == error ]] \
    || fail "invalid policy was not marked as a setup error: ${bad_policy}"
  [[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -eq 0 ]] \
    || fail "invalid policy left an enforcement set active: ${bad_policy}"
  assert_no_policy_temporaries
done

HASH_ADAPTER="${TEST_ROOT}/hash-adapter"
MUTATING_HASH="${TEST_ROOT}/mutating-hash"
HASH_STATE="${TEST_ROOT}/hash-state"
cat > "$HASH_ADAPTER" <<'HASH_ADAPTER_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
if command -v sha256sum >/dev/null 2>&1; then
  exec sha256sum -- "$1"
fi
exec shasum -a 256 "$1"
HASH_ADAPTER_SCRIPT
cat > "$MUTATING_HASH" <<'MUTATING_HASH_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
path="$1"
count="$(cat "$TEST_HASH_STATE")"
"$TEST_HASH_ADAPTER" "$path"
if [[ "$count" -eq 0 ]]; then
  printf '# concurrent harmless mutation\n' >> "$path"
fi
printf '%s\n' "$((count + 1))" > "$TEST_HASH_STATE"
MUTATING_HASH_SCRIPT
chmod +x "$HASH_ADAPTER" "$MUTATING_HASH"
printf '0\n' > "$HASH_STATE"
MACOS_SHA256_COMMAND="$MUTATING_HASH"
TEST_HASH_ADAPTER="$HASH_ADAPTER"
TEST_HASH_STATE="$HASH_STATE"
export MACOS_SHA256_COMMAND TEST_HASH_ADAPTER TEST_HASH_STATE
assert_status 2 macos_package_policy_load "$POLICY_FILE"
assert_reason_contains 'changed while it was being validated'
[[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -eq 0 ]] \
  || fail "mutated snapshot left an enforcement set active"
unset MACOS_SHA256_COMMAND TEST_HASH_ADAPTER TEST_HASH_STATE
assert_no_policy_temporaries

assert_status 0 macos_package_policy_load "$POLICY_FILE"
assert_prohibited_name "lib${DENIED_UPPER}.2.dylib"
assert_prohibited_name "${DENIED_TOKEN}-runtime.framework"
policy_index=0
while [[ "$policy_index" -lt "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" ]]; do
  policy_token="${MACOS_FORBIDDEN_COMPONENT_TOKENS[$policy_index]}"
  policy_token_upper="$(printf '%s' "$policy_token" | tr '[:lower:]' '[:upper:]')"
  assert_prohibited_name "prefix-${policy_token_upper}-suffix.dylib"
  policy_index=$((policy_index + 1))
done

BROKEN_NORMALIZER_DIR="${TEST_ROOT}/broken-normalizer"
mkdir -p "$BROKEN_NORMALIZER_DIR"
printf '%s\n' '#!/usr/bin/env bash' 'exit 77' > "$BROKEN_NORMALIZER_DIR/tr"
chmod +x "$BROKEN_NORMALIZER_DIR/tr"
ORIGINAL_PATH="$PATH"
PATH="${BROKEN_NORMALIZER_DIR}:${PATH}"
export PATH
assert_status 0 macos_package_policy_load "$POLICY_FILE"
policy_index=0
while [[ "$policy_index" -lt "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" ]]; do
  policy_token="${MACOS_FORBIDDEN_COMPONENT_TOKENS[$policy_index]}"
  assert_prohibited_name "prefix-${policy_token}-suffix.dylib"
  policy_index=$((policy_index + 1))
done
PATH="$ORIGINAL_PATH"
export PATH
for ordinary_runtime in \
  'libgstlibav.dylib' \
  'libavcodec.61.dylib' \
  'libgstfdkaac.dylib' \
  'libgstaudioparsers.dylib' \
  'libbluray.2.dylib' \
  'libsoup-3.0.dylib' \
  'libssl.3.dylib' \
  'libcrypto.3.dylib'; do
  assert_allowed_name "$ordinary_runtime"
done
if macos_copy_control_relative_path_is_prohibited \
    '/opt/local/libbluray/1.3.4/lib/libgstlibav.dylib'; then
  fail "ordinary codec path was overmatched"
fi
macos_copy_control_relative_path_is_prohibited \
  "/opt/local/${DENIED_TOKEN}/1.0/lib/libinnocent.dylib" \
  || fail "denied source parent path was not detected"
macos_copy_control_relative_path_is_prohibited \
  "../${DENIED_UPPER}/helper.dylib" \
  || fail "denied intermediate path component was not detected"

# The reference source exposed a copy helper here. Balun keeps this module
# inspection-only so future packaging must make its staging set explicit.
if declare -F macos_stage_gstreamer_plugin >/dev/null; then
  fail "inspection policy unexpectedly exposes a GStreamer staging helper"
fi

FAKE_OTOOL="${TEST_ROOT}/fake-otool"
cat > "$FAKE_OTOOL" <<'FAKE_OTOOL_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
mode=""
artifact=""
saw_arch_all=false
saw_mach_header=false
previous=""
for argument in "$@"; do
  case "$argument" in
    -L|-l) mode="$argument" ;;
    -h) saw_mach_header=true ;;
  esac
  if [[ "$previous" == -arch && "$argument" == all ]]; then
    saw_arch_all=true
  fi
  previous="$argument"
  artifact="$argument"
done
[[ "$saw_arch_all" == true ]] || exit 72
if [[ "$mode" == -l && "$saw_mach_header" != true ]]; then
  exit 73
fi
if [[ "$mode" == -L && "$saw_mach_header" == true ]]; then
  exit 74
fi
emit_mach_header() {
  local load_command_count="${1:-1}"
  printf 'Mach header\n'
  printf '      magic cputype cpusubtype caps filetype ncmds sizeofcmds flags\n'
  printf ' 0xfeedfacf 16777223 3 0x00 2 %s 48 0x00000000\n' \
    "$load_command_count"
}
[[ ! -e "${artifact}.otool-fail" ]] || exit 71
[[ ! -e "${artifact}.empty-output" ]] || exit 0
if [[ -e "${artifact}.oversized-output" ]]; then
  perl -e 'print "x" x ($ENV{TEST_MAX_OUTPUT_BYTES} + 1)'
  exit 0
fi
if [[ -e "${artifact}.malformed-output" ]]; then
  printf 'unstructured tool output\n'
  exit 0
fi
if [[ -e "${artifact}.invalid-text" ]]; then
  printf '\377'
  exit 0
fi
if [[ -e "${artifact}.architectures" \
    || -e "${artifact}.architecture-mismatch" \
    || -e "${artifact}.architecture-missing-header" ]]; then
  for architecture in x86_64 arm64; do
    if [[ -e "${artifact}.architecture-mismatch" \
        && "$mode" == -l && "$architecture" == arm64 ]]; then
      continue
    fi
    printf '%s (architecture %s):\n' "$artifact" "$architecture"
    if [[ "$mode" == -l ]]; then
      if [[ ! -e "${artifact}.architecture-missing-header" \
          || "$architecture" != arm64 ]]; then
        emit_mach_header
      fi
      printf 'Load command 0\n      cmd LC_RPATH\n  cmdsize 48\n     path /usr/lib (offset 12)\n'
    else
      printf '\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n'
    fi
  done
  exit 0
fi
if [[ "$mode" == -l ]]; then
  printf '%s:\n' "$artifact"
  if [[ ! -e "${artifact}.missing-mach-header" ]]; then
    if [[ -e "${artifact}.load-count-mismatch" ]]; then
      emit_mach_header 2
    else
      emit_mach_header
    fi
  fi
  if [[ -e "${artifact}.missing-cmd" ]]; then
    printf 'Load command 0\n'
  elif [[ -e "${artifact}.missing-reference" ]]; then
    printf 'Load command 0\n      cmd LC_LOAD_DYLIB\n  cmdsize 56\n'
  elif [[ -e "${artifact}.unknown-command" ]]; then
    printf 'Load command 0\n      cmd LC_NOT_A_REAL_COMMAND\n  cmdsize 48\n'
  elif [[ -e "${artifact}.malformed-load-record" ]]; then
    printf 'Load command 0\n      cmd LC_UUID\n  cmdsize 24\ngarbage\n'
  elif [[ -f "${artifact}.load-commands" ]]; then
    cat "${artifact}.load-commands"
  else
    printf 'Load command 0\n      cmd LC_RPATH\n  cmdsize 48\n     path /usr/lib (offset 12)\n'
  fi
  if [[ -e "${artifact}.mutate-content" ]]; then
    printf 'synthetic Mach-X\n' > "$artifact"
  fi
  if [[ -e "${artifact}.mutate-type" ]]; then
    rm -f "$artifact"
    mkdir "$artifact"
  fi
  if [[ -e "${artifact}.retarget-link" ]]; then
    rm -f "$TEST_RETARGET_LINK"
    ln -s "$TEST_RETARGET_TARGET" "$TEST_RETARGET_LINK"
  fi
  exit 0
fi
printf '%s:\n' "$artifact"
if [[ -e "${artifact}.unparsed-denied" ]]; then
  printf 'metadata prefix-%s-suffix\n' "$TEST_DENIED_TOKEN"
elif [[ -e "${artifact}.malformed-dependency" ]]; then
  printf '\t/usr/lib/libordinary.dylib (compatibility version invalid, current version 1.0.0)\n'
elif [[ -f "${artifact}.deps" ]]; then
  while IFS= read -r dependency || [[ -n "$dependency" ]]; do
    printf '\t%s (compatibility version 1.0.0, current version 1.0.0)\n' \
      "$dependency"
  done < "${artifact}.deps"
else
  printf '\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)\n'
fi
FAKE_OTOOL_SCRIPT
chmod +x "$FAKE_OTOOL"
MACOS_OTOOL_COMMAND="$FAKE_OTOOL"
TEST_DENIED_TOKEN="$DENIED_TOKEN"
TEST_MAX_OUTPUT_BYTES="$MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES"
export MACOS_OTOOL_COMMAND TEST_DENIED_TOKEN TEST_MAX_OUTPUT_BYTES

ARTIFACTS="${TEST_ROOT}/artifacts"
mkdir -p "$ARTIFACTS"
SAFE_ARTIFACT="${ARTIFACTS}/libordinary.dylib"
printf 'synthetic Mach-O\n' > "$SAFE_ARTIFACT"
printf '%s\n' \
  '/opt/local/lib/libavcodec.61.dylib' \
  '/opt/local/lib/libcrypto.3.dylib' \
  > "${SAFE_ARTIFACT}.deps"
assert_status 0 macos_validate_macho_copy_control "$SAFE_ARTIFACT"
[[ "$MACOS_PACKAGE_POLICY_RESULT" == allowed ]] \
  || fail "ordinary synthetic Mach-O was not marked allowed"
assert_no_policy_temporaries

ARCHITECTURE_ARTIFACT="${ARTIFACTS}/libarchitectures.dylib"
printf 'synthetic Mach-O\n' > "$ARCHITECTURE_ARTIFACT"
touch "${ARCHITECTURE_ARTIFACT}.architectures"
assert_status 0 macos_validate_macho_copy_control "$ARCHITECTURE_ARTIFACT"
assert_no_policy_temporaries

ARCHITECTURE_MISMATCH_ARTIFACT="${ARTIFACTS}/libarchitecture-mismatch.dylib"
printf 'synthetic Mach-O\n' > "$ARCHITECTURE_MISMATCH_ARTIFACT"
touch "${ARCHITECTURE_MISMATCH_ARTIFACT}.architecture-mismatch"
assert_status 2 macos_validate_macho_copy_control "$ARCHITECTURE_MISMATCH_ARTIFACT"
assert_reason_contains 'disagree on architecture coverage'
assert_no_policy_temporaries

ARCHITECTURE_HEADER_ARTIFACT="${ARTIFACTS}/libarchitecture-missing-header.dylib"
printf 'synthetic Mach-O\n' > "$ARCHITECTURE_HEADER_ARTIFACT"
touch "${ARCHITECTURE_HEADER_ARTIFACT}.architecture-missing-header"
assert_status 2 macos_validate_macho_copy_control "$ARCHITECTURE_HEADER_ARTIFACT"
assert_reason_contains 'omits its Mach header preamble'
assert_no_policy_temporaries

NAMED_ARTIFACT="${ARTIFACTS}/lib${DENIED_TOKEN}.dylib"
printf 'synthetic Mach-O\n' > "$NAMED_ARTIFACT"
assert_status 1 macos_validate_macho_copy_control "$NAMED_ARTIFACT"

SOURCE_PATH_ARTIFACT=\
"${TEST_ROOT}/${DENIED_TOKEN}-source/libinnocent-source.dylib"
mkdir -p "$(dirname "$SOURCE_PATH_ARTIFACT")"
printf 'synthetic Mach-O\n' > "$SOURCE_PATH_ARTIFACT"
assert_status 1 macos_validate_macho_copy_control "$SOURCE_PATH_ARTIFACT"

IMPORTED_ARTIFACT="${ARTIFACTS}/libinnocent-import.dylib"
printf 'synthetic Mach-O\n' > "$IMPORTED_ARTIFACT"
printf '@rpath/lib%s.0.dylib\n' "$DENIED_TOKEN" \
  > "${IMPORTED_ARTIFACT}.deps"
assert_status 1 macos_validate_macho_copy_control "$IMPORTED_ARTIFACT"

LOAD_PATH_ARTIFACT="${ARTIFACTS}/libinnocent-load.dylib"
printf 'synthetic Mach-O\n' > "$LOAD_PATH_ARTIFACT"
printf '%s\n' \
  'Load command 0' \
  '          cmd LC_RPATH' \
  '      cmdsize 48' \
  "         path @loader_path/../${DENIED_TOKEN} (offset 12)" \
  > "${LOAD_PATH_ARTIFACT}.load-commands"
assert_status 1 macos_validate_macho_copy_control "$LOAD_PATH_ARTIFACT"

UNINSPECTABLE_ARTIFACT="${ARTIFACTS}/libuninspectable.dylib"
printf 'synthetic Mach-O\n' > "$UNINSPECTABLE_ARTIFACT"
touch "${UNINSPECTABLE_ARTIFACT}.otool-fail"
assert_status 2 macos_validate_macho_copy_control "$UNINSPECTABLE_ARTIFACT"
assert_no_policy_temporaries

INVALID_TEXT_ARTIFACT="${ARTIFACTS}/libinvalid-text.dylib"
printf 'synthetic Mach-O\n' > "$INVALID_TEXT_ARTIFACT"
touch "${INVALID_TEXT_ARTIFACT}.invalid-text"
assert_status 2 macos_validate_macho_copy_control "$INVALID_TEXT_ARTIFACT"
assert_reason_contains 'not valid UTF-8 text'
assert_no_policy_temporaries

EMPTY_OUTPUT_ARTIFACT="${ARTIFACTS}/libempty-output.dylib"
printf 'synthetic Mach-O\n' > "$EMPTY_OUTPUT_ARTIFACT"
touch "${EMPTY_OUTPUT_ARTIFACT}.empty-output"
assert_status 2 macos_validate_macho_copy_control "$EMPTY_OUTPUT_ARTIFACT"
assert_reason_contains 'no supported artifact header'
assert_no_policy_temporaries

MALFORMED_OUTPUT_ARTIFACT="${ARTIFACTS}/libmalformed-output.dylib"
printf 'synthetic Mach-O\n' > "$MALFORMED_OUTPUT_ARTIFACT"
touch "${MALFORMED_OUTPUT_ARTIFACT}.malformed-output"
assert_status 2 macos_validate_macho_copy_control "$MALFORMED_OUTPUT_ARTIFACT"
assert_reason_contains 'unsupported record'
assert_no_policy_temporaries

MALFORMED_DEPENDENCY_ARTIFACT="${ARTIFACTS}/libmalformed-dependency.dylib"
printf 'synthetic Mach-O\n' > "$MALFORMED_DEPENDENCY_ARTIFACT"
touch "${MALFORMED_DEPENDENCY_ARTIFACT}.malformed-dependency"
assert_status 2 macos_validate_macho_copy_control "$MALFORMED_DEPENDENCY_ARTIFACT"
assert_reason_contains 'malformed dependency record'
assert_no_policy_temporaries

MISSING_HEADER_ARTIFACT="${ARTIFACTS}/libmissing-mach-header.dylib"
printf 'synthetic Mach-O\n' > "$MISSING_HEADER_ARTIFACT"
touch "${MISSING_HEADER_ARTIFACT}.missing-mach-header"
assert_status 2 macos_validate_macho_copy_control "$MISSING_HEADER_ARTIFACT"
assert_reason_contains 'omits its Mach header preamble'
assert_no_policy_temporaries

LOAD_COUNT_ARTIFACT="${ARTIFACTS}/libload-count-mismatch.dylib"
printf 'synthetic Mach-O\n' > "$LOAD_COUNT_ARTIFACT"
touch "${LOAD_COUNT_ARTIFACT}.load-count-mismatch"
assert_status 2 macos_validate_macho_copy_control "$LOAD_COUNT_ARTIFACT"
assert_reason_contains 'load-command output is incomplete'
assert_no_policy_temporaries

MISSING_CMD_ARTIFACT="${ARTIFACTS}/libmissing-cmd.dylib"
printf 'synthetic Mach-O\n' > "$MISSING_CMD_ARTIFACT"
touch "${MISSING_CMD_ARTIFACT}.missing-cmd"
assert_status 2 macos_validate_macho_copy_control "$MISSING_CMD_ARTIFACT"
assert_reason_contains 'missing cmd'
assert_no_policy_temporaries

MISSING_REFERENCE_ARTIFACT="${ARTIFACTS}/libmissing-reference.dylib"
printf 'synthetic Mach-O\n' > "$MISSING_REFERENCE_ARTIFACT"
touch "${MISSING_REFERENCE_ARTIFACT}.missing-reference"
assert_status 2 macos_validate_macho_copy_control "$MISSING_REFERENCE_ARTIFACT"
assert_reason_contains 'omits its required name record'
assert_no_policy_temporaries

UNKNOWN_COMMAND_ARTIFACT="${ARTIFACTS}/libunknown-command.dylib"
printf 'synthetic Mach-O\n' > "$UNKNOWN_COMMAND_ARTIFACT"
touch "${UNKNOWN_COMMAND_ARTIFACT}.unknown-command"
assert_status 2 macos_validate_macho_copy_control "$UNKNOWN_COMMAND_ARTIFACT"
assert_reason_contains 'invalid cmd record'
assert_no_policy_temporaries

MALFORMED_LOAD_ARTIFACT="${ARTIFACTS}/libmalformed-load-record.dylib"
printf 'synthetic Mach-O\n' > "$MALFORMED_LOAD_ARTIFACT"
touch "${MALFORMED_LOAD_ARTIFACT}.malformed-load-record"
assert_status 2 macos_validate_macho_copy_control "$MALFORMED_LOAD_ARTIFACT"
assert_reason_contains 'unsupported metadata record'
assert_no_policy_temporaries

UNPARSED_DENIED_ARTIFACT="${ARTIFACTS}/libunparsed-record.dylib"
printf 'synthetic Mach-O\n' > "$UNPARSED_DENIED_ARTIFACT"
touch "${UNPARSED_DENIED_ARTIFACT}.unparsed-denied"
assert_status 1 macos_validate_macho_copy_control "$UNPARSED_DENIED_ARTIFACT"
assert_reason_contains 'import record containing forbidden token'
assert_no_policy_temporaries

OVERSIZED_OUTPUT_ARTIFACT="${ARTIFACTS}/liboversized-output.dylib"
printf 'synthetic Mach-O\n' > "$OVERSIZED_OUTPUT_ARTIFACT"
touch "${OVERSIZED_OUTPUT_ARTIFACT}.oversized-output"
assert_status 2 macos_validate_macho_copy_control "$OVERSIZED_OUTPUT_ARTIFACT"
assert_reason_contains 'exceeds the'
assert_no_policy_temporaries

TIMEOUT_OUTPUT="${TEST_ROOT}/timeout-output"
assert_status 2 macos_package_policy_capture_output \
  'bounded timeout probe' "$TIMEOUT_OUTPUT" 1024 1 sleep 5
assert_reason_contains 'could not collect bounded timeout probe'
rm -f "$TIMEOUT_OUTPUT"

RAISE_LIMIT_PRODUCER="${TEST_ROOT}/raise-file-limit"
cat > "$RAISE_LIMIT_PRODUCER" <<'RAISE_LIMIT_SCRIPT'
#!/usr/bin/env bash
set -u
ulimit -S -f unlimited 2>/dev/null || true
head -c 1048576 /dev/zero
RAISE_LIMIT_SCRIPT
chmod +x "$RAISE_LIMIT_PRODUCER"
RAISE_LIMIT_OUTPUT="${TEST_ROOT}/raise-limit-output"
assert_status 2 macos_package_policy_capture_output \
  'hard file-limit probe' "$RAISE_LIMIT_OUTPUT" 1024 5 "$RAISE_LIMIT_PRODUCER"
raise_limit_bytes="$(wc -c < "$RAISE_LIMIT_OUTPUT")"
[[ "$raise_limit_bytes" -le 3072 ]] \
  || fail "producer bypassed bounded capture and wrote ${raise_limit_bytes} bytes"
rm -f "$RAISE_LIMIT_OUTPUT"

DESCENDANT_OUTPUT="${TEST_ROOT}/descendant-output"
assert_status 0 macos_package_policy_capture_output \
  'producer descendant probe' "$DESCENDANT_OUTPUT" 1024 5 perl -e '
    my $child = fork();
    exit 77 unless defined $child;
    exit 0 if $child;
    select undef, undef, undef, 0.5;
    print "late output";
  '
sleep 1
[[ ! -s "$DESCENDANT_OUTPUT" ]] \
  || fail 'producer descendant wrote after bounded capture returned'
rm -f "$DESCENDANT_OUTPUT"

# Invalid tool output fails closed but does not authorize anything; reload the
# reviewed policy before exercising whole-tree validation.
assert_status 0 macos_package_policy_load "$POLICY_FILE"

make_bundle() {
  local root="$1"
  local bundle_name
  bundle_name="${root##*/}"
  bundle_name="${bundle_name%.app}"
  mkdir -p \
    "$root/Contents/MacOS" \
    "$root/Contents/Frameworks" \
    "$root/Contents/Resources/lib/modules"
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' \
    > "$root/Contents/MacOS/${bundle_name}"
}

SAFE_BUNDLE="${TEST_ROOT}/Safe.app"
make_bundle "$SAFE_BUNDLE"
printf 'synthetic Mach-O\n' > "$SAFE_BUNDLE/Contents/MacOS/Safe-bin"
printf 'synthetic Mach-O\n' > "$SAFE_BUNDLE/Contents/Frameworks/libavcodec.61.dylib"
printf 'synthetic Mach-O\n' \
  > "$SAFE_BUNDLE/Contents/Resources/lib/modules/libgstlibav.dylib"
mkdir -p "$SAFE_BUNDLE/Contents/Resources/ordinary-codecs"
printf 'data\n' > "$SAFE_BUNDLE/Contents/Resources/ordinary-codecs/helper.dat"
ln -s 'ordinary-codecs/helper.dat' \
  "$SAFE_BUNDLE/Contents/Resources/ordinary-runtime-link"
assert_status 0 macos_validate_bundle_copy_control "$SAFE_BUNDLE"
[[ "$MACOS_PACKAGE_POLICY_RESULT" == allowed ]] \
  || fail "safe synthetic bundle was not marked allowed"
assert_no_policy_temporaries

SAFE_PARENT_BUNDLE="${TEST_ROOT}/${DENIED_TOKEN}-parent/SafeParent.app"
make_bundle "$SAFE_PARENT_BUNDLE"
printf 'synthetic Mach-O\n' \
  > "$SAFE_PARENT_BUNDLE/Contents/Frameworks/libgstlibav.dylib"
assert_status 0 macos_validate_bundle_copy_control "$SAFE_PARENT_BUNDLE"
assert_no_policy_temporaries

DENIED_ROOT_BUNDLE="${TEST_ROOT}/${DENIED_TOKEN}-viewer.app"
make_bundle "$DENIED_ROOT_BUNDLE"
assert_status 1 macos_validate_bundle_copy_control "$DENIED_ROOT_BUNDLE"

REAL_PERL_COMMAND="${MACOS_PERL_COMMAND:-perl}"
MACOS_PERL_COMMAND=false
assert_status 2 macos_validate_bundle_copy_control "$SAFE_BUNDLE"
assert_reason_contains 'could not collect macOS bundle content manifest'
MACOS_PERL_COMMAND=true
assert_status 2 macos_validate_bundle_copy_control "$SAFE_BUNDLE"
assert_reason_contains 'omits its root or Contents directory'
MACOS_PERL_COMMAND="$REAL_PERL_COMMAND"
assert_no_policy_temporaries

PARTIAL_MANIFEST="${TEST_ROOT}/partial-bundle-manifest"
printf 'D\t.\t40755\t0\t0\t1\t1\t2\t0\t0\t0' > "$PARTIAL_MANIFEST"
assert_status 2 macos_validate_bundle_member_manifest "$PARTIAL_MANIFEST"
assert_reason_contains 'not completely NUL-delimited'

UNSUPPORTED_MANIFEST="${TEST_ROOT}/unsupported-bundle-manifest"
printf 'X\tContents\000' > "$UNSUPPORTED_MANIFEST"
assert_status 2 macos_validate_bundle_member_manifest "$UNSUPPORTED_MANIFEST"
assert_reason_contains 'unsupported entry type'

NONCANONICAL_MANIFEST="${TEST_ROOT}/noncanonical-bundle-manifest"
printf 'D\tContents/..\t40755\t0\t0\t1\t1\t2\t0\t0\t0\000' \
  > "$NONCANONICAL_MANIFEST"
assert_status 2 macos_validate_bundle_member_manifest "$NONCANONICAL_MANIFEST"
assert_reason_contains 'non-canonical member path'

TYPE_MISMATCH_MANIFEST="${TEST_ROOT}/type-mismatch-bundle-manifest"
printf 'D\tContents\t100755\t0\t0\t1\t1\t1\t0\t0\t0\000' \
  > "$TYPE_MISMATCH_MANIFEST"
assert_status 2 macos_validate_bundle_member_manifest "$TYPE_MISMATCH_MANIFEST"
assert_reason_contains 'malformed directory record'

ROOT_ESCAPE_MANIFEST="${TEST_ROOT}/root-escape-bundle-manifest"
printf 'L\talias\t120777\t0\t0\t1\t1\t1\t2\t0\t0\t..\tContents\000' \
  > "$ROOT_ESCAPE_MANIFEST"
assert_status 2 macos_validate_bundle_member_manifest "$ROOT_ESCAPE_MANIFEST"
assert_reason_contains 'unsafe symlink record'

ABSOLUTE_LINK_BUNDLE="${TEST_ROOT}/AbsoluteLink.app"
make_bundle "$ABSOLUTE_LINK_BUNDLE"
printf 'data\n' > "$ABSOLUTE_LINK_BUNDLE/Contents/Resources/target.dat"
ln -s "$ABSOLUTE_LINK_BUNDLE/Contents/Resources/target.dat" \
  "$ABSOLUTE_LINK_BUNDLE/Contents/Resources/absolute-link"
assert_status 2 macos_validate_bundle_copy_control "$ABSOLUTE_LINK_BUNDLE"
assert_no_policy_temporaries

printf 'outside data\n' > "$TEST_ROOT/outside-target.dat"
ESCAPING_LINK_BUNDLE="${TEST_ROOT}/EscapingLink.app"
make_bundle "$ESCAPING_LINK_BUNDLE"
ln -s '../../../outside-target.dat' \
  "$ESCAPING_LINK_BUNDLE/Contents/Resources/escaping-link"
assert_status 2 macos_validate_bundle_copy_control "$ESCAPING_LINK_BUNDLE"
assert_no_policy_temporaries

DANGLING_LINK_BUNDLE="${TEST_ROOT}/DanglingLink.app"
make_bundle "$DANGLING_LINK_BUNDLE"
ln -s 'missing-target.dat' \
  "$DANGLING_LINK_BUNDLE/Contents/Resources/dangling-link"
assert_status 2 macos_validate_bundle_copy_control "$DANGLING_LINK_BUNDLE"
assert_no_policy_temporaries

CONTROL_LINK_BUNDLE="${TEST_ROOT}/ControlLink.app"
make_bundle "$CONTROL_LINK_BUNDLE"
ln -s $'ordinary-target.dat\n' \
  "$CONTROL_LINK_BUNDLE/Contents/Resources/control-link"
assert_status 2 macos_validate_bundle_copy_control "$CONTROL_LINK_BUNDLE"
assert_no_policy_temporaries

FIFO_BUNDLE="${TEST_ROOT}/Fifo.app"
make_bundle "$FIFO_BUNDLE"
mkfifo "$FIFO_BUNDLE/Contents/Resources/runtime-pipe"
assert_status 2 macos_validate_bundle_copy_control "$FIFO_BUNDLE"
assert_no_policy_temporaries

HARDLINK_BUNDLE="${TEST_ROOT}/Hardlink.app"
make_bundle "$HARDLINK_BUNDLE"
printf 'data\n' > "$HARDLINK_BUNDLE/Contents/Resources/original.dat"
ln "$HARDLINK_BUNDLE/Contents/Resources/original.dat" \
  "$HARDLINK_BUNDLE/Contents/Resources/second-name.dat"
assert_status 2 macos_validate_bundle_copy_control "$HARDLINK_BUNDLE"
assert_no_policy_temporaries

CONTENT_MUTATION_BUNDLE="${TEST_ROOT}/ContentMutation.app"
make_bundle "$CONTENT_MUTATION_BUNDLE"
CONTENT_MUTATION_ARTIFACT=\
"$CONTENT_MUTATION_BUNDLE/Contents/Frameworks/libordinary.dylib"
printf 'synthetic Mach-O\n' > "$CONTENT_MUTATION_ARTIFACT"
touch "${CONTENT_MUTATION_ARTIFACT}.mutate-content"
assert_status 2 macos_validate_bundle_copy_control "$CONTENT_MUTATION_BUNDLE"
assert_reason_contains 'changed during component-policy validation'
assert_no_policy_temporaries

TYPE_MUTATION_BUNDLE="${TEST_ROOT}/TypeMutation.app"
make_bundle "$TYPE_MUTATION_BUNDLE"
TYPE_MUTATION_ARTIFACT="$TYPE_MUTATION_BUNDLE/Contents/Frameworks/libordinary.dylib"
printf 'synthetic Mach-O\n' > "$TYPE_MUTATION_ARTIFACT"
touch "${TYPE_MUTATION_ARTIFACT}.mutate-type"
assert_status 2 macos_validate_bundle_copy_control "$TYPE_MUTATION_BUNDLE"
assert_reason_contains 'changed during component-policy validation'
assert_no_policy_temporaries

RETARGET_BUNDLE="${TEST_ROOT}/Retarget.app"
make_bundle "$RETARGET_BUNDLE"
RETARGET_ARTIFACT="$RETARGET_BUNDLE/Contents/Frameworks/libordinary.dylib"
printf 'synthetic Mach-O\n' > "$RETARGET_ARTIFACT"
printf 'one\n' > "$RETARGET_BUNDLE/Contents/Resources/target-one.dat"
printf 'two\n' > "$RETARGET_BUNDLE/Contents/Resources/target-two.dat"
TEST_RETARGET_LINK="$RETARGET_BUNDLE/Contents/Resources/runtime-link"
TEST_RETARGET_TARGET='target-two.dat'
ln -s 'target-one.dat' "$TEST_RETARGET_LINK"
touch "${RETARGET_ARTIFACT}.retarget-link"
export TEST_RETARGET_LINK TEST_RETARGET_TARGET
assert_status 2 macos_validate_bundle_copy_control "$RETARGET_BUNDLE"
assert_reason_contains 'changed during component-policy validation'
unset TEST_RETARGET_LINK TEST_RETARGET_TARGET
assert_no_policy_temporaries

NAMED_BUNDLE="${TEST_ROOT}/Named.app"
make_bundle "$NAMED_BUNDLE"
printf 'synthetic Mach-O\n' \
  > "$NAMED_BUNDLE/Contents/Frameworks/lib${DENIED_TOKEN}.dylib"
assert_status 1 macos_validate_bundle_copy_control "$NAMED_BUNDLE"

DIRECTORY_BUNDLE="${TEST_ROOT}/Directory.app"
make_bundle "$DIRECTORY_BUNDLE"
mkdir -p "$DIRECTORY_BUNDLE/Contents/Resources/${DENIED_UPPER}.framework"
printf 'data\n' \
  > "$DIRECTORY_BUNDLE/Contents/Resources/${DENIED_UPPER}.framework/helper.dat"
assert_status 1 macos_validate_bundle_copy_control "$DIRECTORY_BUNDLE"

IMPORTED_BUNDLE="${TEST_ROOT}/Imported.app"
make_bundle "$IMPORTED_BUNDLE"
printf 'synthetic Mach-O\n' \
  > "$IMPORTED_BUNDLE/Contents/Frameworks/libinnocent.dylib"
printf '@rpath/lib%s.0.dylib\n' "$DENIED_TOKEN" \
  > "$IMPORTED_BUNDLE/Contents/Frameworks/libinnocent.dylib.deps"
assert_status 1 macos_validate_bundle_copy_control "$IMPORTED_BUNDLE"

RESOURCE_MACHO_BUNDLE="${TEST_ROOT}/ResourceMachO.app"
make_bundle "$RESOURCE_MACHO_BUNDLE"
printf '\317\372\355\376' \
  > "$RESOURCE_MACHO_BUNDLE/Contents/Resources/allowed-helper.dat"
printf '@rpath/lib%s.0.dylib\n' "$DENIED_TOKEN" \
  > "$RESOURCE_MACHO_BUNDLE/Contents/Resources/allowed-helper.dat.deps"
assert_status 1 macos_validate_bundle_copy_control "$RESOURCE_MACHO_BUNDLE"

FRAMEWORK_MACHO_BUNDLE="${TEST_ROOT}/FrameworkMachO.app"
make_bundle "$FRAMEWORK_MACHO_BUNDLE"
printf '\317\372\355\376' \
  > "$FRAMEWORK_MACHO_BUNDLE/Contents/Frameworks/allowed-helper.dat"
printf '@rpath/lib%s.0.dylib\n' "$DENIED_TOKEN" \
  > "$FRAMEWORK_MACHO_BUNDLE/Contents/Frameworks/allowed-helper.dat.deps"
assert_status 1 macos_validate_bundle_copy_control "$FRAMEWORK_MACHO_BUNDLE"

WRAPPER_MACHO_BUNDLE="${TEST_ROOT}/WrapperMachO.app"
make_bundle "$WRAPPER_MACHO_BUNDLE"
printf '\317\372\355\376' \
  > "$WRAPPER_MACHO_BUNDLE/Contents/MacOS/WrapperMachO"
printf '@rpath/lib%s.0.dylib\n' "$DENIED_TOKEN" \
  > "$WRAPPER_MACHO_BUNDLE/Contents/MacOS/WrapperMachO.deps"
assert_status 1 macos_validate_bundle_copy_control "$WRAPPER_MACHO_BUNDLE"

SYMLINK_BUNDLE="${TEST_ROOT}/Symlink.app"
make_bundle "$SYMLINK_BUNDLE"
mkdir -p "$SYMLINK_BUNDLE/Contents/${DENIED_TOKEN}"
printf 'synthetic target\n' \
  > "$SYMLINK_BUNDLE/Contents/${DENIED_TOKEN}/helper.dylib"
ln -s "../${DENIED_TOKEN}/helper.dylib" \
  "$SYMLINK_BUNDLE/Contents/Resources/runtime-link"
assert_status 1 macos_validate_bundle_copy_control "$SYMLINK_BUNDLE"

UNINSPECTABLE_BUNDLE="${TEST_ROOT}/Uninspectable.app"
make_bundle "$UNINSPECTABLE_BUNDLE"
printf 'synthetic Mach-O\n' \
  > "$UNINSPECTABLE_BUNDLE/Contents/Frameworks/libordinary.dylib"
touch "$UNINSPECTABLE_BUNDLE/Contents/Frameworks/libordinary.dylib.otool-fail"
assert_status 2 macos_validate_bundle_copy_control "$UNINSPECTABLE_BUNDLE"
assert_no_policy_temporaries

echo "ok - macOS component policy inspection rejects denied bundle members without staging a package"
