#!/usr/bin/env bash
# Linux artifact policy: Balun may use ordinary distro/runtime codecs, but its
# own payload must not bundle or link components denied by the reviewed shared
# release policy. This is intentionally Linux-only; other platforms own their
# independent package layouts and validation.
#
# This v0.1 port is a trusted-build-output gate with deterministic synthetic
# coverage. Extracted trees are bounded and must produce matching metadata-and-
# content snapshots around inspection. Native package extractors still own
# archive-path interpretation, however: archive source replacement, resource
# amplification, and extraction containment are not bounded before this tree
# gate runs. Release jobs must accept only locally produced artifacts until
# archive-member preflight and extractor-specific containment land separately.

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
max_tree_entries=8192
max_tree_regular_bytes=1073741824
max_tree_file_bytes=268435456
max_tree_path_bytes=2048
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
        # The internal standalone-entry mode has no payload root against
        # which a relative target can be proven confined.
        fail "standalone symlink inspection cannot establish payload confinement: $entry"
    elif [ -f "$entry" ]; then
        check_elf "$entry" false
    fi
}

safe_relative_symlink_target()
{
    local relative target parent component depth
    local -a components
    relative=$1
    target=$2

    case "$target" in
        /*) return 1 ;;
    esac
    [ "${#target}" -le "$max_tree_path_bytes" ] || return 1

    parent=${relative%/*}
    [ "$parent" != "$relative" ] || parent=
    depth=0
    if [ -n "$parent" ]; then
        IFS=/ read -r -a components <<< "$parent"
        for component in "${components[@]}"; do
            [ -z "$component" ] || depth=$((depth + 1))
        done
    fi

    IFS=/ read -r -a components <<< "$target"
    for component in "${components[@]}"; do
        case "$component" in
            '' | .) ;;
            ..)
                [ "$depth" -gt 0 ] || return 1
                depth=$((depth - 1))
                ;;
            *) depth=$((depth + 1)) ;;
        esac
    done
    return 0
}

tree_relative_path()
{
    local root entry
    root=$1
    entry=$2
    case "$entry" in
        "$root"/*)
            REPLY=${entry#"$root"/}
            [ -n "$REPLY" ] || return 1
            ;;
        *) return 1 ;;
    esac
}

preflight_tree_entries()
{
    local root entries entry relative target size entry_count regular_bytes
    root=$1
    entries=$2
    entry_count=0
    regular_bytes=0

    while IFS= read -r -d '' entry; do
        entry_count=$((entry_count + 1))
        [ "$entry_count" -le "$max_tree_entries" ] || \
            fail "package payload exceeds the $max_tree_entries-entry limit"
        tree_relative_path "$root" "$entry" || \
            fail "package traversal returned an entry outside its root"
        relative=$REPLY
        [ "${#relative}" -le "$max_tree_path_bytes" ] || \
            fail "package payload path exceeds the $max_tree_path_bytes-byte limit"

        if [ -L "$entry" ]; then
            target=$(readlink -- "$entry") || \
                fail "could not inspect symlink: $entry"
            safe_relative_symlink_target "$relative" "$target" || \
                fail "symlink target is absolute, escapes the payload, or exceeds the path limit: $entry"
        elif [ -f "$entry" ]; then
            size=$(stat -Lc '%s' -- "$entry") || \
                fail "could not measure regular file: $entry"
            [[ "$size" =~ ^[0-9]+$ ]] || \
                fail "regular file has an invalid size: $entry"
            [ "$size" -le "$max_tree_file_bytes" ] || \
                fail "regular file exceeds the $max_tree_file_bytes-byte limit: $entry"
            [ "$regular_bytes" -le $((max_tree_regular_bytes - size)) ] || \
                fail "package payload exceeds the $max_tree_regular_bytes-byte regular-file budget"
            regular_bytes=$((regular_bytes + size))
        elif [ -d "$entry" ]; then
            :
        else
            fail "package payload contains an unsupported file type: $entry"
        fi
    done < "$entries"
}

append_manifest_entry()
{
    local root entry manifest relative kind target hash_output digest size size_after
    root=$1
    entry=$2
    manifest=$3
    tree_relative_path "$root" "$entry" || \
        fail "package traversal returned an entry outside its root"
    relative=$REPLY

    if [ -L "$entry" ]; then
        kind=symlink
        target=$(readlink -- "$entry") || fail "could not inspect symlink: $entry"
        safe_relative_symlink_target "$relative" "$target" || \
            fail "symlink target is absolute, escapes the payload, or exceeds the path limit: $entry"
        printf '%s\0%s\0' "$relative" "$kind" >> "$manifest" || \
            fail "could not write package payload manifest"
        stat --printf='%f\0%s\0%d\0%i\0%a\0%u\0%g\0%y\0%z\0' -- "$entry" \
            >> "$manifest" || fail "could not stat symlink: $entry"
        printf '%s\0' "$target" >> "$manifest" || \
            fail "could not write package payload manifest"
    elif [ -f "$entry" ]; then
        kind=regular
        size=$(stat -Lc '%s' -- "$entry") || \
            fail "could not measure regular file: $entry"
        [[ "$size" =~ ^[0-9]+$ ]] && [ "$size" -le "$max_tree_file_bytes" ] || \
            fail "regular file changed size or exceeds its limit: $entry"
        # Bound the content read even if a concurrent writer grows the file
        # after preflight. The following stat must still report the exact size
        # seen before hashing; the outer manifest comparison catches same-size
        # content or metadata changes.
        hash_output=$(head -c "$((max_tree_file_bytes + 1))" -- "$entry" | sha256sum) || \
            fail "could not hash regular file: $entry"
        hash_output=${hash_output#\\}
        digest=${hash_output%% *}
        [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || \
            fail "regular file hash has an invalid format: $entry"
        [ -f "$entry" ] && [ ! -L "$entry" ] || \
            fail "regular file changed type while being hashed: $entry"
        size_after=$(stat -Lc '%s' -- "$entry") || \
            fail "could not remeasure regular file: $entry"
        [ "$size_after" = "$size" ] || \
            fail "regular file changed size while being hashed: $entry"
        printf '%s\0%s\0' "$relative" "$kind" >> "$manifest" || \
            fail "could not write package payload manifest"
        stat -L --printf='%f\0%s\0%d\0%i\0%a\0%u\0%g\0%y\0%z\0' -- "$entry" \
            >> "$manifest" || fail "could not stat regular file: $entry"
        printf '%s\0' "$digest" >> "$manifest" || \
            fail "could not write package payload manifest"
    elif [ -d "$entry" ]; then
        kind=directory
        printf '%s\0%s\0' "$relative" "$kind" >> "$manifest" || \
            fail "could not write package payload manifest"
        stat -L --printf='%f\0%s\0%d\0%i\0%a\0%u\0%g\0%y\0%z\0' -- "$entry" \
            >> "$manifest" || fail "could not stat directory: $entry"
    else
        fail "package payload entry changed to an unsupported file type: $entry"
    fi
}

build_tree_manifest()
{
    local root manifest work_prefix unsorted entries entry
    root=$1
    manifest=$2
    work_prefix=$3
    unsorted="$work_prefix.unsorted"
    entries="$work_prefix.entries"

    : > "$manifest" || fail "could not initialize package payload manifest"
    # Include the root record so replacement, permission changes, and even a
    # transient create/remove cycle that restores the same children are not
    # invisible to the before/after comparison.
    printf '.\0directory\0' >> "$manifest" || \
        fail "could not write package payload manifest"
    stat -L --printf='%f\0%s\0%d\0%i\0%a\0%u\0%g\0%y\0%z\0' -- "$root" \
        >> "$manifest" || fail "could not stat package payload root"

    # Sorting NUL-delimited paths makes the snapshot independent of directory
    # enumeration order and includes dotfiles without any special cases.
    if ! find "$root" -mindepth 1 -print0 | \
        TREE_ENTRY_LIMIT="$max_tree_entries" \
        TREE_PATH_LIMIT="$max_tree_path_bytes" \
        perl -0 -e '
            use strict;
            use warnings;
            my $entry_limit = $ENV{TREE_ENTRY_LIMIT};
            my $path_limit = $ENV{TREE_PATH_LIMIT};
            my $count = 0;
            while (defined(my $entry = <STDIN>)) {
                chomp $entry;
                ++$count;
                die "entry limit\n" if $count > $entry_limit;
                die "invalid rooted path\n"
                    unless length($entry) > 2 && substr($entry, 0, 2) eq "./";
                die "path limit\n" if length(substr($entry, 2)) > $path_limit;
                print $entry, "\0" or die "snapshot write\n";
            }
            close STDOUT or die "snapshot close\n";
        ' > "$unsorted"
    then
        fail "could not enumerate package payload tree within its entry and path limits: $root"
    fi
    if ! sort -z -- "$unsorted" > "$entries"; then
        fail "could not sort package payload tree: $root"
    fi
    preflight_tree_entries "$root" "$entries"
    while IFS= read -r -d '' entry; do
        append_manifest_entry "$root" "$entry" "$manifest"
    done < "$entries"
}

check_tree_entry()
{
    local root entry relative target
    root=$1
    entry=$2
    tree_relative_path "$root" "$entry" || \
        fail "package traversal returned an entry outside its root"
    relative=$REPLY
    check_component "$entry"
    if [ -L "$entry" ]; then
        target=$(readlink -- "$entry") || fail "could not inspect symlink: $entry"
        safe_relative_symlink_target "$relative" "$target" || \
            fail "symlink target is absolute, escapes the payload, or exceeds the path limit: $entry"
        check_path_components "$target"
    elif [ -f "$entry" ]; then
        check_elf "$entry" false
    elif [ ! -d "$entry" ]; then
        fail "package payload contains an unsupported file type: $entry"
    fi
}

check_tree()
(
    input_root=$1
    inspection_mode=${2:-payload}
    root_argument=$input_root
    while [ "$root_argument" != / ] && [ "${root_argument%/}" != "$root_argument" ]; do
        root_argument=${root_argument%/}
    done
    while [ "$root_argument" != / ] && [ "${root_argument%/.}" != "$root_argument" ]; do
        root_argument=${root_argument%/.}
        while [ "$root_argument" != / ] && [ "${root_argument%/}" != "$root_argument" ]; do
            root_argument=${root_argument%/}
        done
    done
    [ -d "$input_root" ] && [ ! -L "$root_argument" ] || {
        echo "Linux package payload is missing or is not a directory: $input_root" >&2
        exit 2
    }
    display_root=$(CDPATH= cd -- "$input_root" 2>/dev/null && pwd -P) || {
        echo "Linux package payload root cannot be resolved: $input_root" >&2
        exit 2
    }
    [ "$display_root" != / ] || {
        echo "Linux package payload root must not be the filesystem root" >&2
        exit 2
    }
    [ -d "$display_root" ] && [ ! -L "$display_root" ] || \
        fail "package payload root changed while it was being resolved"
    cd -- "$display_root" || fail "could not pin package payload root"
    root=.
    root_identity=$(stat -Lc '%d:%i' -- "$root") || \
        fail "could not identify package payload root"
    display_identity=$(stat -Lc '%d:%i' -- "$display_root") || \
        fail "package payload root disappeared while it was being pinned"
    [ "$display_identity" = "$root_identity" ] || \
        fail "package payload root was replaced while it was being pinned"

    scan_dir=$(mktemp -d) || setup_error "could not create a private tree snapshot directory"
    trap 'rm -rf -- "$scan_dir"' EXIT HUP INT TERM
    before="$scan_dir/before"
    after="$scan_dir/after"
    build_tree_manifest "$root" "$before" "$scan_dir/first"
    display_identity=$(stat -Lc '%d:%i' -- "$display_root") || \
        fail "package payload root disappeared during inspection"
    [ "$display_identity" = "$root_identity" ] || \
        fail "package payload root was replaced during inspection"

    entries="$scan_dir/first.entries"
    while IFS= read -r -d '' entry; do
        check_tree_entry "$root" "$entry"
        if [ "$inspection_mode" = metadata ] && [ -f "$entry" ] && [ ! -L "$entry" ]; then
            check_text_metadata_file "$entry"
        fi
    done < "$entries"

    build_tree_manifest "$root" "$after" "$scan_dir/second"
    display_identity=$(stat -Lc '%d:%i' -- "$display_root") || \
        fail "package payload root disappeared during inspection"
    [ "$display_identity" = "$root_identity" ] || \
        fail "package payload root was replaced during inspection"
    if ! cmp -s -- "$before" "$after"; then
        fail "package payload changed while it was being inspected: $display_root"
    fi
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
{
    check_tree "$1" metadata
}

extract_deb()
(
    package=$1
    require_command dpkg-deb
    temp_dir=$(mktemp -d)
    trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
    dpkg-deb --control "$package" "$temp_dir/control" || \
        fail "could not extract Debian control metadata"
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

for required_command in cmp cp find head mktemp perl readlink rm sed sha256sum sort stat wc; do
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
