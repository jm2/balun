#!/bin/sh
# Install the completed bundle into a throwaway user installation and ask the
# packaged runtime for every structural playback factory Balun checks at
# startup. The manifest's build-time probe sees the org.gnome.Sdk registry the
# build ran in; this probe sees org.gnome.Platform exactly as the installed app
# does, so a factory the SDK carries but the Platform lacks fails here instead
# of at first launch. The codec extension is not needed for these factories and
# is left out (--no-related) so the probe fetches nothing beyond the runtime.

set -eu
set -f

app_id=io.github.jm2.Balun
factories="playbin3 uridecodebin3 decodebin3 appsrc tsdemux deinterlace gtk4paintablesink"
bundle=${1:-}

if [ -z "$bundle" ] || [ "$#" -ne 1 ]; then
    echo "Usage: validate-bundle-runtime.sh FILE.flatpak" >&2
    exit 2
fi
if [ ! -f "$bundle" ] || [ -L "$bundle" ]; then
    echo "Flatpak bundle is missing or is not a regular file: $bundle" >&2
    exit 2
fi
command -v flatpak >/dev/null 2>&1 || {
    echo "Flatpak runtime probe requires 'flatpak'" >&2
    exit 2
}

case "$bundle" in
    /*) ;;
    *) bundle="$(pwd -P)/$bundle" ;;
esac

temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
mkdir -p "$temp_dir/home" "$temp_dir/installation"
# Keep the runner's own user installation and app data untouched.
HOME="$temp_dir/home"
FLATPAK_USER_DIR="$temp_dir/installation"
export HOME FLATPAK_USER_DIR

flatpak --user install --noninteractive --no-related "$bundle"

for factory in $factories; do
    flatpak --user run --command=gst-inspect-1.0 "$app_id" --exists "$factory" || {
        echo "packaged runtime lacks GStreamer factory $factory" >&2
        exit 1
    }
done

echo "Packaged runtime supplies every structural playback factory: $bundle"
