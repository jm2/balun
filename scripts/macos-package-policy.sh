#!/usr/bin/env bash
# Inspection-only helpers for enforcing Balun's macOS bundle component policy.
#
# This preparatory module validates synthetic or completed bundle trees. It does
# not build a Balun application bundle and intentionally exposes no helper that
# copies or stages GStreamer plugins.

MACOS_PACKAGE_POLICY_REASON=""
MACOS_PACKAGE_POLICY_RESULT=""
MACOS_PACKAGE_POLICY_MATCHED_TOKEN=""
MACOS_PACKAGE_POLICY_DIGEST=""
MACOS_PACKAGE_POLICY_NORMALIZED=""
MACOS_PACKAGE_POLICY_TRIMMED=""
MACOS_PACKAGE_POLICY_MANIFEST_KIND=""
MACOS_PACKAGE_POLICY_MANIFEST_PATH=""
MACOS_PACKAGE_POLICY_MANIFEST_EXECUTABLE=""
MACOS_PACKAGE_POLICY_MANIFEST_MAGIC=""
MACOS_PACKAGE_POLICY_MANIFEST_LINK_TARGET=""
MACOS_PACKAGE_POLICY_MANIFEST_RESOLVED_TARGET=""
MACOS_FORBIDDEN_COMPONENT_TOKENS=()
MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0

readonly MACOS_PACKAGE_POLICY_EXPECTED_SHA256="844f3ab37329b0785cf82ae8c29c6665f5052998ea3def790630b239408c8bed"
readonly MACOS_PACKAGE_POLICY_MAX_BYTES=65536
readonly MACOS_PACKAGE_POLICY_MAX_LINES=1024
readonly MACOS_PACKAGE_POLICY_MAX_TOKENS=256
readonly MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES=8388608
readonly MACOS_PACKAGE_POLICY_MAX_OUTPUT_LINES=131072
readonly MACOS_PACKAGE_POLICY_MAX_MANIFEST_BYTES=67108864
readonly MACOS_PACKAGE_POLICY_MAX_BUNDLE_ENTRIES=100000
readonly MACOS_PACKAGE_POLICY_MAX_BUNDLE_DEPTH=128
readonly MACOS_PACKAGE_POLICY_MAX_IMPORT_CANDIDATES=8192
readonly MACOS_PACKAGE_POLICY_MAX_REGULAR_FILE_BYTES=2147483648
readonly MACOS_PACKAGE_POLICY_MAX_REGULAR_BYTES=8589934592
readonly MACOS_PACKAGE_POLICY_MAX_REFERENCE_BYTES=4096
readonly MACOS_PACKAGE_POLICY_MAX_TOOL_SECONDS=120
readonly MACOS_PACKAGE_POLICY_MAX_BUNDLE_IMPORT_SECONDS=300

# Bash 3.2 has no ${value,,} expansion. Keep case folding inside Bash so a
# missing or failing external normalizer cannot turn a denied name into an
# allowed one. The policy alphabet and all tokens being matched are ASCII.
macos_package_policy_ascii_lower() {
  local value="$1"

  value="${value//A/a}"
  value="${value//B/b}"
  value="${value//C/c}"
  value="${value//D/d}"
  value="${value//E/e}"
  value="${value//F/f}"
  value="${value//G/g}"
  value="${value//H/h}"
  value="${value//I/i}"
  value="${value//J/j}"
  value="${value//K/k}"
  value="${value//L/l}"
  value="${value//M/m}"
  value="${value//N/n}"
  value="${value//O/o}"
  value="${value//P/p}"
  value="${value//Q/q}"
  value="${value//R/r}"
  value="${value//S/s}"
  value="${value//T/t}"
  value="${value//U/u}"
  value="${value//V/v}"
  value="${value//W/w}"
  value="${value//X/x}"
  value="${value//Y/y}"
  value="${value//Z/z}"
  MACOS_PACKAGE_POLICY_NORMALIZED="$value"
}

macos_package_policy_trim_ascii_space() {
  local value="$1"
  local LC_ALL=C
  export LC_ALL

  while [[ "$value" == [[:space:]]* ]]; do
    value="${value#?}"
  done
  while [[ "$value" == *[[:space:]] ]]; do
    value="${value%?}"
  done
  MACOS_PACKAGE_POLICY_TRIMMED="$value"
}

macos_package_policy_default_file() {
  local helper_dir
  helper_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
  printf '%s\n' "${helper_dir}/../build-aux/packaging/forbidden-bundled-components.txt"
}

macos_package_policy_reset() {
  MACOS_PACKAGE_POLICY_REASON=""
  MACOS_PACKAGE_POLICY_RESULT=""
  MACOS_PACKAGE_POLICY_MATCHED_TOKEN=""
  MACOS_PACKAGE_POLICY_DIGEST=""
  MACOS_PACKAGE_POLICY_NORMALIZED=""
  MACOS_PACKAGE_POLICY_TRIMMED=""
  MACOS_FORBIDDEN_COMPONENT_TOKENS=()
  MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0
}

macos_package_policy_setup_error() {
  MACOS_FORBIDDEN_COMPONENT_TOKENS=()
  MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0
  MACOS_PACKAGE_POLICY_MATCHED_TOKEN=""
  MACOS_PACKAGE_POLICY_DIGEST=""
  MACOS_PACKAGE_POLICY_REASON="$1"
  MACOS_PACKAGE_POLICY_RESULT="error"
  return 2
}

macos_package_policy_check_utf8_text() {
  local label="$1"
  local input="$2"
  local perl_command="${MACOS_PERL_COMMAND:-perl}"
  local status=0

  if ! command -v "$perl_command" >/dev/null 2>&1; then
    MACOS_PACKAGE_POLICY_REASON=\
"required UTF-8 inspection tool is unavailable: ${perl_command}"
    MACOS_PACKAGE_POLICY_RESULT="error"
    return 2
  fi

  "$perl_command" -MEncode=decode,FB_CROAK -e '
    use strict;
    use warnings;
    my $path = shift;
    open my $handle, "<:raw", $path or exit 12;
    local $/;
    my $bytes = <$handle>;
    if (!defined $bytes) {
      exit 12 unless eof $handle;
      $bytes = "";
    }
    close $handle or exit 12;
    exit 10 if index($bytes, "\0") >= 0;
    eval { decode("UTF-8", $bytes, FB_CROAK); 1 } or exit 11;
  ' -- "$input" || status=$?

  case "$status" in
    0) return 0 ;;
    10) MACOS_PACKAGE_POLICY_REASON="$label contains a NUL byte: $input" ;;
    11) MACOS_PACKAGE_POLICY_REASON="$label is not valid UTF-8 text: $input" ;;
    *) MACOS_PACKAGE_POLICY_REASON="could not validate $label as UTF-8 text: $input" ;;
  esac
  MACOS_PACKAGE_POLICY_RESULT="error"
  return 2
}

macos_package_policy_sha256() {
  local input="$1"
  local output digest ignored

  MACOS_PACKAGE_POLICY_DIGEST=""
  if [[ -n "${MACOS_SHA256_COMMAND:-}" ]]; then
    if ! command -v "$MACOS_SHA256_COMMAND" >/dev/null 2>&1; then
      macos_package_policy_setup_error \
        "configured SHA-256 command is unavailable: ${MACOS_SHA256_COMMAND}"
      return 2
    fi
    if ! output="$("$MACOS_SHA256_COMMAND" "$input" 2>/dev/null)"; then
      macos_package_policy_setup_error "could not hash policy snapshot: $input"
      return 2
    fi
  elif command -v sha256sum >/dev/null 2>&1; then
    if ! output="$(sha256sum -- "$input" 2>/dev/null)"; then
      macos_package_policy_setup_error "could not hash policy snapshot: $input"
      return 2
    fi
  elif command -v shasum >/dev/null 2>&1; then
    if ! output="$(shasum -a 256 "$input" 2>/dev/null)"; then
      macos_package_policy_setup_error "could not hash policy snapshot: $input"
      return 2
    fi
  else
    macos_package_policy_setup_error "required SHA-256 inspection tool is unavailable"
    return 2
  fi

  [[ "$output" != *$'\n'* ]] || {
    macos_package_policy_setup_error "SHA-256 command returned multiple records"
    return 2
  }
  read -r digest ignored <<< "$output"
  if [[ ! "$digest" =~ ^[0-9A-Fa-f]{64}$ ]]; then
    macos_package_policy_setup_error "SHA-256 command returned an invalid digest"
    return 2
  fi
  macos_package_policy_ascii_lower "$digest"
  MACOS_PACKAGE_POLICY_DIGEST="$MACOS_PACKAGE_POLICY_NORMALIZED"
}

macos_package_policy_parse_snapshot() {
  local policy_snapshot="$1"
  local policy_file="$2"
  local policy_lines=0
  local line token existing token_index

  MACOS_FORBIDDEN_COMPONENT_TOKENS=()
  MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    policy_lines=$((policy_lines + 1))
    if [[ "$policy_lines" -gt "$MACOS_PACKAGE_POLICY_MAX_LINES" ]]; then
      macos_package_policy_setup_error \
        "policy contains more than ${MACOS_PACKAGE_POLICY_MAX_LINES} lines"
      return 2
    fi
    if [[ "${#line}" -gt 1024 ]]; then
      macos_package_policy_setup_error "policy contains an overlong line"
      return 2
    fi
    token="${line%$'\r'}"
    macos_package_policy_trim_ascii_space "$token"
    token="$MACOS_PACKAGE_POLICY_TRIMMED"
    [[ -z "$token" || "$token" == \#* ]] && continue
    if [[ "${#token}" -gt 64 ]]; then
      macos_package_policy_setup_error "policy contains an overlong token"
      return 2
    fi
    if [[ ! "$token" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]]; then
      macos_package_policy_setup_error \
        "policy contains an invalid filename token: ${token}"
      return 2
    fi
    macos_package_policy_ascii_lower "$token"
    token="$MACOS_PACKAGE_POLICY_NORMALIZED"
    token_index=0
    while [[ "$token_index" -lt "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" ]]; do
      existing="${MACOS_FORBIDDEN_COMPONENT_TOKENS[$token_index]}"
      if [[ "$existing" == "$token" ]]; then
        macos_package_policy_setup_error \
          "policy contains a duplicate filename token: ${token}"
        return 2
      fi
      token_index=$((token_index + 1))
    done
    MACOS_FORBIDDEN_COMPONENT_TOKENS[$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT]="$token"
    MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT=$((MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT + 1))
    if [[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" \
        -gt "$MACOS_PACKAGE_POLICY_MAX_TOKENS" ]]; then
      macos_package_policy_setup_error \
        "policy contains more than ${MACOS_PACKAGE_POLICY_MAX_TOKENS} filename tokens"
      return 2
    fi
  done < "$policy_snapshot"

  if [[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -eq 0 ]]; then
    macos_package_policy_setup_error \
      "policy contains no filename tokens: ${policy_file}"
    return 2
  fi
}

macos_package_policy_load() {
  local policy_file="${1:-${BALUN_FORBIDDEN_COMPONENTS_FILE:-}}"
  local policy_snapshot policy_bytes
  local digest_before digest_after old_umask
  local LC_ALL=C
  export LC_ALL

  [[ -n "$policy_file" ]] || policy_file="$(macos_package_policy_default_file)"
  macos_package_policy_reset

  if [[ ! -f "$policy_file" || -L "$policy_file" || "$policy_file" == -* ]]; then
    macos_package_policy_setup_error \
      "required bundled-component policy is missing or is not a regular file: ${policy_file}"
    return 2
  fi

  old_umask="$(umask)"
  umask 077
  if ! policy_snapshot="$(mktemp "${TMPDIR:-/tmp}/balun-macos-component-policy.XXXXXX")"; then
    umask "$old_umask"
    macos_package_policy_setup_error "could not create a private policy snapshot"
    return 2
  fi
  umask "$old_umask"

  if ! cp "$policy_file" "$policy_snapshot"; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error "could not snapshot policy: ${policy_file}"
    return 2
  fi
  if ! policy_bytes="$(wc -c < "$policy_snapshot")"; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error "could not measure policy: ${policy_file}"
    return 2
  fi
  macos_package_policy_trim_ascii_space "$policy_bytes"
  policy_bytes="$MACOS_PACKAGE_POLICY_TRIMMED"
  if [[ ! "$policy_bytes" =~ ^[0-9]+$ ]]; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error "could not measure policy: ${policy_file}"
    return 2
  fi
  if [[ "$policy_bytes" -gt "$MACOS_PACKAGE_POLICY_MAX_BYTES" ]]; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error \
      "policy exceeds the ${MACOS_PACKAGE_POLICY_MAX_BYTES}-byte limit: ${policy_file}"
    return 2
  fi
  if ! macos_package_policy_check_utf8_text "policy" "$policy_snapshot"; then
    rm -f "$policy_snapshot"
    return 2
  fi

  # Shape-check once before the first digest, then parse the enforcement set
  # again between two hashes. The first pass provides precise bounded-format
  # failures without trusting it for enforcement; the bracketed second pass
  # cannot accept bytes that changed while tokens were being derived.
  if ! macos_package_policy_parse_snapshot "$policy_snapshot" "$policy_file"; then
    rm -f "$policy_snapshot"
    return 2
  fi
  if ! macos_package_policy_sha256 "$policy_snapshot"; then
    rm -f "$policy_snapshot"
    return 2
  fi
  digest_before="$MACOS_PACKAGE_POLICY_DIGEST"
  if [[ "$digest_before" != "$MACOS_PACKAGE_POLICY_EXPECTED_SHA256" ]]; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error \
      "policy does not match the reviewed component set: ${policy_file}"
    return 2
  fi
  if ! macos_package_policy_parse_snapshot "$policy_snapshot" "$policy_file"; then
    rm -f "$policy_snapshot"
    return 2
  fi
  if ! macos_package_policy_sha256 "$policy_snapshot"; then
    rm -f "$policy_snapshot"
    return 2
  fi
  digest_after="$MACOS_PACKAGE_POLICY_DIGEST"
  if [[ "$digest_after" != "$digest_before" ]]; then
    rm -f "$policy_snapshot"
    macos_package_policy_setup_error \
      "policy snapshot changed while it was being validated: ${policy_file}"
    return 2
  fi
  if ! rm -f "$policy_snapshot"; then
    macos_package_policy_setup_error "could not remove private policy snapshot"
    return 2
  fi

  MACOS_PACKAGE_POLICY_DIGEST="$digest_after"
  MACOS_PACKAGE_POLICY_RESULT="loaded"
}

macos_copy_control_path_is_prohibited() {
  local path="$1"
  local filename token token_index=0
  local LC_ALL=C
  export LC_ALL

  filename="${path##*/}"
  macos_package_policy_ascii_lower "$filename"
  filename="$MACOS_PACKAGE_POLICY_NORMALIZED"
  MACOS_PACKAGE_POLICY_MATCHED_TOKEN=""
  while [[ "$token_index" -lt "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" ]]; do
    token="${MACOS_FORBIDDEN_COMPONENT_TOKENS[$token_index]}"
    if [[ "$filename" == *"$token"* ]]; then
      MACOS_PACKAGE_POLICY_MATCHED_TOKEN="$token"
      return 0
    fi
    token_index=$((token_index + 1))
  done
  return 1
}

macos_copy_control_relative_path_is_prohibited() {
  local remaining="$1"
  local component

  while :; do
    component="${remaining%%/*}"
    if [[ -n "$component" ]] \
        && macos_copy_control_path_is_prohibited "$component"; then
      return 0
    fi
    [[ "$remaining" == */* ]] || break
    remaining="${remaining#*/}"
  done
  return 1
}

macos_package_policy_reference_is_well_formed() {
  local label="$1"
  local reference="$2"
  local LC_ALL=C
  export LC_ALL

  if [[ "${#reference}" -gt "$MACOS_PACKAGE_POLICY_MAX_REFERENCE_BYTES" ]]; then
    MACOS_PACKAGE_POLICY_REASON=\
"${label} exceeds the ${MACOS_PACKAGE_POLICY_MAX_REFERENCE_BYTES}-byte limit"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$reference" == *[$'\001'-$'\037'$'\177']* ]]; then
    MACOS_PACKAGE_POLICY_REASON="${label} contains an unsupported control character"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

macos_package_policy_check_output() {
  local label="$1"
  local output_file="$2"
  local output_bytes

  if ! output_bytes="$(wc -c < "$output_file")"; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  macos_package_policy_trim_ascii_space "$output_bytes"
  output_bytes="$MACOS_PACKAGE_POLICY_TRIMMED"
  if [[ ! "$output_bytes" =~ ^[0-9]+$ ]]; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$output_bytes" -gt "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES" ]]; then
    MACOS_PACKAGE_POLICY_REASON=\
"${label} exceeds the ${MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES}-byte limit"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! macos_package_policy_check_utf8_text "$label" "$output_file"; then
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

macos_package_policy_capture_output() {
  local label="$1"
  local output_file="$2"
  local max_bytes="$3"
  local max_seconds="$4"
  shift 4
  local command_status=0 output_bytes file_blocks
  local perl_command="${MACOS_PERL_COMMAND:-perl}"

  if [[ ! "$max_bytes" =~ ^[0-9]+$ || ! "$max_seconds" =~ ^[1-9][0-9]*$ \
      || "$#" -eq 0 ]]; then
    MACOS_PACKAGE_POLICY_REASON="invalid bounded-capture configuration for ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! command -v "$perl_command" >/dev/null 2>&1; then
    MACOS_PACKAGE_POLICY_REASON="required bounded-capture tool is unavailable: ${perl_command}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi

  # Bash expresses RLIMIT_FSIZE in 1024-byte blocks outside POSIX mode. Permit
  # one block beyond the reviewed byte ceiling so an overflow is materialized
  # and diagnosed, while a runaway producer remains bounded by the kernel.
  file_blocks=$(((max_bytes + 1024) / 1024))
  (
    # Set both limits: a producer must not be able to raise the soft limit back
    # to an inherited, larger hard limit before it starts writing.
    ulimit -S -f "$file_blocks" || exit 125
    ulimit -H -f "$file_blocks" || exit 125
    "$perl_command" -e '
      use strict;
      use warnings;
      my $seconds = shift @ARGV;
      exit 126 unless @ARGV;
      my $child = fork();
      exit 127 unless defined $child;
      if ($child == 0) {
        setpgrp 0, 0 or exit 127;
        exec { $ARGV[0] } @ARGV;
        exit 127;
      }
      sub terminate_group {
        my ($group) = @_;
        return unless kill q{TERM}, -$group;
        select undef, undef, undef, 0.1;
        kill q{KILL}, -$group;
      }
      $SIG{ALRM} = sub {
        terminate_group($child);
        waitpid $child, 0;
        exit 124;
      };
      alarm $seconds;
      waitpid $child, 0;
      alarm 0;
      my $status = $?;
      # A producer that returned without reaping its own descendants must not
      # leave them writing to the captured file after validation has started.
      terminate_group($child);
      exit(128 + ($status & 127)) if $status & 127;
      exit($status >> 8);
    ' -- "$max_seconds" "$@"
  ) > "$output_file" 2>/dev/null || command_status=$?

  if ! output_bytes="$(wc -c < "$output_file")"; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  macos_package_policy_trim_ascii_space "$output_bytes"
  output_bytes="$MACOS_PACKAGE_POLICY_TRIMMED"
  if [[ ! "$output_bytes" =~ ^[0-9]+$ ]]; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$output_bytes" -gt "$max_bytes" ]]; then
    MACOS_PACKAGE_POLICY_REASON="${label} exceeds the ${max_bytes}-byte limit"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$command_status" -ne 0 ]]; then
    MACOS_PACKAGE_POLICY_REASON="could not collect ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

macos_package_policy_remove_private_dir() {
  local private_dir="$1"
  if ! rm -rf "$private_dir"; then
    MACOS_PACKAGE_POLICY_REASON="could not remove private macOS policy workspace"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

MACOS_PACKAGE_POLICY_OTOOL_ARCHITECTURE=""
MACOS_PACKAGE_POLICY_OTOOL_LINKED_SECTIONS=""
MACOS_PACKAGE_POLICY_OTOOL_LOAD_SECTIONS=""

macos_package_policy_otool_header_is_supported() {
  local artifact="$1"
  local line="$2"
  local prefix architecture

  MACOS_PACKAGE_POLICY_OTOOL_ARCHITECTURE=""
  if [[ "$line" == "${artifact}:" ]]; then
    return 0
  fi
  prefix="${artifact} (architecture "
  if [[ "$line" == "$prefix"* && "$line" == *'):' ]]; then
    architecture="${line#"$prefix"}"
    architecture="${architecture%):}"
    if [[ -n "$architecture" \
        && "$architecture" =~ ^[A-Za-z0-9._+-]+$ ]]; then
      MACOS_PACKAGE_POLICY_OTOOL_ARCHITECTURE="$architecture"
      return 0
    fi
  fi
  return 1
}

macos_package_policy_parse_linked_output() {
  local artifact="$1"
  local linked_output="$2"
  local line dependency_record dependency version_details prefix architecture
  local version_pattern='^[0-9]+([.][0-9]+)*, current version [0-9]+([.][0-9]+)*\)$'
  local line_count=0 header_count=0 dependency_count=0
  local header_shape="" architecture_keys="|" section_signature=""

  MACOS_PACKAGE_POLICY_OTOOL_LINKED_SECTIONS=""

  while IFS= read -r line || [[ -n "$line" ]]; do
    line_count=$((line_count + 1))
    if [[ "$line_count" -gt "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_LINES" \
        || "${#line}" -gt 16384 ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O import output has an unsupported shape"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    if macos_package_policy_otool_header_is_supported "$artifact" "$line"; then
      architecture="$MACOS_PACKAGE_POLICY_OTOOL_ARCHITECTURE"
      if [[ -z "$architecture" ]]; then
        if [[ "$header_count" -ne 0 || "$header_shape" == fat ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O import output has duplicate or mixed artifact headers"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        header_shape=plain
        section_signature=plain
      else
        if [[ "$header_shape" == plain \
            || "$architecture_keys" == *"|${architecture}|"* ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O import output has duplicate or mixed architecture headers"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        header_shape=fat
        architecture_keys="${architecture_keys}${architecture}|"
        section_signature="${section_signature}|${architecture}"
      fi
      header_count=$((header_count + 1))
      continue
    fi
    if [[ "$header_count" -gt 0 ]] \
        && macos_copy_control_relative_path_is_prohibited "$line"; then
      MACOS_PACKAGE_POLICY_REASON=\
"${artifact##*/} has an import record containing forbidden token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}'"
      MACOS_PACKAGE_POLICY_RESULT="prohibited"
      return 1
    fi
    if [[ "$header_count" -eq 0 || -z "$line" \
        || ( "$line" != ' '* && "$line" != $'\t'* ) ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O import output has an unsupported record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi

    dependency_record="$line"
    while [[ "$dependency_record" == ' '* || "$dependency_record" == $'\t'* ]]; do
      dependency_record="${dependency_record#?}"
    done
    if [[ "$dependency_record" != *' (compatibility version '*', current version '*')' ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O import output has an unsupported dependency record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    dependency="${dependency_record%% \(compatibility version *}"
    prefix="${dependency} (compatibility version "
    version_details="${dependency_record#"$prefix"}"
    if [[ -z "$dependency" || ! "$version_details" =~ $version_pattern ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O import output has a malformed dependency record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    if ! macos_package_policy_reference_is_well_formed \
        "Mach-O dependency path" "$dependency"; then
      return 2
    fi
    if macos_copy_control_relative_path_is_prohibited "$dependency"; then
      MACOS_PACKAGE_POLICY_REASON=\
"${artifact##*/} imports forbidden component path ${dependency} (token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}')"
      MACOS_PACKAGE_POLICY_RESULT="prohibited"
      return 1
    fi
    dependency_count=$((dependency_count + 1))
  done < "$linked_output"

  if [[ "$header_count" -eq 0 ]]; then
    MACOS_PACKAGE_POLICY_REASON="Mach-O import output contains no supported artifact header"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  MACOS_PACKAGE_POLICY_OTOOL_LINKED_SECTIONS="${header_shape}:${section_signature}"
  : "$dependency_count"
}

MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=""

macos_package_policy_otool_command_is_supported() {
  local command_name="$1"

  MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=""
  case "$command_name" in
    LC_LOADFVMLIB|LC_IDFVMLIB|LC_FVMFILE|LC_LOAD_DYLIB|LC_ID_DYLIB|\
    LC_LOAD_DYLINKER|LC_ID_DYLINKER|LC_PREBOUND_DYLIB|\
    LC_LOAD_WEAK_DYLIB|LC_REEXPORT_DYLIB|LC_LAZY_LOAD_DYLIB|\
    LC_DYLD_ENVIRONMENT|LC_LOAD_UPWARD_DYLIB)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=name
      ;;
    LC_RPATH)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=path
      ;;
    LC_SUB_FRAMEWORK)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=umbrella
      ;;
    LC_SUB_UMBRELLA)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=sub_umbrella
      ;;
    LC_SUB_CLIENT)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=client
      ;;
    LC_SUB_LIBRARY)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=sub_library
      ;;
    LC_FILESET_ENTRY)
      MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD=entry_id
      ;;
    LC_SEGMENT|LC_SYMTAB|LC_SYMSEG|LC_THREAD|LC_UNIXTHREAD|LC_IDENT|\
    LC_PREPAGE|LC_DYSYMTAB|LC_ROUTINES|LC_TWOLEVEL_HINTS|\
    LC_PREBIND_CKSUM|LC_SEGMENT_64|LC_ROUTINES_64|LC_UUID|\
    LC_CODE_SIGNATURE|LC_SEGMENT_SPLIT_INFO|LC_ENCRYPTION_INFO|\
    LC_DYLD_INFO|LC_DYLD_INFO_ONLY|LC_VERSION_MIN_MACOSX|\
    LC_VERSION_MIN_IPHONEOS|LC_FUNCTION_STARTS|LC_MAIN|LC_DATA_IN_CODE|\
    LC_SOURCE_VERSION|LC_DYLIB_CODE_SIGN_DRS|LC_ENCRYPTION_INFO_64|\
    LC_LINKER_OPTION|LC_LINKER_OPTIMIZATION_HINT|LC_VERSION_MIN_TVOS|\
    LC_VERSION_MIN_WATCHOS|LC_NOTE|LC_BUILD_VERSION|\
    LC_DYLD_EXPORTS_TRIE|LC_DYLD_CHAINED_FIXUPS|LC_ATOM_INFO|\
    LC_FUNCTION_VARIANTS|LC_FUNCTION_VARIANT_FIXUPS|LC_TARGET_TRIPLE)
      ;;
    *) return 1 ;;
  esac
}

macos_package_policy_parse_load_output() {
  local artifact="$1"
  local load_output="$2"
  local line trimmed command_name load_reference reference_field architecture
  local header_magic header_cpu header_subcpu header_caps header_filetype
  local header_ncmds header_sizeofcmds header_flags header_extra
  local required_field="" reference_count=0
  local load_reference_pattern='^([A-Za-z][A-Za-z0-9_]*) (.*) \(offset [0-9]+\)$'
  local load_command_pattern='^Load command [0-9]+$'
  local metadata_pattern='^[A-Za-z][A-Za-z0-9_]*( [A-Za-z][A-Za-z0-9_]*)* .*$'
  local line_count=0 header_count=0 load_count=0 section_load_count=0
  local in_load=false command_seen=false cmdsize_seen=false
  local header_shape="" architecture_keys="|" section_signature=""
  local preamble_state=none expected_load_commands=""
  local signed_numeric='^-?[0-9]+$'
  local unsigned_numeric='^(0|[1-9][0-9]*)$'
  local hexadecimal='^0x[0-9A-Fa-f]+$'

  MACOS_PACKAGE_POLICY_OTOOL_LOAD_SECTIONS=""

  while IFS= read -r line || [[ -n "$line" ]]; do
    line_count=$((line_count + 1))
    if [[ "$line_count" -gt "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_LINES" \
        || "${#line}" -gt 16384 ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unsupported shape"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    if macos_package_policy_otool_header_is_supported "$artifact" "$line"; then
      if [[ "$in_load" == true ]]; then
        if [[ "$command_seen" != true || "$cmdsize_seen" != true ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load command is missing cmd or cmdsize"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        if [[ -n "$required_field" && "$reference_count" -ne 1 ]]; then
          MACOS_PACKAGE_POLICY_REASON=\
"Mach-O ${command_name} command omits its required ${required_field} record"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
      fi
      if [[ "$header_count" -gt 0 \
          && ( "$preamble_state" != complete \
            || "$section_load_count" != "$expected_load_commands" ) ]]; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O architecture section has an incomplete load-command table"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      architecture="$MACOS_PACKAGE_POLICY_OTOOL_ARCHITECTURE"
      if [[ -z "$architecture" ]]; then
        if [[ "$header_count" -ne 0 || "$header_shape" == fat ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has duplicate or mixed artifact headers"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        header_shape=plain
        section_signature=plain
      else
        if [[ "$header_shape" == plain \
            || "$architecture_keys" == *"|${architecture}|"* ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has duplicate or mixed architecture headers"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        header_shape=fat
        architecture_keys="${architecture_keys}${architecture}|"
        section_signature="${section_signature}|${architecture}"
      fi
      header_count=$((header_count + 1))
      section_load_count=0
      in_load=false
      command_seen=false
      cmdsize_seen=false
      command_name=""
      required_field=""
      reference_count=0
      preamble_state=mach_header
      expected_load_commands=""
      continue
    fi
    if [[ "$header_count" -eq 0 || -z "$line" ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unsupported record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi

    trimmed="$line"
    while [[ "$trimmed" == ' '* || "$trimmed" == $'\t'* ]]; do
      trimmed="${trimmed#?}"
    done
    # Denied tokens take precedence over shape errors so an unfamiliar otool
    # record can never hide a recognizable prohibited component.
    if macos_copy_control_relative_path_is_prohibited "$trimmed"; then
      MACOS_PACKAGE_POLICY_REASON=\
"${artifact##*/} contains a forbidden Mach-O load record (token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}')"
      MACOS_PACKAGE_POLICY_RESULT="prohibited"
      return 1
    fi
    case "$preamble_state" in
      mach_header)
        if [[ "$trimmed" != 'Mach header' ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output omits its Mach header preamble"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        preamble_state=columns
        continue
        ;;
      columns)
        read -r header_magic header_cpu header_subcpu header_caps \
          header_filetype header_ncmds header_sizeofcmds header_flags \
          header_extra <<< "$trimmed"
        if [[ "$header_magic" != magic || "$header_cpu" != cputype \
            || "$header_subcpu" != cpusubtype || "$header_caps" != caps \
            || "$header_filetype" != filetype || "$header_ncmds" != ncmds \
            || "$header_sizeofcmds" != sizeofcmds || "$header_flags" != flags \
            || -n "${header_extra:-}" ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unsupported Mach header schema"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        preamble_state=values
        continue
        ;;
      values)
        read -r header_magic header_cpu header_subcpu header_caps \
          header_filetype header_ncmds header_sizeofcmds header_flags \
          header_extra <<< "$trimmed"
        if [[ ! "$header_magic" =~ ^0x[0-9A-Fa-f]{8,16}$ \
            || ! "$header_cpu" =~ $signed_numeric \
            || ! "$header_subcpu" =~ $signed_numeric \
            || ! "$header_caps" =~ $hexadecimal \
            || ! "$header_filetype" =~ $unsigned_numeric \
            || ! "$header_ncmds" =~ $unsigned_numeric \
            || ! "$header_sizeofcmds" =~ $unsigned_numeric \
            || ! "$header_flags" =~ $hexadecimal \
            || -n "${header_extra:-}" \
            || "${#header_ncmds}" -gt 9 \
            || "$header_ncmds" -gt "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_LINES" ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has malformed Mach header values"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        expected_load_commands="$header_ncmds"
        preamble_state=complete
        continue
        ;;
    esac
    if [[ "$trimmed" =~ $load_command_pattern ]]; then
      if [[ "$in_load" == true ]]; then
        if [[ "$command_seen" != true || "$cmdsize_seen" != true ]]; then
          MACOS_PACKAGE_POLICY_REASON="Mach-O load command is missing cmd or cmdsize"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
        if [[ -n "$required_field" && "$reference_count" -ne 1 ]]; then
          MACOS_PACKAGE_POLICY_REASON=\
"Mach-O ${command_name} command omits its required ${required_field} record"
          MACOS_PACKAGE_POLICY_RESULT="uninspectable"
          return 2
        fi
      fi
      if [[ "$trimmed" != "Load command ${section_load_count}" ]]; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O load commands are missing or out of order"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      in_load=true
      command_seen=false
      cmdsize_seen=false
      command_name=""
      required_field=""
      reference_count=0
      load_count=$((load_count + 1))
      section_load_count=$((section_load_count + 1))
      continue
    fi
    if [[ "$trimmed" == cmd\ * ]]; then
      command_name="${trimmed#cmd }"
      if [[ "$in_load" != true || "$command_seen" == true \
          || "$cmdsize_seen" == true ]] \
          || ! macos_package_policy_otool_command_is_supported "$command_name"; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an invalid cmd record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      command_seen=true
      required_field="$MACOS_PACKAGE_POLICY_OTOOL_REQUIRED_FIELD"
      continue
    fi
    if [[ "$trimmed" == cmdsize\ * ]]; then
      if [[ "$in_load" != true || "$command_seen" != true \
          || "$cmdsize_seen" == true \
          || ! "$trimmed" =~ ^cmdsize\ [1-9][0-9]*$ ]]; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an invalid cmdsize record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      cmdsize_seen=true
      continue
    fi
    if [[ "$in_load" != true || "$command_seen" != true \
        || "$cmdsize_seen" != true ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has a record outside a complete command"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    if [[ "$trimmed" =~ $load_reference_pattern ]]; then
      reference_field="${BASH_REMATCH[1]}"
      load_reference="${BASH_REMATCH[2]}"
      if [[ -z "$required_field" || "$reference_field" != "$required_field" \
          || "$reference_count" -ne 0 || -z "$load_reference" ]] \
          || ! macos_package_policy_reference_is_well_formed \
            "Mach-O load-command path" "$load_reference"; then
        [[ -n "$MACOS_PACKAGE_POLICY_REASON" ]] \
          || MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an invalid path record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      reference_count=1
      continue
    fi
    if [[ "$trimmed" == Section ]]; then
      if [[ "$command_name" != LC_SEGMENT && "$command_name" != LC_SEGMENT_64 ]]; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unexpected Section record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      continue
    fi
    if [[ "$trimmed" =~ ^Build\ tool\ [0-9]+$ ]]; then
      if [[ "$command_name" != LC_BUILD_VERSION ]]; then
        MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unexpected build-tool record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      continue
    fi
    if [[ ! "$trimmed" =~ $metadata_pattern ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output has an unsupported metadata record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
  done < "$load_output"

  if [[ "$in_load" == true ]]; then
    if [[ "$command_seen" != true || "$cmdsize_seen" != true ]]; then
      MACOS_PACKAGE_POLICY_REASON="Mach-O load command is missing cmd or cmdsize"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    if [[ -n "$required_field" && "$reference_count" -ne 1 ]]; then
      MACOS_PACKAGE_POLICY_REASON=\
"Mach-O ${command_name} command omits its required ${required_field} record"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
  fi
  if [[ "$header_count" -eq 0 || "$preamble_state" != complete \
      || "$section_load_count" != "$expected_load_commands" ]]; then
    MACOS_PACKAGE_POLICY_REASON="Mach-O load-command output is incomplete"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  MACOS_PACKAGE_POLICY_OTOOL_LOAD_SECTIONS="${header_shape}:${section_signature}"
}

macos_validate_macho_copy_control() {
  local artifact="$1"
  local inspect_source_path="${2:-true}"
  local inspection_budget="${3:-$MACOS_PACKAGE_POLICY_MAX_TOOL_SECONDS}"
  local inspection_dir linked_output load_output validation_status
  local deadline remaining_seconds
  local LC_ALL=C
  export LC_ALL

  MACOS_PACKAGE_POLICY_REASON=""
  MACOS_PACKAGE_POLICY_RESULT=""
  if [[ ! "$inspection_budget" =~ ^[1-9][0-9]*$ ]]; then
    MACOS_PACKAGE_POLICY_REASON="invalid Mach-O inspection time budget"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  deadline=$((SECONDS + inspection_budget))
  if [[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -eq 0 ]]; then
    MACOS_PACKAGE_POLICY_REASON="bundled-component policy has not been loaded"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! macos_package_policy_reference_is_well_formed "Mach-O artifact path" "$artifact"; then
    return 2
  fi
  if [[ ! -f "$artifact" || -L "$artifact" || "$artifact" == -* ]]; then
    MACOS_PACKAGE_POLICY_REASON="Mach-O artifact is missing, a symlink, or has an unsafe path: ${artifact}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi

  if [[ "$inspect_source_path" == true ]] \
      && macos_copy_control_relative_path_is_prohibited "$artifact"; then
    MACOS_PACKAGE_POLICY_REASON=\
"source path ${artifact} matches forbidden token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}'"
    MACOS_PACKAGE_POLICY_RESULT="prohibited"
    return 1
  fi
  if [[ "$inspect_source_path" != true ]] \
      && macos_copy_control_path_is_prohibited "$artifact"; then
    MACOS_PACKAGE_POLICY_REASON=\
"${artifact##*/} matches forbidden token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}'"
    MACOS_PACKAGE_POLICY_RESULT="prohibited"
    return 1
  fi

  if ! inspection_dir="$(mktemp -d "${TMPDIR:-/tmp}/balun-macos-macho-policy.XXXXXX")"; then
    MACOS_PACKAGE_POLICY_REASON="could not create a private Mach-O inspection directory"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  linked_output="${inspection_dir}/linked.txt"
  load_output="${inspection_dir}/load.txt"

  remaining_seconds=$((deadline - SECONDS))
  if [[ "$remaining_seconds" -le 0 ]]; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    MACOS_PACKAGE_POLICY_REASON="Mach-O inspection exceeded its time budget"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! macos_package_policy_capture_output \
      "Mach-O import output" "$linked_output" \
      "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES" \
      "$remaining_seconds" \
      "${MACOS_OTOOL_COMMAND:-otool}" -arch all -L "$artifact"; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return 2
  fi
  if ! macos_package_policy_check_output "Mach-O import output" "$linked_output"; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return 2
  fi
  validation_status=0
  macos_package_policy_parse_linked_output "$artifact" "$linked_output" \
    || validation_status=$?
  if [[ "$validation_status" -ne 0 ]]; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return "$validation_status"
  fi

  remaining_seconds=$((deadline - SECONDS))
  if [[ "$remaining_seconds" -le 0 ]]; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    MACOS_PACKAGE_POLICY_REASON="Mach-O inspection exceeded its time budget"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! macos_package_policy_capture_output \
      "Mach-O load-command output" "$load_output" \
      "$MACOS_PACKAGE_POLICY_MAX_OUTPUT_BYTES" \
      "$remaining_seconds" \
      "${MACOS_OTOOL_COMMAND:-otool}" -arch all -h -l "$artifact"; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return 2
  fi
  if ! macos_package_policy_check_output "Mach-O load-command output" "$load_output"; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return 2
  fi
  validation_status=0
  macos_package_policy_parse_load_output "$artifact" "$load_output" \
    || validation_status=$?
  if [[ "$validation_status" -ne 0 ]]; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    return "$validation_status"
  fi
  if [[ "$MACOS_PACKAGE_POLICY_OTOOL_LINKED_SECTIONS" \
      != "$MACOS_PACKAGE_POLICY_OTOOL_LOAD_SECTIONS" ]]; then
    macos_package_policy_remove_private_dir "$inspection_dir" || true
    MACOS_PACKAGE_POLICY_REASON="Mach-O inspections disagree on architecture coverage"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi

  macos_package_policy_remove_private_dir "$inspection_dir" || return 2
  MACOS_PACKAGE_POLICY_RESULT="allowed"
}

macos_bundle_artifact_requires_import_scan() {
  local bundle_root="$1"
  local artifact="$2"
  local executable="$3"
  local magic="$4"
  local bundle_name artifact_name

  bundle_name="${bundle_root##*/}"
  bundle_name="${bundle_name%.app}"
  artifact_name="${artifact##*/}"
  case "$artifact_name" in
    *.dylib|*.so) return 0 ;;
  esac
  case "$artifact" in
    "$bundle_root"/Contents/MacOS/*)
      [[ "$artifact" == "$bundle_root/Contents/MacOS/$bundle_name" ]] || return 0
      ;;
    "$bundle_root"/Contents/Frameworks/*)
      [[ "$executable" == 1 ]] && return 0
      ;;
  esac

  case "$magic" in
    feedface|cefaedfe|feedfacf|cffaedfe|cafebabe|bebafeca|cafebabf|bfbafeca)
      return 0
      ;;
  esac
  return 1
}

macos_package_policy_check_manifest_shape() {
  local manifest="$1"
  local label="$2"
  local bytes

  if [[ ! -f "$manifest" || -L "$manifest" ]]; then
    MACOS_PACKAGE_POLICY_REASON="${label} is missing or is not a regular file"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! bytes="$(wc -c < "$manifest")"; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  macos_package_policy_trim_ascii_space "$bytes"
  bytes="$MACOS_PACKAGE_POLICY_TRIMMED"
  if [[ ! "$bytes" =~ ^[0-9]+$ ]]; then
    MACOS_PACKAGE_POLICY_REASON="could not measure ${label}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$bytes" -gt "$MACOS_PACKAGE_POLICY_MAX_MANIFEST_BYTES" ]]; then
    MACOS_PACKAGE_POLICY_REASON=\
"${label} exceeds the ${MACOS_PACKAGE_POLICY_MAX_MANIFEST_BYTES}-byte limit"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

macos_package_policy_build_tree_manifest() {
  local bundle_root="$1"
  local manifest="$2"
  local perl_command="${MACOS_PERL_COMMAND:-perl}"
  local hash_command hash_mode

  if command -v sha256sum >/dev/null 2>&1; then
    hash_command="$(command -v sha256sum)"
    hash_mode=sha256sum
  elif command -v shasum >/dev/null 2>&1; then
    hash_command="$(command -v shasum)"
    hash_mode=shasum
  else
    MACOS_PACKAGE_POLICY_REASON="required bundle-content SHA-256 tool is unavailable"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi

  macos_package_policy_capture_output \
    "macOS bundle content manifest" "$manifest" \
    "$MACOS_PACKAGE_POLICY_MAX_MANIFEST_BYTES" \
    "$MACOS_PACKAGE_POLICY_MAX_TOOL_SECONDS" \
    "$perl_command" -e '
      use strict;
      use warnings;
      use bytes;
      use Cwd qw(abs_path);
      use Fcntl qw(O_RDONLY :mode);

      my ($input_root, $max_entries, $max_depth, $max_reference, $max_file,
          $max_total, $hash_mode, $hash_command) = @ARGV;
      exit 20 unless defined $hash_command;
      for ($max_entries, $max_depth, $max_reference, $max_file, $max_total) {
        exit 20 unless defined $_ && $_ =~ /^\d+$/;
      }
      my $root = abs_path($input_root);
      exit 21 unless defined $root && -d $root && !-l $input_root;
      $root =~ s{/+$}{};
      my $root_prefix = "$root/";
      my $no_follow = eval { Fcntl::O_NOFOLLOW() };
      exit 22 unless defined $no_follow;
      my $entry_count = 0;
      my $regular_bytes = 0;

      sub valid_reference {
        my ($value) = @_;
        return 0 unless defined $value && length($value) > 0;
        return 0 if length($value) > $max_reference;
        return 0 if $value =~ /[\x00-\x1f\x7f]/;
        return 1;
      }

      sub inside_root {
        my ($path) = @_;
        return defined $path && ($path eq $root || index($path, $root_prefix) == 0);
      }

      sub same_stat {
        my ($left, $right) = @_;
        return 0 unless @$left == @$right && @$left >= 11;
        for my $index (0, 1, 2, 3, 4, 5, 7, 9, 10) {
          return 0 if $left->[$index] != $right->[$index];
        }
        return 1;
      }

      sub metadata_fields {
        my ($stat) = @_;
        return (
          sprintf("%o", $stat->[2]), $stat->[4], $stat->[5],
          $stat->[0], $stat->[1], $stat->[3], $stat->[7],
          $stat->[9], $stat->[10]
        );
      }

      sub emit_record {
        my (@fields) = @_;
        for my $field (@fields) {
          exit 23 unless defined $field && $field !~ /[\x00\x09\x0a\x0d]/;
        }
        print STDOUT join("\t", @fields), "\0" or exit 24;
      }

      sub lexical_target {
        my ($relative, $target) = @_;
        return undef if $target =~ m{^/};
        my @parts = split m{/}, $relative, -1;
        pop @parts;
        for my $part (split m{/}, $target, -1) {
          next if $part eq q{} || $part eq q{.};
          if ($part eq q{..}) {
            return undef unless @parts;
            pop @parts;
          } else {
            push @parts, $part;
          }
        }
        return undef unless @parts;
        return join q{/}, @parts;
      }

      sub walk_node {
        my ($absolute, $relative, $depth) = @_;
        exit 25 if $depth > $max_depth;
        if (length $relative) {
          exit 26 unless valid_reference($relative) && $relative !~ m{^/};
          $entry_count++;
          exit 27 if $entry_count > $max_entries;
        }

        my @before = lstat($absolute);
        exit 28 unless @before;
        my $canonical = abs_path($absolute);
        exit 29 unless inside_root($canonical);
        my @metadata = metadata_fields(\@before);

        if (S_ISDIR($before[2])) {
          opendir(my $directory, $absolute) or exit 30;
          my @opened = stat($directory);
          exit 31 unless @opened && same_stat(\@before, \@opened);
          my @names;
          while (defined(my $name = readdir($directory))) {
            next if $name eq q{.} || $name eq q{..};
            exit 32 unless valid_reference($name);
            exit 33 if @names >= $max_entries - $entry_count;
            push @names, $name;
          }
          for my $name (sort { $a cmp $b } @names) {
            my $child_relative = length($relative) ? "$relative/$name" : $name;
            walk_node("$absolute/$name", $child_relative, $depth + 1);
          }
          closedir($directory) or exit 34;
          my @after = lstat($absolute);
          exit 35 unless @after && same_stat(\@before, \@after);
          emit_record(q{D}, length($relative) ? $relative : q{.}, @metadata);
          return;
        }

        if (S_ISREG($before[2])) {
          exit 36 unless $before[3] == 1;
          exit 37 if $before[7] > $max_file;
          exit 38 if $before[7] > $max_total - $regular_bytes;
          $regular_bytes += $before[7];
          sysopen(my $file, $absolute, O_RDONLY | $no_follow) or exit 39;
          binmode($file) or exit 40;
          my @opened = stat($file);
          exit 41 unless @opened && same_stat(\@before, \@opened);
          my $prefix = q{};
          my $read = sysread($file, $prefix, 4);
          exit 42 unless defined $read;
          my @hash_arguments;
          if ($hash_mode eq q{sha256sum}) {
            @hash_arguments = ($hash_command, q{--}, $absolute);
          } elsif ($hash_mode eq q{shasum}) {
            @hash_arguments = ($hash_command, q{-a}, q{256}, $absolute);
          } else {
            exit 43;
          }
          open(my $hash_output, q{-|}, @hash_arguments) or exit 44;
          my $hash_line = <$hash_output>;
          my $hash_extra = <$hash_output>;
          close($hash_output) or exit 45;
          exit 46 unless defined $hash_line && !defined $hash_extra
            && $hash_line =~ /^([0-9A-Fa-f]{64})(?:[[:space:]]|$)/;
          my $digest = lc $1;
          my @opened_after = stat($file);
          close($file) or exit 47;
          my @after = lstat($absolute);
          exit 48 unless @opened_after && @after
            && same_stat(\@before, \@opened_after)
            && same_stat(\@before, \@after);
          my $executable = ($before[2] & 0111) ? 1 : 0;
          my $magic = unpack(q{H*}, $prefix);
          emit_record(q{F}, $relative, @metadata, $digest, $executable, $magic);
          return;
        }

        if (S_ISLNK($before[2])) {
          exit 49 unless $before[3] == 1;
          my $target = readlink($absolute);
          exit 50 unless valid_reference($target) && $target !~ m{^/};
          my $resolved_relative = lexical_target($relative, $target);
          exit 51 unless defined $resolved_relative && valid_reference($resolved_relative);
          my $resolved = abs_path("$root/$resolved_relative");
          exit 52 unless inside_root($resolved) && -e $resolved;
          my @after = lstat($absolute);
          exit 53 unless @after && same_stat(\@before, \@after);
          emit_record(q{L}, $relative, @metadata, $target, $resolved_relative);
          return;
        }

        exit 54;
      }

      walk_node($root, q{}, 0);
      close(STDOUT) or exit 55;
    ' -- "$bundle_root" \
      "$MACOS_PACKAGE_POLICY_MAX_BUNDLE_ENTRIES" \
      "$MACOS_PACKAGE_POLICY_MAX_BUNDLE_DEPTH" \
      "$MACOS_PACKAGE_POLICY_MAX_REFERENCE_BYTES" \
      "$MACOS_PACKAGE_POLICY_MAX_REGULAR_FILE_BYTES" \
      "$MACOS_PACKAGE_POLICY_MAX_REGULAR_BYTES" \
      "$hash_mode" "$hash_command"
}

macos_package_policy_symlink_target_matches_resolution() {
  local relative_path="$1"
  local target="$2"
  local expected="$3"
  local base remaining component resolved part_count=0 part_index
  local parts=()

  base="${relative_path%/*}"
  [[ "$base" != "$relative_path" ]] || base=""
  remaining="$base"
  while [[ -n "$remaining" ]]; do
    component="${remaining%%/*}"
    [[ -z "$component" || "$component" == . || "$component" == .. ]] \
      && return 1
    parts[$part_count]="$component"
    part_count=$((part_count + 1))
    [[ "$remaining" == */* ]] || break
    remaining="${remaining#*/}"
  done

  remaining="$target"
  while :; do
    component="${remaining%%/*}"
    case "$component" in
      ''|.) ;;
      ..)
        [[ "$part_count" -gt 0 ]] || return 1
        part_count=$((part_count - 1))
        unset 'parts[part_count]'
        ;;
      *)
        parts[$part_count]="$component"
        part_count=$((part_count + 1))
        ;;
    esac
    [[ "$remaining" == */* ]] || break
    remaining="${remaining#*/}"
  done

  [[ "$part_count" -gt 0 ]] || return 1
  resolved=""
  part_index=0
  while [[ "$part_index" -lt "$part_count" ]]; do
    component="${parts[$part_index]}"
    if [[ -n "$resolved" ]]; then
      resolved="${resolved}/${component}"
    else
      resolved="$component"
    fi
    part_index=$((part_index + 1))
  done
  [[ "$resolved" == "$expected" ]]
}

macos_package_policy_relative_path_is_canonical() {
  local remaining="$1"
  local component

  [[ -n "$remaining" && "$remaining" != /* && "$remaining" != */ ]] || return 1
  while :; do
    component="${remaining%%/*}"
    [[ -n "$component" && "$component" != . && "$component" != .. ]] || return 1
    [[ "$remaining" == */* ]] || break
    remaining="${remaining#*/}"
  done
}

macos_package_policy_manifest_record_is_well_formed() {
  local record="$1"
  local kind relative_path mode uid gid device inode links size modified changed
  local digest executable magic link_target resolved_relative extra
  local remaining field_count=1 expected_fields
  local numeric='^[0-9]+$'
  local octal='^[0-7]+$'
  local digest_pattern='^[0-9a-f]{64}$'
  local magic_pattern='^([0-9a-f][0-9a-f]){0,4}$'

  MACOS_PACKAGE_POLICY_REASON=""
  kind="${record%%$'\t'*}"
  case "$kind" in
    D)
      expected_fields=11
      IFS=$'\t' read -r kind relative_path mode uid gid device inode links \
        size modified changed extra <<< "$record"
      ;;
    F)
      expected_fields=14
      IFS=$'\t' read -r kind relative_path mode uid gid device inode links \
        size modified changed digest executable magic extra <<< "$record"
      ;;
    L)
      expected_fields=13
      IFS=$'\t' read -r kind relative_path mode uid gid device inode links \
        size modified changed link_target resolved_relative extra <<< "$record"
      ;;
    *)
      MACOS_PACKAGE_POLICY_REASON="bundle manifest contains an unsupported entry type"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
      ;;
  esac

  remaining="$record"
  while [[ "$remaining" == *$'\t'* ]]; do
    field_count=$((field_count + 1))
    remaining="${remaining#*$'\t'}"
  done

  if [[ "$field_count" -ne "$expected_fields" || "${#record}" -gt 32768 \
      || -n "${extra:-}" || -z "${relative_path:-}" \
      || ! "${mode:-}" =~ $octal || ! "${uid:-}" =~ $numeric \
      || ! "${gid:-}" =~ $numeric || ! "${device:-}" =~ $numeric \
      || ! "${inode:-}" =~ $numeric || ! "${links:-}" =~ $numeric \
      || ! "${size:-}" =~ $numeric || ! "${modified:-}" =~ $numeric \
      || ! "${changed:-}" =~ $numeric ]]; then
    MACOS_PACKAGE_POLICY_REASON="bundle manifest contains a malformed metadata record"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$relative_path" != . ]] \
      && ! macos_package_policy_relative_path_is_canonical "$relative_path"; then
    MACOS_PACKAGE_POLICY_REASON="bundle manifest contains a non-canonical member path"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$relative_path" == . && "$kind" != D ]] \
      || ! macos_package_policy_reference_is_well_formed \
        "bundle member path" "$relative_path"; then
    return 2
  fi
  case "$kind" in
    F)
      if [[ "$mode" != 10[0-7][0-7][0-7][0-7] \
          || ! "$digest" =~ $digest_pattern \
          || ( "$executable" != 0 && "$executable" != 1 ) \
          || ! "$magic" =~ $magic_pattern || "$links" != 1 ]]; then
        MACOS_PACKAGE_POLICY_REASON="bundle manifest contains a malformed regular-file record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      ;;
    L)
      if [[ "$mode" != 12[0-7][0-7][0-7][0-7] \
          || -z "$link_target" || -z "$resolved_relative" \
          || "$link_target" == /* || "$links" != 1 ]] \
          || ! macos_package_policy_reference_is_well_formed \
            "bundle symlink target" "$link_target" \
          || ! macos_package_policy_relative_path_is_canonical \
            "$resolved_relative" \
          || ! macos_package_policy_reference_is_well_formed \
            "resolved bundle symlink target" "$resolved_relative" \
          || ! macos_package_policy_symlink_target_matches_resolution \
            "$relative_path" "$link_target" "$resolved_relative"; then
        [[ -n "$MACOS_PACKAGE_POLICY_REASON" ]] \
          || MACOS_PACKAGE_POLICY_REASON="bundle manifest contains an unsafe symlink record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      ;;
    D)
      if [[ "$mode" != 4[0-7][0-7][0-7][0-7] ]]; then
        MACOS_PACKAGE_POLICY_REASON="bundle manifest contains a malformed directory record"
        MACOS_PACKAGE_POLICY_RESULT="uninspectable"
        return 2
      fi
      ;;
  esac

  MACOS_PACKAGE_POLICY_MANIFEST_KIND="$kind"
  MACOS_PACKAGE_POLICY_MANIFEST_PATH="$relative_path"
  MACOS_PACKAGE_POLICY_MANIFEST_EXECUTABLE="${executable:-}"
  MACOS_PACKAGE_POLICY_MANIFEST_MAGIC="${magic:-}"
  MACOS_PACKAGE_POLICY_MANIFEST_LINK_TARGET="${link_target:-}"
  MACOS_PACKAGE_POLICY_MANIFEST_RESOLVED_TARGET="${resolved_relative:-}"
}

macos_validate_bundle_member_manifest() {
  local manifest="$1"
  local record="" relative_path
  local entry_count=0
  local saw_root=false saw_contents=false

  while IFS= read -r -d '' record; do
    entry_count=$((entry_count + 1))
    if [[ "$entry_count" -gt "$MACOS_PACKAGE_POLICY_MAX_BUNDLE_ENTRIES" ]]; then
      MACOS_PACKAGE_POLICY_REASON=\
"bundle contains more than ${MACOS_PACKAGE_POLICY_MAX_BUNDLE_ENTRIES} members"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    macos_package_policy_manifest_record_is_well_formed "$record" || return $?
    relative_path="$MACOS_PACKAGE_POLICY_MANIFEST_PATH"
    if [[ "$relative_path" == . && "$MACOS_PACKAGE_POLICY_MANIFEST_KIND" == D ]]; then
      saw_root=true
    elif [[ "$relative_path" == Contents \
        && "$MACOS_PACKAGE_POLICY_MANIFEST_KIND" == D ]]; then
      saw_contents=true
    fi
    if macos_copy_control_relative_path_is_prohibited "$relative_path"; then
      MACOS_PACKAGE_POLICY_REASON=\
"bundle member ${relative_path} has a path component matching forbidden token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}'"
      MACOS_PACKAGE_POLICY_RESULT="prohibited"
      return 1
    fi
    if [[ "$MACOS_PACKAGE_POLICY_MANIFEST_KIND" == L ]]; then
      if macos_copy_control_relative_path_is_prohibited \
          "$MACOS_PACKAGE_POLICY_MANIFEST_LINK_TARGET" \
          || macos_copy_control_relative_path_is_prohibited \
            "$MACOS_PACKAGE_POLICY_MANIFEST_RESOLVED_TARGET"; then
        MACOS_PACKAGE_POLICY_REASON=\
"bundle symlink ${relative_path} targets a path with a forbidden component (token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}')"
        MACOS_PACKAGE_POLICY_RESULT="prohibited"
        return 1
      fi
    fi
  done < "$manifest"

  if [[ -n "$record" ]]; then
    MACOS_PACKAGE_POLICY_REASON="macOS bundle content manifest is not completely NUL-delimited"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if [[ "$saw_root" != true || "$saw_contents" != true ]]; then
    MACOS_PACKAGE_POLICY_REASON="bundle manifest omits its root or Contents directory"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
}

macos_validate_bundle_import_manifest() {
  local bundle_root="$1"
  local manifest="$2"
  local record artifact candidate_status validation_status
  local entry_count=0
  local deadline=$((SECONDS + MACOS_PACKAGE_POLICY_MAX_BUNDLE_IMPORT_SECONDS))
  local remaining_seconds

  while IFS= read -r -d '' record; do
    macos_package_policy_manifest_record_is_well_formed "$record" || return $?
    [[ "$MACOS_PACKAGE_POLICY_MANIFEST_KIND" == F ]] || continue
    artifact="$bundle_root/$MACOS_PACKAGE_POLICY_MANIFEST_PATH"
    if [[ ! -f "$artifact" || -L "$artifact" ]]; then
      MACOS_PACKAGE_POLICY_REASON="bundle import candidate changed type during inspection"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    candidate_status=0
    macos_bundle_artifact_requires_import_scan \
      "$bundle_root" "$artifact" \
      "$MACOS_PACKAGE_POLICY_MANIFEST_EXECUTABLE" \
      "$MACOS_PACKAGE_POLICY_MANIFEST_MAGIC" || candidate_status=$?
    case "$candidate_status" in
      0) ;;
      1) continue ;;
      *) return "$candidate_status" ;;
    esac
    entry_count=$((entry_count + 1))
    if [[ "$entry_count" -gt "$MACOS_PACKAGE_POLICY_MAX_IMPORT_CANDIDATES" ]]; then
      MACOS_PACKAGE_POLICY_REASON=\
"bundle contains more than ${MACOS_PACKAGE_POLICY_MAX_IMPORT_CANDIDATES} native import candidates"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    validation_status=0
    remaining_seconds=$((deadline - SECONDS))
    if [[ "$remaining_seconds" -le 0 ]]; then
      MACOS_PACKAGE_POLICY_REASON="bundle native-import inspection exceeded its time budget"
      MACOS_PACKAGE_POLICY_RESULT="uninspectable"
      return 2
    fi
    macos_validate_macho_copy_control "$artifact" false "$remaining_seconds" \
      || validation_status=$?
    [[ "$validation_status" -eq 0 ]] || return "$validation_status"
  done < "$manifest"
}

macos_validate_bundle_copy_control() {
  local bundle_root="$1"
  local physical_root manifest_dir members_before members_after validation_status

  MACOS_PACKAGE_POLICY_REASON=""
  MACOS_PACKAGE_POLICY_RESULT=""
  if [[ "$MACOS_FORBIDDEN_COMPONENT_TOKEN_COUNT" -eq 0 ]]; then
    MACOS_PACKAGE_POLICY_REASON="bundled-component policy has not been loaded"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if ! macos_package_policy_reference_is_well_formed "macOS bundle path" "$bundle_root"; then
    return 2
  fi
  bundle_root="${bundle_root%/}"
  if [[ ! -d "$bundle_root" || -L "$bundle_root" \
      || ! -d "$bundle_root/Contents" || -L "$bundle_root/Contents" ]]; then
    MACOS_PACKAGE_POLICY_REASON=\
"macOS bundle and its Contents member must be real directories: ${bundle_root}"
    MACOS_PACKAGE_POLICY_RESULT="error"
    return 1
  fi
  if ! physical_root="$(CDPATH= cd -- "$bundle_root" && pwd -P)"; then
    MACOS_PACKAGE_POLICY_REASON="could not resolve physical macOS bundle root"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  if macos_copy_control_path_is_prohibited "$bundle_root"; then
    MACOS_PACKAGE_POLICY_REASON=\
"bundle name ${bundle_root##*/} matches forbidden token '${MACOS_PACKAGE_POLICY_MATCHED_TOKEN}'"
    MACOS_PACKAGE_POLICY_RESULT="prohibited"
    return 1
  fi
  if ! manifest_dir="$(mktemp -d "${TMPDIR:-/tmp}/balun-macos-bundle-policy.XXXXXX")"; then
    MACOS_PACKAGE_POLICY_REASON="could not create a private bundle-policy directory"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi
  members_before="${manifest_dir}/members-before.nul"
  members_after="${manifest_dir}/members-after.nul"

  if ! macos_package_policy_build_tree_manifest "$physical_root" "$members_before"; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return 2
  fi
  if ! macos_package_policy_check_manifest_shape "$members_before" \
      "macOS bundle member manifest"; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return 2
  fi
  validation_status=0
  macos_validate_bundle_member_manifest "$members_before" \
    || validation_status=$?
  if [[ "$validation_status" -ne 0 ]]; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return "$validation_status"
  fi

  validation_status=0
  macos_validate_bundle_import_manifest "$physical_root" "$members_before" \
    || validation_status=$?
  if [[ "$validation_status" -ne 0 ]]; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return "$validation_status"
  fi

  if ! macos_package_policy_build_tree_manifest "$physical_root" "$members_after"; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return 2
  fi
  if ! macos_package_policy_check_manifest_shape "$members_after" \
      "macOS bundle revalidation manifest"; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return 2
  fi
  validation_status=0
  macos_validate_bundle_member_manifest "$members_after" \
    || validation_status=$?
  if [[ "$validation_status" -ne 0 ]]; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    return "$validation_status"
  fi
  if ! cmp -s "$members_before" "$members_after"; then
    macos_package_policy_remove_private_dir "$manifest_dir" || true
    MACOS_PACKAGE_POLICY_REASON=\
"macOS bundle changed during component-policy validation: ${bundle_root}"
    MACOS_PACKAGE_POLICY_RESULT="uninspectable"
    return 2
  fi

  macos_package_policy_remove_private_dir "$manifest_dir" || return 2
  MACOS_PACKAGE_POLICY_RESULT="allowed"
}
