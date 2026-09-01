#!/usr/bin/env bash
# Linux artifact policy: Balun may use ordinary distro/runtime codecs, but its
# own payload must not bundle or link components denied by the reviewed shared
# release policy. This is intentionally Linux-only; other platforms own their
# independent package layouts and validation.
#
# This v0.1 port is a trusted-build-output gate with deterministic synthetic
# coverage. Before release jobs accept externally supplied archives, add fixed
# archive/tree count and byte budgets, stable artifact snapshot/reopen checks,
# and explicit extraction containment/root-escape enforcement.

set -euo pipefail
set -f
export LC_ALL=C
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
policy_file="$repository_root/build-aux/packaging/forbidden-bundled-components.txt"
expected_policy_sha256=844f3ab37329b0785cf82ae8c29c6665f5052998ea3def790630b239408c8bed
max_policy_bytes=65536
max_policy_lines=1024
policy_snapshot=
policy_tokens=()

usage()
{
    cat >&2 <<'EOF'
Usage:
  validate-package-compliance.sh --tree DIRECTORY
  validate-package-compliance.sh --elf FILE
  validate-package-compliance.sh --deb FILE.deb
  validate-package-compliance.sh --rpm FILE.rpm
  validate-package-compliance.sh --arch FILE.pkg.tar.zst
  validate-package-compliance.sh --metadata FILE...
EOF
    exit 2
}

fail()
{
    echo "Linux package compliance violation: $*" >&2
    exit 1
}

setup_error()
{
    echo "Linux package compliance setup error: $*" >&2
    exit 2
}

require_command()
{
    command -v "$1" >/dev/null 2>&1 || \
        setup_error "required command '$1' is unavailable"
}

check_utf8_text()
{
    local label input status
    label=$1
    input=$2
    if perl -MEncode=decode,FB_CROAK -e '
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
    ' -- "$input"; then
        return 0
    else
        status=$?
    fi

    case "$status" in
        10) setup_error "$label contains a NUL byte: $input" ;;
        11) setup_error "$label is not valid UTF-8 text: $input" ;;
        *) setup_error "could not validate $label as UTF-8 text: $input" ;;
    esac
}

load_policy()
{
    local policy_bytes policy_digest policy_digest_after policy_line_count line token existing
    [ -f "$policy_file" ] && [ ! -L "$policy_file" ] || \
        setup_error "required policy is missing or is not a regular file: $policy_file"

    policy_snapshot=$(mktemp) || setup_error "could not create a private policy snapshot"
    trap 'rm -f -- "$policy_snapshot"' EXIT HUP INT TERM
    cp -- "$policy_file" "$policy_snapshot" || \
        setup_error "could not snapshot policy: $policy_file"

    policy_bytes=$(wc -c < "$policy_snapshot") || \
        setup_error "could not measure policy: $policy_file"
    [[ "$policy_bytes" =~ ^[0-9]+$ ]] || \
        setup_error "policy has an invalid size: $policy_file"
    [ "$policy_bytes" -le "$max_policy_bytes" ] || \
        setup_error "policy exceeds the $max_policy_bytes-byte limit: $policy_file"
    check_utf8_text "policy" "$policy_snapshot"

    policy_digest=$(sha256sum -- "$policy_snapshot") || \
        setup_error "could not hash policy: $policy_file"
    policy_digest=${policy_digest%% *}
    [[ "$policy_digest" =~ ^[0-9a-f]{64}$ ]] || \
        setup_error "policy hash has an invalid format: $policy_file"
    [ "$policy_digest" = "$expected_policy_sha256" ] || \
        setup_error "policy does not match the reviewed component set: $policy_file"

    policy_tokens=()
    policy_line_count=0
    while IFS= read -r line || [ -n "$line" ]; do
        policy_line_count=$((policy_line_count + 1))
        [ "$policy_line_count" -le "$max_policy_lines" ] || \
            setup_error "policy contains more than $max_policy_lines lines"
        [ "${#line}" -le 1024 ] || setup_error "policy contains an overlong line"
        token=${line%$'\r'}
        token=$(printf '%s' "$token" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
        case "$token" in
            '' | \#*) continue ;;
        esac
        [ "${#token}" -le 64 ] || setup_error "policy contains an overlong token"
        [[ "$token" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] || \
            setup_error "policy contains an invalid filename token: $token"
        token=${token,,}
        for existing in "${policy_tokens[@]}"; do
            [ "$existing" != "$token" ] || \
                setup_error "policy contains a duplicate filename token: $token"
        done
        policy_tokens+=("$token")
        [ "${#policy_tokens[@]}" -le 256 ] || \
            setup_error "policy contains more than 256 filename tokens"
    done < "$policy_snapshot"
    [ "${#policy_tokens[@]}" -gt 0 ] || \
        setup_error "policy contains no filename tokens: $policy_file"

    # Hash the exact private snapshot again after parsing. A partial read or a
    # concurrent mutation cannot produce an enforcement set different from the
    # reviewed bytes and still be accepted.
    policy_digest_after=$(sha256sum -- "$policy_snapshot") || \
        setup_error "could not re-hash policy: $policy_file"
    policy_digest_after=${policy_digest_after%% *}
    [ "$policy_digest_after" = "$expected_policy_sha256" ] || \
        setup_error "policy snapshot changed while it was being parsed: $policy_file"
}

forbidden_component()
{
    component=${1##*/}
    component=${component,,}
    for token in "${policy_tokens[@]}"; do
        case "$component" in
            *"$token"*) return 0 ;;
        esac
    done
    return 1
}

forbidden_path_component()
{
    path=${1,,}
    for token in "${policy_tokens[@]}"; do
        case "$path" in
            *"$token"*) return 0 ;;
        esac
    done
    return 1
}

check_component()
{
    forbidden_component "$1" && fail "prohibited component '$1'"
    return 0
}

check_path_components()
{
    forbidden_path_component "$1" && fail "prohibited component reference '$1'"
    return 0
}

check_dependency_text()
(
    input=$1
    # Inspect tokenized package relationships and plain-text installer
    # metadata, never arbitrary binary contents. The shared reviewed list is
    # the sole negative source; ordinary codecs and general-purpose crypto
    # remain eligible unless that policy changes.
    tokens=$(mktemp)
    trap 'rm -f "$tokens"' EXIT HUP INT TERM
    if ! LC_ALL=C tr -s '[:space:],()[]<>=:;|"' '\n' < "$input" > "$tokens"; then
        fail "could not tokenize dependency metadata: $input"
    fi
    while IFS= read -r token || [ -n "$token" ]; do
        [ -z "$token" ] || check_path_components "$token"
    done < "$tokens"
)

elf_inspector()
{
    command -v readelf >/dev/null 2>&1 || {
        echo "Linux package compliance validator requires GNU readelf (binutils)" >&2
        exit 2
    }
    printf '%s\n' readelf
}

is_elf()
{
    [ -f "$1" ] || return 1
    if ! magic=$(LC_ALL=C od -An -tx1 -N4 -- "$1" 2>/dev/null | tr -d ' \n'); then
        return 2
    fi
    [ "$magic" = 7f454c46 ]
}

check_elf()
(
    file=$1
    required=${2:-false}
    if is_elf "$file"; then
        :
    else
        magic_status=$?
        [ "$magic_status" -eq 1 ] || fail "could not inspect file magic: $file"
        [ "$required" = false ] || fail "expected an ELF artifact: $file"
        return 0
    fi

    inspector=$(elf_inspector)
    inspection_dir=$(mktemp -d)
    trap 'rm -rf "$inspection_dir"' EXIT HUP INT TERM
    dynamic="$inspection_dir/dynamic"
    program_headers="$inspection_dir/program-headers"
    references="$inspection_dir/references"
    if ! LC_ALL=C "$inspector" -d -- "$file" > "$dynamic" 2>/dev/null; then
        fail "could not inspect ELF dynamic section: $file"
    fi
    if ! LC_ALL=C "$inspector" -l -- "$file" > "$program_headers" 2>/dev/null; then
        fail "could not inspect ELF program headers: $file"
    fi
    # DT_NEEDED is only one way an ELF can name another component. Inspect all
    # bracket-valued dynamic metadata (including FILTER, AUXILIARY, AUDIT,
    # DEPAUDIT, SONAME, and RUNPATH) plus PT_INTERP from program headers.
    if ! LC_ALL=C sed -n 's/.*\[\([^]]*\)\].*/\1/p' "$dynamic" > "$references"; then
        fail "could not parse ELF dynamic section: $file"
    fi
    if ! LC_ALL=C sed -n 's/.*\[\([^]]*\)\].*/\1/p' "$program_headers" >> "$references"; then
        fail "could not parse ELF program headers: $file"
    fi
    check_dependency_text "$references"
)

check_entry()
{
    entry=$1
    check_component "$entry"
    if [ -L "$entry" ]; then
        target=$(readlink -- "$entry") || fail "could not inspect symlink: $entry"
        check_path_components "$target"
    elif [ -f "$entry" ]; then
        check_elf "$entry" false
    fi
}

check_tree()
(
    root=$1
    [ -d "$root" ] && [ ! -L "$root" ] || {
        echo "Linux package payload is missing or is not a directory: $root" >&2
        exit 2
    }

    # Process substitution hides find's exit status from the parent shell.
    # Enumerate first and inspect only after a complete, successful traversal
    # so unreadable or disappearing payload paths can never yield a partial
    # policy pass.
    entries=$(mktemp)
    trap 'rm -f "$entries"' EXIT HUP INT TERM
    if ! find "$root" -mindepth 1 -print0 > "$entries"; then
        fail "could not enumerate package payload tree: $root"
    fi
    while IFS= read -r -d '' entry; do
        check_entry "$entry"
    done < "$entries"
)

check_text_metadata_file()
{
    entry=$1
    check_component "$entry"
    [ -f "$entry" ] && [ ! -L "$entry" ] || \
        fail "package control metadata is not a regular file: $entry"
    # Installer metadata is defined as text. Reject an unexpected binary
    # member instead of substring-scanning arbitrary bytes as script tokens.
    if [ -s "$entry" ] && ! LC_ALL=C grep -Iq '' "$entry"; then
        fail "package control metadata is not plain text: $entry"
    fi
    check_dependency_text "$entry"
}

check_text_metadata_tree()
(
    root=$1
    entries=$(mktemp)
    trap 'rm -f "$entries"' EXIT HUP INT TERM
    if ! find "$root" -type f -print0 > "$entries"; then
        fail "could not enumerate package control metadata: $root"
    fi
    while IFS= read -r -d '' entry; do
        check_text_metadata_file "$entry"
    done < "$entries"
)

extract_deb()
(
    package=$1
    require_command dpkg-deb
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
    dpkg-deb --control "$package" "$temp_dir/control" || \
        fail "could not extract Debian control metadata"
    check_tree "$temp_dir/control"
    check_text_metadata_tree "$temp_dir/control"
    dpkg-deb --extract "$package" "$temp_dir/payload" || fail "could not extract Debian package"
    check_tree "$temp_dir/payload"
)

extract_rpm()
(
    package=$1
    require_command rpm
    require_command rpm2cpio
    require_command cpio
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
    : > "$temp_dir/header-metadata"
    for query in \
        --requires --recommends --suggests --supplements --enhances \
        --conflicts --obsoletes --provides \
        --scripts --triggers --filetriggers
    do
        rpm -qp "$query" "$package" >> "$temp_dir/header-metadata" 2>/dev/null || \
            fail "could not read RPM header metadata ($query)"
    done
    check_dependency_text "$temp_dir/header-metadata"
    rpm2cpio "$package" > "$temp_dir/payload.cpio" || fail "could not decode RPM payload"
    mkdir "$temp_dir/payload"
    (cd "$temp_dir/payload" && cpio -idm --quiet < "$temp_dir/payload.cpio") || \
        fail "could not extract RPM payload"
    check_tree "$temp_dir/payload"
)

extract_arch()
(
    package=$1
    require_command bsdtar
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
    bsdtar -xOf "$package" .PKGINFO > "$temp_dir/pkginfo" || \
        fail "could not read Arch package metadata"
    check_dependency_text "$temp_dir/pkginfo"
    mkdir "$temp_dir/payload"
    bsdtar -xf "$package" -C "$temp_dir/payload" || fail "could not extract Arch package"
    check_tree "$temp_dir/payload"
    install_script="$temp_dir/payload/.INSTALL"
    if [ -e "$install_script" ] || [ -L "$install_script" ]; then
        check_text_metadata_file "$install_script"
    fi
)

for required_command in cp mktemp perl rm sed sha256sum wc; do
    require_command "$required_command"
done
perl -MEncode -e 'exit 0' >/dev/null 2>&1 || \
    setup_error "required Perl Encode module is unavailable"
load_policy

[ "$#" -ge 2 ] || usage
mode=$1
shift

case "$mode" in
    --tree)
        [ "$#" -eq 1 ] || usage
        check_tree "$1"
        ;;
    --elf)
        [ "$#" -eq 1 ] || usage
        [ -f "$1" ] && [ ! -L "$1" ] || {
            echo "Linux ELF artifact is missing or is not a regular file: $1" >&2
            exit 2
        }
        check_component "$1"
        check_elf "$1" true
        ;;
    --deb)
        [ "$#" -eq 1 ] || usage
        [ -f "$1" ] && [ ! -L "$1" ] || {
            echo "Debian artifact is missing or is not a regular file: $1" >&2
            exit 2
        }
        check_component "$1"
        extract_deb "$1"
        ;;
    --rpm)
        [ "$#" -eq 1 ] || usage
        [ -f "$1" ] && [ ! -L "$1" ] || {
            echo "RPM artifact is missing or is not a regular file: $1" >&2
            exit 2
        }
        check_component "$1"
        extract_rpm "$1"
        ;;
    --arch)
        [ "$#" -eq 1 ] || usage
        [ -f "$1" ] && [ ! -L "$1" ] || {
            echo "Arch artifact is missing or is not a regular file: $1" >&2
            exit 2
        }
        check_component "$1"
        extract_arch "$1"
        ;;
    --metadata)
        [ "$#" -ge 1 ] || usage
        for metadata in "$@"; do
            [ -f "$metadata" ] || {
                echo "Linux packaging metadata not found: $metadata" >&2
                exit 2
            }
            check_text_metadata_file "$metadata"
        done
        ;;
    --entry)
        [ "$#" -eq 1 ] || usage
        check_entry "$1"
        ;;
    *)
        usage
        ;;
esac
