#!/bin/sh
# Validate Balun's reviewed Flatpak finish-args policy. This checks the exact
# permission contract, not general YAML syntax. There is intentionally no
# default manifest until Balun has a real, reviewed Flatpak recipe.

set -eu
set -f

if [ "$#" -ne 1 ]; then
    echo "Usage: validate-permissions.sh MANIFEST.yml" >&2
    exit 2
fi

manifest=$1
if [ ! -f "$manifest" ]; then
    echo "Flatpak manifest not found: $manifest" >&2
    exit 2
fi

fail()
{
    echo "Flatpak permission policy violation: $*" >&2
    exit 1
}

# Accept only one canonical, block-style finish-args key and canonical,
# unquoted, one-argument list entries beneath it. Quoted keys or values,
# inline collections, YAML aliases/tags/merges, continuations, and inline
# comments therefore cannot hide a second permission interpretation from this
# deliberately small policy parser.
finish_args=$(awk '
    function reject(message) {
        print "Flatpak permission policy violation: " message > "/dev/stderr"
        rejected = 1
        exit 2
    }

    /^finish-args:$/ {
        finish_args_blocks++
        if (finish_args_blocks != 1) {
            reject("duplicate finish-args block")
        }
        found_finish_args = 1
        in_finish_args = 1
        next
    }

    in_finish_args && /^[^[:space:]#]/ {
        in_finish_args = 0
    }

    in_finish_args && /^[[:space:]]*#/ { next }
    in_finish_args && /^[[:space:]]*$/ { next }
    in_finish_args {
        if ($0 !~ /^  - --[^[:space:]#]+$/) {
            reject("noncanonical finish-args entry: " $0)
        }
        sub(/^  - /, "")
        print
        next
    }

    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }

    /^[^[:space:]]/ {
        if (index($0, "finish-args") == 1) {
            reject("noncanonical finish-args key: " $0)
        }
        if ($0 !~ /^[a-z][a-z0-9-]*:/) {
            reject("noncanonical top-level YAML form: " $0)
        }
    }

    END {
        if (rejected) {
            exit 2
        }
        if (!found_finish_args || finish_args_blocks != 1) {
            print "Flatpak permission policy violation: missing canonical finish-args block" > "/dev/stderr"
            exit 2
        }
    }
' < "$manifest")

require_once()
{
    count=$(printf '%s\n' "$finish_args" | grep -Fxc -- "$1" || true)
    [ "$count" -eq 1 ] || fail "expected exactly one '$1' entry (found $count)"
}

# Window-system access. IPC is retained solely for the reviewed X11 fallback.
require_once "--socket=wayland"
require_once "--socket=fallback-x11"
require_once "--share=ipc"

# Live television needs audio playback. Flatpak's standard PulseAudio grant is
# not output-only: it also exposes microphone/input, MIDI, and ALSA sound-device
# access. Keep that accepted boundary explicit when reviewing future runtimes.
require_once "--socket=pulseaudio"

# HDHomeRun discovery, control, and stream traffic require network access.
require_once "--share=network"

# Practical UHD playback needs Flatpak's standard GPU/DRI grant for hardware
# decoding and presentation. Depending on Flatpak and the host driver, this is
# broader than render nodes alone, but remains substantially narrower than
# --device=all.
require_once "--device=dri"

# Every finish argument must be reviewed here. In particular, Balun has no
# direct host/media filesystem, GVfs, Secret Service, MPRIS, theme/icon, raw
# camera/input device, --device=all, or broad session/system bus grant in the
# v0.1 policy.
entry_count=0
for entry in $finish_args; do
    case "$entry" in
        "--socket=wayland" | \
        "--socket=fallback-x11" | \
        "--share=ipc" | \
        "--socket=pulseaudio" | \
        "--share=network" | \
        "--device=dri")
            entry_count=$((entry_count + 1))
            ;;
        *)
            fail "unreviewed finish argument '$entry'"
            ;;
    esac
done

[ "$entry_count" -eq 6 ] || fail "expected exactly 6 reviewed finish arguments (found $entry_count)"

echo "Flatpak permission policy is valid: $manifest"
