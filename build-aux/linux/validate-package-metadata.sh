#!/bin/sh
# Pin every Linux input that can add payload files or direct package
# dependencies. Ordinary distribution GStreamer packages remain external.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
validator="$script_dir/validate-package-compliance.sh"

"$validator" --metadata \
    "$repository_root/Cargo.toml" \
    "$repository_root/Cargo.lock" \
    "$repository_root/build-aux/flatpak/io.github.jm2.Balun.yml" \
    "$repository_root/build-aux/arch/PKGBUILD"

echo "Linux packaging inputs comply with the reviewed component policy"
