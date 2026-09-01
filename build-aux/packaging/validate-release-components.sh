#!/usr/bin/env bash
# Validate Balun's source and packaging inputs against the shared release
# component policy. This is deliberately an input gate, not a substitute for
# inspecting native dependency closure or completed application artifacts.

set -euo pipefail
set -f
export LC_ALL=C

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
policy_file="$script_dir/forbidden-bundled-components.txt"
max_input_bytes=67108864
max_total_input_bytes=67108864
max_scanned_inputs=4096
max_policy_bytes=65536
max_policy_lines=1024
max_repository_entries=100000
expected_policy_sha256=844f3ab37329b0785cf82ae8c29c6665f5052998ea3def790630b239408c8bed
policy_pattern_file=
scanned_input_bytes=0
scanned_input_count=0

usage()
{
    printf '%s\n' \
        'Usage:' \
        '  validate-release-components.sh --repository' \
        '  validate-release-components.sh --inputs FILE...' >&2
    exit 2
}

setup_error()
{
    echo "Release component policy setup error: $*" >&2
    exit 2
}

violation()
{
    echo "Release component policy violation: $*" >&2
    exit 1
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
    local policy_bytes policy_digest policy_line_count
    [ -f "$policy_file" ] && [ ! -L "$policy_file" ] || \
        setup_error "required policy is missing or is not a regular file: $policy_file"
    policy_bytes=$(wc -c < "$policy_file") || \
        setup_error "could not measure policy: $policy_file"
    [[ "$policy_bytes" =~ ^[0-9]+$ ]] || \
        setup_error "policy has an invalid size: $policy_file"
    [ "$policy_bytes" -le "$max_policy_bytes" ] || \
        setup_error "policy exceeds the $max_policy_bytes-byte limit: $policy_file"
    check_utf8_text "policy" "$policy_file"

    policy_tokens=()
    policy_line_count=0
    while IFS= read -r line || [ -n "$line" ]; do
        policy_line_count=$((policy_line_count + 1))
        [ "$policy_line_count" -le "$max_policy_lines" ] || \
            setup_error "policy contains more than $max_policy_lines lines"
        [ "${#line}" -le 1024 ] || setup_error "policy contains an overlong line"
        line=${line%$'\r'}
        token=$(printf '%s' "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
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
    done < "$policy_file"

    [ "${#policy_tokens[@]}" -gt 0 ] || \
        setup_error "policy contains no filename tokens: $policy_file"

    policy_digest=$(sha256sum -- "$policy_file") || \
        setup_error "could not hash policy: $policy_file"
    policy_digest=${policy_digest%% *}
    [[ "$policy_digest" =~ ^[0-9a-f]{64}$ ]] || \
        setup_error "policy hash has an invalid format: $policy_file"
    [ "$policy_digest" = "$expected_policy_sha256" ] || \
        setup_error "policy does not match the reviewed component set: $policy_file"

    policy_pattern_file=$(mktemp) || \
        setup_error "could not create a component-pattern file"
    trap 'rm -f -- "$policy_pattern_file"' EXIT HUP INT TERM
    {
        for token in "${policy_tokens[@]}"; do
            printf '%s\n' "$token"
        done
    } > "$policy_pattern_file" || \
        setup_error "could not write the component-pattern file"
}

matched_token=
reference_is_forbidden()
{
    local reference normalized token
    reference=$1
    normalized=${reference,,}
    for token in "${policy_tokens[@]}"; do
        if [[ "$normalized" == *"$token"* ]]; then
            matched_token=$token
            return 0
        fi
    done
    matched_token=
    return 1
}

check_reference()
{
    local label reference
    label=$1
    reference=$2
    [ "${#reference}" -le 4096 ] || \
        setup_error "$label exceeds the 4096-byte limit"
    [[ "$reference" != *[$'\001'-$'\037'$'\177']* ]] || \
        setup_error "$label contains an unsupported control character"
    if reference_is_forbidden "$reference"; then
        violation "$label matches denied filename token '$matched_token': $reference"
    fi
}

check_text_input()
{
    local input input_bytes remaining status
    input=$1
    [ -f "$input" ] && [ ! -L "$input" ] || \
        setup_error "packaging input is not a regular file: $input"
    input_bytes=$(wc -c < "$input") || \
        setup_error "could not measure packaging input: $input"
    [[ "$input_bytes" =~ ^[0-9]+$ ]] || \
        setup_error "packaging input has an invalid size: $input"
    [ "$input_bytes" -le "$max_input_bytes" ] || \
        setup_error "packaging input exceeds the $max_input_bytes-byte limit: $input"
    [ "$scanned_input_count" -lt "$max_scanned_inputs" ] || \
        setup_error "more than $max_scanned_inputs packaging inputs require scanning"
    remaining=$((max_total_input_bytes - scanned_input_bytes))
    [ "$input_bytes" -le "$remaining" ] || \
        setup_error "packaging inputs exceed the cumulative $max_total_input_bytes-byte limit"
    scanned_input_count=$((scanned_input_count + 1))
    scanned_input_bytes=$((scanned_input_bytes + input_bytes))

    check_utf8_text "packaging input" "$input"
    if grep -Fqi -f "$policy_pattern_file" -- "$input"; then
        violation "packaging input references a denied filename token: $input"
    else
        status=$?
        [ "$status" -eq 1 ] || \
            setup_error "could not scan packaging input: $input"
    fi
}

is_scanned_text_input()
{
    case "$1" in
        *.rs | *.toml | Cargo.lock | */Cargo.lock | \
        .cargo/config | .cargo/config.toml | */.cargo/config | */.cargo/config.toml | \
        .github/workflows/* | .github/actions/* | \
        .githooks/* | build-aux/* | packaging/* | \
        *.sh | *.bash | *.ps1 | *.psm1 | *.cmd | *.bat | \
        *.py | *.rb | *.pl | \
        scripts/build-* | scripts/package-* | scripts/release-* | \
        Makefile | */Makefile | GNUmakefile | */GNUmakefile | \
        makefile | */makefile | *.mk | \
        CMakeLists.txt | */CMakeLists.txt | *.cmake | \
        meson.build | */meson.build | meson_options.txt | */meson_options.txt | *.wrap | \
        configure | */configure | configure.ac | */configure.ac | \
        configure.in | */configure.in | *.m4 | \
        Justfile | */Justfile | justfile | */justfile | \
        Taskfile.yml | */Taskfile.yml | Taskfile.yaml | */Taskfile.yaml | \
        Dockerfile | */Dockerfile | Containerfile | */Containerfile | \
        PKGBUILD | */PKGBUILD | debian/* | */debian/* | \
        snapcraft.yaml | */snapcraft.yaml | snap/snapcraft.yaml | */snap/snapcraft.yaml | \
        *.spec | *.iss | *.nsi | *.nsh | \
        *.wxs | *.wxi | *.wxl | *.wixproj | *.wapproj | *.appxmanifest | \
        *.flatpak.yml | *.flatpak.yaml | *.flatpak.json | \
        *.pkgproj | *.nix | vcpkg.json | */vcpkg.json | \
        conanfile.py | */conanfile.py | conanfile.txt | */conanfile.txt)
            return 0
            ;;
    esac
    return 1
}

check_repository_entry()
{
    local relative absolute target
    relative=$1
    [ -n "$relative" ] || setup_error "Git returned an empty repository path"
    [[ "$relative" != /* && "$relative" != ../* && "$relative" != */../* ]] || \
        setup_error "Git returned a path outside the repository"
    check_reference "repository path" "$relative"

    absolute="$repository_root/$relative"
    if [ -L "$absolute" ]; then
        target=$(readlink -- "$absolute") || \
            setup_error "could not inspect repository symlink: $relative"
        check_reference "repository symlink target" "$target"
        if is_scanned_text_input "$relative"; then
            setup_error "packaging input must not be a symlink: $relative"
        fi
    elif [ -f "$absolute" ]; then
        if { is_scanned_text_input "$relative" || [ -x "$absolute" ]; } && \
            [ "$absolute" != "$policy_file" ]; then
            check_text_input "$absolute"
        fi
    else
        setup_error "tracked repository entry is missing or has an unsupported type: $relative"
    fi
}

validate_repository()
(
    require_command git
    local git_root entries entries_bytes relative entry_count
    git_root=$(git -C "$repository_root" rev-parse --show-toplevel 2>/dev/null) || \
        setup_error "could not resolve the Git repository root"
    [ "$git_root" = "$repository_root" ] || \
        setup_error "validator is not located at the Git repository root"

    entries=$(mktemp) || setup_error "could not create a repository manifest"
    trap 'rm -f "$entries"' EXIT HUP INT TERM
    if ! git -C "$repository_root" ls-files \
        --cached --others --exclude-standard -z > "$entries"; then
        setup_error "could not enumerate repository inputs"
    fi
    entries_bytes=$(wc -c < "$entries") || \
        setup_error "could not measure the repository manifest"
    [[ "$entries_bytes" =~ ^[0-9]+$ ]] || \
        setup_error "repository manifest has an invalid size"
    [ "$entries_bytes" -le "$max_input_bytes" ] || \
        setup_error "repository manifest exceeds the $max_input_bytes-byte limit"

    entry_count=0
    while IFS= read -r -d '' relative; do
        entry_count=$((entry_count + 1))
        [ "$entry_count" -le "$max_repository_entries" ] || \
            setup_error "repository contains more than $max_repository_entries input paths"
        check_repository_entry "$relative"
    done < "$entries"
    [ "$entry_count" -gt 0 ] || setup_error "repository contains no input paths"
)

validate_explicit_input()
{
    local input target
    input=$1
    check_reference "packaging input path" "$input"
    if [ -L "$input" ]; then
        target=$(readlink -- "$input") || \
            setup_error "could not inspect packaging-input symlink: $input"
        check_reference "packaging-input symlink target" "$target"
    fi
    check_text_input "$input"
}

for required_command in grep mktemp perl readlink rm sed sha256sum wc; do
    require_command "$required_command"
done
perl -MEncode -e 'exit 0' >/dev/null 2>&1 || \
    setup_error "required Perl Encode module is unavailable"

load_policy

[ "$#" -ge 1 ] || usage
mode=$1
shift
case "$mode" in
    --repository)
        [ "$#" -eq 0 ] || usage
        validate_repository
        echo "Repository and packaging inputs comply with the release component policy."
        ;;
    --inputs)
        [ "$#" -gt 0 ] || usage
        [ "$#" -le "$max_scanned_inputs" ] || \
            setup_error "more than $max_scanned_inputs packaging inputs were provided"
        for input in "$@"; do
            validate_explicit_input "$input"
        done
        echo "Packaging inputs comply with the release component policy."
        ;;
    *)
        usage
        ;;
esac
