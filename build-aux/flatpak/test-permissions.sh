#!/bin/sh
# Exercise Balun's preparatory Flatpak permission contract against a canonical
# synthetic manifest. No production Flatpak manifest or packaging claim is
# implied by this fixture.

set -eu
set -f

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
validator="$script_dir/validate-permissions.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
manifest="$temp_dir/io.github.jm2.Balun.synthetic.yml"

printf '%s\n' \
    'app-id: io.github.jm2.Balun' \
    'runtime: org.gnome.Platform' \
    'runtime-version: "50"' \
    'sdk: org.gnome.Sdk' \
    'command: balun' \
    'finish-args:' \
    '  # Wayland with a reviewed X11 fallback' \
    '  - --socket=wayland' \
    '  - --socket=fallback-x11' \
    '  - --share=ipc' \
    '  # PulseAudio playback plus its input/MIDI/ALSA boundary' \
    '  - --socket=pulseaudio' \
    '  # Network discovery/control/streaming' \
    '  - --share=network' \
    '  # Standard GPU/DRI grant for practical UHD decode/presentation' \
    '  - --device=dri' \
    '' \
    'modules:' \
    '  - name: balun' \
    '    buildsystem: simple' > "$manifest"

fail()
{
    echo "Flatpak permission test failure: $*" >&2
    exit 1
}

reject_fixture()
{
    name=$1
    fixture=$2

    if "$validator" "$fixture" >/dev/null 2>&1; then
        fail "negative fixture unexpectedly passed: $name"
    fi
}

insert_and_reject()
{
    name=$1
    entry=$2
    fixture="$temp_dir/$name.yml"

    awk -v entry="$entry" '
        { print }
        $0 == "  - --device=dri" {
            print entry
            inserted = 1
        }
        END { if (!inserted) exit 2 }
    ' "$manifest" > "$fixture"
    reject_fixture "$name" "$fixture"
}

remove_and_reject()
{
    name=$1
    entry=$2
    fixture="$temp_dir/$name.yml"

    awk -v entry="$entry" '
        $0 == entry {
            removed++
            next
        }
        { print }
        END { if (removed != 1) exit 2 }
    ' "$manifest" > "$fixture"
    reject_fixture "$name" "$fixture"
}

duplicate_and_reject()
{
    name=$1
    entry=$2
    fixture="$temp_dir/$name.yml"

    awk -v entry="$entry" '
        { print }
        $0 == entry {
            print
            duplicated++
        }
        END { if (duplicated != 1) exit 2 }
    ' "$manifest" > "$fixture"
    reject_fixture "$name" "$fixture"
}

replace_line_and_reject()
{
    name=$1
    old_line=$2
    new_line=$3
    fixture="$temp_dir/$name.yml"

    awk -v old_line="$old_line" -v new_line="$new_line" '
        $0 == old_line {
            print new_line
            replaced++
            next
        }
        { print }
        END { if (replaced != 1) exit 2 }
    ' "$manifest" > "$fixture"
    reject_fixture "$name" "$fixture"
}

append_and_reject()
{
    name=$1
    line=$2
    value_indicator=${3-}
    fixture="$temp_dir/$name.yml"

    cp "$manifest" "$fixture"
    printf '\n%s\n' "$line" >> "$fixture"
    if [ -n "$value_indicator" ]; then
        printf '%s\n' "$value_indicator" >> "$fixture"
    fi
    printf '  - --filesystem=host:rw\n' >> "$fixture"
    reject_fixture "$name" "$fixture"
}

"$validator" "$manifest" >/dev/null

if "$validator" >/dev/null 2>&1; then
    fail "validator accepted an omitted explicit manifest argument"
fi
if "$validator" "$temp_dir/missing.yml" >/dev/null 2>&1; then
    fail "validator accepted a missing explicit manifest"
fi

# A manifest basename of "-" must be read as the named regular file, never as
# awk's standard-input sentinel. Feed the canonical manifest on stdin while the
# named file contains an otherwise complete but overprivileged policy; the
# validator must reject the named file.
dash_name_dir="$temp_dir/dash-name"
mkdir "$dash_name_dir"
awk '
    { print }
    $0 == "  - --device=dri" {
        print "  - --filesystem=host:rw"
        inserted++
    }
    END { if (inserted != 1) exit 2 }
' "$manifest" > "$dash_name_dir/-"
reject_fixture dash-name-direct "$dash_name_dir/-"
if (
    cd "$dash_name_dir"
    "$validator" - < "$manifest"
) >/dev/null 2>&1; then
    fail "validator substituted stdin for a manifest named '-'"
fi

for permission in \
    '--socket=wayland' \
    '--socket=fallback-x11' \
    '--share=ipc' \
    '--socket=pulseaudio' \
    '--share=network' \
    '--device=dri'
do
    suffix=$(printf '%s\n' "$permission" | tr '=/' '--' | sed 's/^--//')
    remove_and_reject "missing-$suffix" "  - $permission"
    duplicate_and_reject "duplicate-$suffix" "  - $permission"
done

# Reject broad and desktop-integration grants that Balun deliberately does not
# inherit from Tributary and must not acquire from a future packaging template.
insert_and_reject host-filesystem '  - --filesystem=host:rw'
insert_and_reject home-filesystem '  - --filesystem=home:ro'
insert_and_reject music-filesystem '  - --filesystem=xdg-music:rw'
insert_and_reject media-filesystem '  - --filesystem=/run/media:ro'
insert_and_reject gvfs-bus '  - --talk-name=org.gtk.vfs.*'
insert_and_reject secrets-bus '  - --talk-name=org.freedesktop.secrets'
insert_and_reject mpris-name '  - --own-name=org.mpris.MediaPlayer2.balun'
insert_and_reject theme-filesystem '  - --filesystem=xdg-data/themes:ro'
insert_and_reject icon-filesystem '  - --filesystem=xdg-data/icons:ro'
insert_and_reject raw-devices '  - --device=all'
insert_and_reject system-bus '  - --system-talk-name=org.freedesktop.UDisks2'
insert_and_reject session-bus '  - --talk-name=org.example.Unreviewed'
insert_and_reject broad-session-bus '  - --talk-name=org.freedesktop.*'

# Reject alternate YAML spellings that a text-only allowlist could otherwise
# fail to interpret the same way as a YAML parser.
insert_and_reject quoted-entry '  - "--filesystem=host:rw"'
insert_and_reject single-quoted-entry "  - '--filesystem=host:rw'"
insert_and_reject inline-comment '  - --filesystem=host:rw # hidden grant'
insert_and_reject alias-entry '  - *hidden_permissions'
insert_and_reject tagged-entry '  - !!str --filesystem=host:rw'
replace_line_and_reject inline-list 'finish-args:' \
    'finish-args: [--socket=wayland, --share=network]'
replace_line_and_reject anchored-block 'finish-args:' \
    'finish-args: &hidden_permissions'
replace_line_and_reject spaced-key 'finish-args:' 'finish-args :'
append_and_reject duplicate-block 'finish-args:'
append_and_reject quoted-key '"finish-args":'
append_and_reject escaped-key '"finish\u002dargs":'
append_and_reject explicit-key '? finish-args' ':'
append_and_reject merge-key '<<: *hidden_permissions'

echo "Flatpak permission policy positive and negative tests passed"
