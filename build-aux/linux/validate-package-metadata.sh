#!/bin/sh
# Pin the Linux dependency inputs that exist in the current Balun tree.
# Package recipes and manifests are added here atomically when they land.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)
validator="$script_dir/validate-package-compliance.sh"

"$validator" --metadata \
    "$repository_root/Cargo.toml" \
    "$repository_root/Cargo.lock" \
    "$repository_root/build-aux/flatpak/io.github.jm2.Balun.yml"

echo "Current Linux packaging inputs comply with the reviewed component policy"
