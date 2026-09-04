#!/usr/bin/env bash
# Deterministic positive/negative coverage for the Linux payload scanner. No
# real package manager, public network, media runtime, or forbidden component
# is required.

set -euo pipefail
set -f
export LC_ALL=C

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
validator="$script_dir/validate-package-compliance.sh"
policy="$repository_root/build-aux/packaging/forbidden-bundled-components.txt"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

expect_status()
{
    expected=$1
    shift
    set +e
    "$@" > /dev/null 2>&1
    actual=$?
    set -e
    [ "$actual" -eq "$expected" ] || {
        echo "Expected status $expected, got $actual: $*" >&2
        exit 1
    }
}

require_literal()
{
    file=$1
    literal=$2
    grep -Fq -- "$literal" "$file" || {
        echo "Expected packaging contract missing from $file: $literal" >&2
        exit 1
    }
}

read_validator_limit()
{
    awk -F= -v key="$1" '$1 == key { print $2; found = 1; exit }
        END { if (!found) exit 1 }' "$validator"
}

tree_entry_limit=$(read_validator_limit max_tree_entries)
tree_regular_bytes_limit=$(read_validator_limit max_tree_regular_bytes)
tree_file_bytes_limit=$(read_validator_limit max_tree_file_bytes)
tree_path_bytes_limit=$(read_validator_limit max_tree_path_bytes)
for limit in \
    "$tree_entry_limit" \
    "$tree_regular_bytes_limit" \
    "$tree_file_bytes_limit" \
    "$tree_path_bytes_limit"
do
    [[ "$limit" =~ ^[1-9][0-9]*$ ]] || {
        echo "Linux package validator has an invalid tree resource limit" >&2
        exit 1
    }
done

first_token=$(awk '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print tolower($0); exit }
' "$policy")
[ -n "$first_token" ] || {
    echo "Shared bundled-component policy unexpectedly empty" >&2
    exit 1
}

# Ordinary codecs, generic crypto, and similarly prefixed but unrelated names
# remain eligible. The shared list is deliberately the only negative source.
mkdir -p "$temp_dir/allowed/lib/gstreamer-1.0"
touch "$temp_dir/allowed/lib/gstreamer-1.0/libgstlibav.so"
touch "$temp_dir/allowed/lib/libavcodec.so.62"
touch "$temp_dir/allowed/lib/libcrypto.so.3"
touch "$temp_dir/allowed/lib/libblurhash.so"
touch "$temp_dir/allowed/lib/libbluray.so.2"
ln -s libgstlibav.so "$temp_dir/allowed/lib/gstreamer-1.0/codec-alias.so"
"$validator" --tree "$temp_dir/allowed"
ln -s "$temp_dir/allowed" "$temp_dir/allowed-tree-link"
expect_status 2 "$validator" --tree "$temp_dir/allowed-tree-link"
expect_status 2 "$validator" --tree "$temp_dir/allowed-tree-link/"
expect_status 2 "$validator" --tree "$temp_dir/allowed-tree-link/."

# Payload symlinks must remain lexically confined to the inspected root. An
# absolute target and a relative target that climbs above the root both fail,
# even when the referenced basename would otherwise be allowed.
mkdir -p "$temp_dir/symlink-escape/nested"
ln -s /usr/lib/libgstreamer-1.0.so.0 \
    "$temp_dir/symlink-escape/absolute-link.so"
expect_status 1 "$validator" --tree "$temp_dir/symlink-escape"
rm -f "$temp_dir/symlink-escape/absolute-link.so"
ln -s ../../../outside/libgstreamer-1.0.so.0 \
    "$temp_dir/symlink-escape/nested/relative-link.so"
expect_status 1 "$validator" --tree "$temp_dir/symlink-escape"
rm -f "$temp_dir/symlink-escape/nested/relative-link.so"

# Each tree budget fails before policy inspection. Sparse files exercise byte
# accounting without consuming the corresponding amount of storage.
mkdir -p "$temp_dir/file-limit"
truncate -s "$((tree_file_bytes_limit + 1))" \
    "$temp_dir/file-limit/oversized.bin"
expect_status 1 "$validator" --tree "$temp_dir/file-limit"

mkdir -p "$temp_dir/aggregate-limit"
aggregate_count=$((tree_regular_bytes_limit / tree_file_bytes_limit + 1))
aggregate_index=0
while [ "$aggregate_index" -lt "$aggregate_count" ]; do
    truncate -s "$tree_file_bytes_limit" \
        "$temp_dir/aggregate-limit/file-$aggregate_index.bin"
    aggregate_index=$((aggregate_index + 1))
done
expect_status 1 "$validator" --tree "$temp_dir/aggregate-limit"

mkdir -p "$temp_dir/path-limit"
long_component=$(printf '%0200d' 0 | tr 0 p)
long_path="$temp_dir/path-limit"
long_relative=
while [ "${#long_relative}" -le "$tree_path_bytes_limit" ]; do
    long_path="$long_path/$long_component"
    long_relative=${long_relative:+$long_relative/}$long_component
    mkdir "$long_path"
done
expect_status 1 "$validator" --tree "$temp_dir/path-limit"

mkdir -p "$temp_dir/entry-limit" "$temp_dir/entry-limit-tools"
touch "$temp_dir/entry-limit/entry"
printf '%s\n' \
    '#!/bin/sh' \
    'index=0' \
    'while [ "$index" -lt "$TEST_ENTRY_LIMIT_COUNT" ]; do' \
    '  printf "%s\000" ./entry' \
    '  index=$((index + 1))' \
    'done' \
    > "$temp_dir/entry-limit-tools/find"
chmod +x "$temp_dir/entry-limit-tools/find"
expect_status 1 env PATH="$temp_dir/entry-limit-tools:$PATH" \
    TEST_ENTRY_LIMIT_COUNT="$((tree_entry_limit + 1))" \
    "$validator" --tree "$temp_dir/entry-limit"

mkdir -p "$temp_dir/rejected"
while IFS= read -r line || [ -n "$line" ]; do
    token=$(printf '%s' "$line" | tr -d '\r')
    token=$(printf '%s' "$token" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    case "$token" in
        '' | \#*) continue ;;
    esac
    candidate="$temp_dir/rejected/prefix-${token}-suffix.so"
    touch "$candidate"
    expect_status 1 "$validator" --tree "$temp_dir/rejected"
    rm -f "$candidate"
done < "$policy"

uppercase=$(printf '%s' "$first_token" | tr '[:lower:]' '[:upper:]')
touch "$temp_dir/rejected/${uppercase}.SO"
expect_status 1 "$validator" --tree "$temp_dir/rejected"
rm -f "$temp_dir/rejected/${uppercase}.SO"

ln -s "../${first_token}.so" "$temp_dir/rejected/innocent-link.so"
expect_status 1 "$validator" --tree "$temp_dir/rejected"
rm -f "$temp_dir/rejected/innocent-link.so"

ln -s "../${first_token}/libinnocent.so" "$temp_dir/rejected/innocent-link.so"
expect_status 1 "$validator" --tree "$temp_dir/rejected"
rm -f "$temp_dir/rejected/innocent-link.so"

# A traversal error after producing partial output must fail the scan. This is
# the regression case that process substitution would otherwise conceal.
mkdir -p "$temp_dir/find-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'printf "%s\\0" "$1/lib/gstreamer-1.0/libgstlibav.so"' \
    'exit 7' \
    > "$temp_dir/find-tools/find"
chmod +x "$temp_dir/find-tools/find"
expect_status 1 env PATH="$temp_dir/find-tools:$PATH" \
    "$validator" --tree "$temp_dir/allowed"

# A partial magic read followed by an od error must fail even during a tree
# scan; it must not silently reclassify the regular file as non-ELF.
mkdir -p "$temp_dir/od-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'printf " 7f 45"' \
    'exit 7' \
    > "$temp_dir/od-tools/od"
chmod +x "$temp_dir/od-tools/od"
expect_status 1 env PATH="$temp_dir/od-tools:$PATH" \
    "$validator" --tree "$temp_dir/allowed"

# A replacement of the pathname naming the pinned root must not redirect the
# scan. Replace an otherwise empty root between enumeration and manifesting;
# inode binding, rather than child differences, is what rejects this case.
real_sort=$(command -v sort)
mkdir -p \
    "$temp_dir/root-replacement" \
    "$temp_dir/root-replacement-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'count=0' \
    'if [ -f "$TEST_SORT_STATE" ]; then read -r count < "$TEST_SORT_STATE"; fi' \
    'count=$((count + 1))' \
    'printf "%s\n" "$count" > "$TEST_SORT_STATE"' \
    'if [ "$count" -eq 2 ]; then' \
    '  mv "$TEST_REPLACED_ROOT" "$TEST_REPLACED_ROOT.old"' \
    '  mkdir "$TEST_REPLACED_ROOT"' \
    'fi' \
    'exec "$TEST_REAL_SORT" "$@"' \
    > "$temp_dir/root-replacement-tools/sort"
chmod +x "$temp_dir/root-replacement-tools/sort"
expect_status 1 env PATH="$temp_dir/root-replacement-tools:$PATH" \
    TEST_REAL_SORT="$real_sort" \
    TEST_REPLACED_ROOT="$temp_dir/root-replacement" \
    TEST_SORT_STATE="$temp_dir/root-replacement-sort-state" \
    "$validator" --tree "$temp_dir/root-replacement"

# Hashing dotfiles in both manifests makes a hidden mutation observable. The
# wrapper changes the fixture only on its second tree-specific hash; policy
# hashing and all component names remain derived from normal inputs.
real_sha256sum=$(command -v sha256sum)
mkdir -p "$temp_dir/hidden-mutation" "$temp_dir/sha-tools"
printf 'stable\n' > "$temp_dir/hidden-mutation/.state"
printf '%s\n' \
    '#!/bin/sh' \
    'if [ "$#" -eq 0 ]; then' \
    '  count=0' \
    '  if [ -f "$TEST_SHA_STATE" ]; then read -r count < "$TEST_SHA_STATE"; fi' \
    '  count=$((count + 1))' \
    '  printf "%s\n" "$count" > "$TEST_SHA_STATE"' \
    '  if [ "$count" -eq 2 ]; then printf x >> "$TEST_MUTATION_TARGET"; fi' \
    'fi' \
    'exec "$TEST_REAL_SHA256SUM" "$@"' \
    > "$temp_dir/sha-tools/sha256sum"
chmod +x "$temp_dir/sha-tools/sha256sum"
expect_status 1 env PATH="$temp_dir/sha-tools:$PATH" \
    TEST_REAL_SHA256SUM="$real_sha256sum" \
    TEST_MUTATION_TARGET=./.state \
    TEST_SHA_STATE="$temp_dir/hidden-mutation-sha-state" \
    "$validator" --tree "$temp_dir/hidden-mutation"

# A comparison I/O failure is a policy failure, never evidence that two
# snapshots match.
mkdir -p "$temp_dir/cmp-tools"
printf '%s\n' '#!/bin/sh' 'exit 2' > "$temp_dir/cmp-tools/cmp"
chmod +x "$temp_dir/cmp-tools/cmp"
expect_status 1 env PATH="$temp_dir/cmp-tools:$PATH" \
    "$validator" --tree "$temp_dir/allowed"

printf 'depends = gstreamer1.0-plugins-good\n' > "$temp_dir/allowed-metadata"
"$validator" --metadata "$temp_dir/allowed-metadata"
printf 'depends = %s-runtime\n' "$first_token" > "$temp_dir/rejected-metadata"
expect_status 1 "$validator" --metadata "$temp_dir/rejected-metadata"
printf '\000binary\n' > "$temp_dir/binary-metadata"
expect_status 1 "$validator" --metadata "$temp_dir/binary-metadata"
ln -s "$temp_dir/allowed-metadata" "$temp_dir/symlink-metadata"
expect_status 1 "$validator" --metadata "$temp_dir/symlink-metadata"

# Tokenizer failure after partial allowed output must not become a policy pass.
real_tr=$(command -v tr)
mkdir -p "$temp_dir/tr-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'if [ "$1" = -s ]; then' \
    '  echo gstreamer1.0-plugins-good' \
    '  exit 7' \
    'fi' \
    'exec "$TEST_REAL_TR" "$@"' \
    > "$temp_dir/tr-tools/tr"
chmod +x "$temp_dir/tr-tools/tr"
expect_status 1 env PATH="$temp_dir/tr-tools:$PATH" TEST_REAL_TR="$real_tr" \
    "$validator" --metadata "$temp_dir/allowed-metadata"

# Drive ELF reference parsing through a fixed fake inspector. This covers a
# renamed payload whose basename is harmless but a dynamic tag or PT_INTERP
# names a prohibited component.
mkdir -p "$temp_dir/tools"
printf '%s\n' \
    '#!/bin/sh' \
    'mode=$1' \
    'last=' \
    'for argument in "$@"; do last=$argument; done' \
    'if [ "$mode" = -l ]; then' \
    '  case "$last" in' \
    '    *rejected-elf-interpreter)' \
    '      printf "      [Requesting program interpreter: /lib/%s.so.1]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '      ;;' \
    '    *) printf "Elf file type is DYN (Shared object file)\\n" ;;' \
    '  esac' \
    '  exit 0' \
    'fi' \
    'case "$last" in' \
    '  *rejected-elf-needed)' \
    '    printf " 0x0000000000000001 (NEEDED) Shared library: [%s.so.1]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '    ;;' \
    '  *rejected-elf-filter)' \
    '    printf " 0x000000007fffffff (FILTER) Filter library: [%s.so.1]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '    ;;' \
    '  *rejected-elf-audit)' \
    '    printf " 0x000000006ffffefc (AUDIT) Audit library: [%s.so.1]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '    ;;' \
    '  *rejected-elf-runpath)' \
    '    printf " 0x000000000000001d (RUNPATH) Library runpath: [/opt/%s/lib]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '    ;;' \
    '  *rejected-elf-soname)' \
    '    printf " 0x000000000000000e (SONAME) Library soname: [lib%s-proxy.so]\\n" "$TEST_FORBIDDEN_TOKEN"' \
    '    ;;' \
    '  *)' \
    '    printf " 0x0000000000000001 (NEEDED) Shared library: [libgstreamer-1.0.so.0]\\n"' \
    '    ;;' \
    'esac' \
    > "$temp_dir/tools/readelf"
chmod +x "$temp_dir/tools/readelf"
printf '\177ELFfixture' > "$temp_dir/allowed-elf"
PATH="$temp_dir/tools:$PATH" TEST_FORBIDDEN_TOKEN="$first_token" \
    "$validator" --elf "$temp_dir/allowed-elf"
ln -s "$temp_dir/allowed-elf" "$temp_dir/allowed-elf-link"
expect_status 2 env PATH="$temp_dir/tools:$PATH" \
    "$validator" --elf "$temp_dir/allowed-elf-link"
for elf_reference in needed filter audit runpath soname interpreter; do
    fixture="$temp_dir/rejected-elf-$elf_reference"
    printf '\177ELFfixture' > "$fixture"
    expect_status 1 env PATH="$temp_dir/tools:$PATH" \
        TEST_FORBIDDEN_TOKEN="$first_token" "$validator" --elf "$fixture"
done

# The outer completed-artifact filename is checked before extractor discovery,
# even when deterministic fake extractors would otherwise produce safe files.
for package_mode in deb rpm arch; do
    rejected_package="$temp_dir/prefix-${first_token}-suffix.$package_mode"
    [ "$package_mode" != arch ] || rejected_package="$rejected_package.pkg.tar.zst"
    touch "$rejected_package"
    expect_status 1 env PATH="$temp_dir/archive-tools:$PATH" \
        "$validator" "--$package_mode" "$rejected_package"
done

# A rejected ELF must clean its private inspection files even though the
# policy failure exits from nested dependency-token validation.
mkdir -p "$temp_dir/elf-cleanup-tmp"
expect_status 1 env TMPDIR="$temp_dir/elf-cleanup-tmp" \
    PATH="$temp_dir/tools:$PATH" TEST_FORBIDDEN_TOKEN="$first_token" \
    "$validator" --elf "$temp_dir/rejected-elf-filter"
if find "$temp_dir/elf-cleanup-tmp" -mindepth 1 -print -quit | grep -q .; then
    echo "ELF validation leaked private inspection files" >&2
    exit 1
fi

# Exercise each native archive boundary with deterministic fake package tools.
# The validator must reject both a declared dependency and a file introduced
# only while extracting the completed package payload.
mkdir -p "$temp_dir/archive-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'case "$1" in' \
    '  --control)' \
    '    destination=$3' \
    '    mkdir -p "$destination"' \
    '    printf "Package: balun\\nDepends: gstreamer1.0-plugins-good\\n" > "$destination/control"' \
    '    printf "#!/bin/sh\\n/sbin/ldconfig\\n" > "$destination/postinst"' \
    '    case "${TEST_ARCHIVE_MODE:-allowed}" in' \
    '      forbidden-dependency)' \
    '        printf "Recommends: %s-runtime\\n" "$TEST_FORBIDDEN_TOKEN" >> "$destination/control"' \
    '        ;;' \
    '      forbidden-script)' \
    '        printf "curl https://example.invalid/%s/install.sh\\n" "$TEST_FORBIDDEN_TOKEN" >> "$destination/postinst"' \
    '        ;;' \
    '      binary-control)' \
    '        printf "\\000\\001binary" > "$destination/blob"' \
    '        ;;' \
    '    esac' \
    '    ;;' \
    '  --extract)' \
    '    destination=$3' \
    '    mkdir -p "$destination/usr/lib"' \
    '    if [ "${TEST_ARCHIVE_MODE:-allowed}" = forbidden-payload ]; then' \
    '      touch "$destination/usr/lib/${TEST_FORBIDDEN_TOKEN}.so"' \
    '    else' \
    '      touch "$destination/usr/lib/libgstlibav.so"' \
    '    fi' \
    '    ;;' \
    '  *) exit 2 ;;' \
    'esac' \
    > "$temp_dir/archive-tools/dpkg-deb"
printf '%s\n' \
    '#!/bin/sh' \
    'case "$2:${TEST_ARCHIVE_MODE:-allowed}" in' \
    '  --recommends:forbidden-dependency)' \
    '    echo "${TEST_FORBIDDEN_TOKEN}-runtime"' \
    '    ;;' \
    '  --scripts:forbidden-script)' \
    '    echo "curl https://example.invalid/${TEST_FORBIDDEN_TOKEN}/install.sh"' \
    '    ;;' \
    '  --requires:*|--recommends:*|--suggests:*|--supplements:*|--enhances:*)' \
    '    echo gstreamer1-plugins-good' \
    '    ;;' \
    '  *) echo none ;;' \
    'esac' \
    > "$temp_dir/archive-tools/rpm"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$temp_dir/archive-tools/rpm2cpio"
printf '%s\n' \
    '#!/bin/sh' \
    'mkdir -p usr/lib' \
    'if [ "${TEST_ARCHIVE_MODE:-allowed}" = forbidden-payload ]; then' \
    '  touch "usr/lib/${TEST_FORBIDDEN_TOKEN}.so"' \
    'else' \
    '  touch usr/lib/libgstlibav.so' \
    'fi' \
    > "$temp_dir/archive-tools/cpio"
printf '%s\n' \
    '#!/bin/sh' \
    'case "$1" in' \
    '  -xOf)' \
    '    if [ "${TEST_ARCHIVE_MODE:-allowed}" = forbidden-dependency ]; then' \
    '      echo "depend = ${TEST_FORBIDDEN_TOKEN}-runtime"' \
    '    else' \
    '      echo "depend = gst-plugins-good"' \
    '    fi' \
    '    ;;' \
    '  -xf)' \
    '    destination=$4' \
    '    mkdir -p "$destination/usr/lib"' \
    '    if [ "${TEST_ARCHIVE_MODE:-allowed}" = forbidden-payload ]; then' \
    '      touch "$destination/usr/lib/${TEST_FORBIDDEN_TOKEN}.so"' \
    '    else' \
    '      touch "$destination/usr/lib/libgstlibav.so"' \
    '    fi' \
    '    if [ "${TEST_ARCHIVE_MODE:-allowed}" = forbidden-script ]; then' \
    '      printf "curl https://example.invalid/%s/install.sh\\n" "$TEST_FORBIDDEN_TOKEN" > "$destination/.INSTALL"' \
    '    fi' \
    '    ;;' \
    '  *) exit 2 ;;' \
    'esac' \
    > "$temp_dir/archive-tools/bsdtar"
chmod +x \
    "$temp_dir/archive-tools/dpkg-deb" \
    "$temp_dir/archive-tools/rpm" \
    "$temp_dir/archive-tools/rpm2cpio" \
    "$temp_dir/archive-tools/cpio" \
    "$temp_dir/archive-tools/bsdtar"
touch "$temp_dir/fixture.deb" "$temp_dir/fixture.rpm" "$temp_dir/fixture.pkg.tar.zst"
for package_mode in deb rpm arch; do
    package="$temp_dir/fixture.$package_mode"
    [ "$package_mode" != arch ] || package="$temp_dir/fixture.pkg.tar.zst"
    PATH="$temp_dir/archive-tools:$PATH" TEST_FORBIDDEN_TOKEN="$first_token" \
        "$validator" "--$package_mode" "$package"
    archive_modes="forbidden-dependency forbidden-payload"
    case "$package_mode" in
        deb) archive_modes="$archive_modes forbidden-script binary-control" ;;
        rpm) archive_modes="$archive_modes forbidden-script" ;;
        arch) archive_modes="$archive_modes forbidden-script" ;;
    esac
    for archive_mode in $archive_modes; do
        expect_status 1 env PATH="$temp_dir/archive-tools:$PATH" \
            TEST_FORBIDDEN_TOKEN="$first_token" TEST_ARCHIVE_MODE="$archive_mode" \
            "$validator" "--$package_mode" "$package"
    done
done

# Exercise the complete Flatpak app-commit boundary without requiring
# Flatpak/OSTree on the unit-test host. The production workflow still imports
# and checks out the real completed bundle with those tools before upload.
mkdir -p "$temp_dir/flatpak-tools"
printf '%s\n' \
    '#!/bin/sh' \
    'exit 0' \
    > "$temp_dir/flatpak-tools/flatpak"
printf '%s\n' \
    '#!/bin/sh' \
    'operation=' \
    'last=' \
    'for argument in "$@"; do' \
    '  last=$argument' \
    '  case "$argument" in init|refs|checkout) operation=$argument ;; esac' \
    'done' \
    'case "$operation" in' \
    '  init) exit 0 ;;' \
    '  refs)' \
    '    if [ "${TEST_BUNDLE_MODE:-allowed}" = unexpected-ref ]; then' \
    '      echo runtime/org.example.Unexpected/x86_64/master' \
    '    else' \
    '      echo app/io.github.jm2.Balun/x86_64/master' \
    '    fi' \
    '    ;;' \
    '  checkout)' \
    '    mkdir -p "$last/files" "$last/export/share/applications"' \
    '    touch "$last/files/libgstlibav.so"' \
    '    touch "$last/export/share/applications/io.github.jm2.Balun.desktop"' \
    '    printf "[Application]\\nruntime=org.gnome.Platform/x86_64/49\\n" > "$last/metadata"' \
    '    case "${TEST_BUNDLE_MODE:-allowed}" in' \
    '      forbidden-export)' \
    '        touch "$last/export/share/applications/${TEST_FORBIDDEN_TOKEN}.desktop"' \
    '        ;;' \
    '      forbidden-metadata)' \
    '        printf "extension=%s-runtime\\n" "$TEST_FORBIDDEN_TOKEN" >> "$last/metadata"' \
    '        ;;' \
    '    esac' \
    '    ;;' \
    '  *) exit 2 ;;' \
    'esac' \
    > "$temp_dir/flatpak-tools/ostree"
chmod +x "$temp_dir/flatpak-tools/flatpak" "$temp_dir/flatpak-tools/ostree"
touch "$temp_dir/balun.flatpak"
ln -s "$temp_dir/balun.flatpak" "$temp_dir/balun-link.flatpak"
expect_status 2 \
    "$repository_root/build-aux/flatpak/validate-bundle-compliance.sh" \
    "$temp_dir/balun-link.flatpak"
PATH="$temp_dir/flatpak-tools:$PATH" \
    "$repository_root/build-aux/flatpak/validate-bundle-compliance.sh" \
    "$temp_dir/balun.flatpak" > /dev/null
for bundle_mode in forbidden-export forbidden-metadata; do
    expect_status 1 env PATH="$temp_dir/flatpak-tools:$PATH" \
        TEST_BUNDLE_MODE="$bundle_mode" TEST_FORBIDDEN_TOKEN="$first_token" \
        "$repository_root/build-aux/flatpak/validate-bundle-compliance.sh" \
        "$temp_dir/balun.flatpak"
done
expect_status 1 env PATH="$temp_dir/flatpak-tools:$PATH" \
    TEST_BUNDLE_MODE=unexpected-ref \
    "$repository_root/build-aux/flatpak/validate-bundle-compliance.sh" \
    "$temp_dir/balun.flatpak"

# Missing, partial, malformed, and mutated shared policy data are all setup
# failures, never an implicit allow-all fallback. Each fixture is derived at
# runtime so this test never duplicates denied component names.
for fixture in missing empty comments binary truncated appended malformed duplicate; do
    mkdir -p "$temp_dir/$fixture/build-aux/linux" "$temp_dir/$fixture/build-aux/packaging"
    cp "$validator" "$temp_dir/$fixture/build-aux/linux/validate-package-compliance.sh"
done
rm -f "$temp_dir/missing/build-aux/packaging/forbidden-bundled-components.txt"
: > "$temp_dir/empty/build-aux/packaging/forbidden-bundled-components.txt"
printf '# no active entries\n' \
    > "$temp_dir/comments/build-aux/packaging/forbidden-bundled-components.txt"
printf '\000binary\n' \
    > "$temp_dir/binary/build-aux/packaging/forbidden-bundled-components.txt"
sed '$d' "$policy" \
    > "$temp_dir/truncated/build-aux/packaging/forbidden-bundled-components.txt"
cp "$policy" "$temp_dir/appended/build-aux/packaging/forbidden-bundled-components.txt"
printf '# unreviewed mutation\n' \
    >> "$temp_dir/appended/build-aux/packaging/forbidden-bundled-components.txt"
printf 'valid-token\nbad token\n' \
    > "$temp_dir/malformed/build-aux/packaging/forbidden-bundled-components.txt"
printf 'duplicate\nDUPLICATE\n' \
    > "$temp_dir/duplicate/build-aux/packaging/forbidden-bundled-components.txt"
for fixture in missing empty comments binary truncated appended malformed duplicate; do
    expect_status 2 "$temp_dir/$fixture/build-aux/linux/validate-package-compliance.sh" \
        --tree "$temp_dir/allowed"
done

# Keep the distribution dependency closure and package build contract visible
# to this existing policy suite without adding a second metadata parser.
manifest="$repository_root/Cargo.toml"
pkgbuild="$repository_root/build-aux/arch/PKGBUILD"
metadata_validator="$script_dir/validate-package-metadata.sh"
require_literal "$manifest" '[package.metadata.deb]'
require_literal "$manifest" 'features = ["desktop"]'
require_literal "$manifest" 'libgtk-4-1 (>= 4.16)'
require_literal "$manifest" 'gstreamer1.0-gtk4'
require_literal "$manifest" 'gstreamer1.0-libav'
require_literal "$manifest" '[package.metadata.generate-rpm.requires]'
require_literal "$manifest" 'gtk4 = ">= 4.16"'
require_literal "$manifest" 'gstreamer1-plugin-gtk4 = "*"'
require_literal "$manifest" 'gstreamer1-plugin-libav = "*"'
require_literal "$pkgbuild" "arch=('x86_64')"
require_literal "$pkgbuild" "options=('!lto' '!debug')"
require_literal "$pkgbuild" "checkdepends=('perl')"
require_literal "$pkgbuild" "'gtk4>=4.16'"
require_literal "$pkgbuild" "'gst-plugins-base-libs'"
require_literal "$pkgbuild" "'gst-plugins-bad-libs'"
require_literal "$pkgbuild" "'gst-plugin-gtk4'"
require_literal "$pkgbuild" "'gst-libav'"
require_literal "$pkgbuild" 'cargo build --frozen --release --features desktop --bin balun'
require_literal "$pkgbuild" 'validate-package-compliance.sh --elf target/release/balun'
require_literal "$pkgbuild" 'validate-package-compliance.sh --tree "$pkgdir"'
require_literal "$metadata_validator" 'build-aux/arch/PKGBUILD'

"$script_dir/validate-package-metadata.sh" > /dev/null

echo "Linux package compliance positive and negative tests passed"
