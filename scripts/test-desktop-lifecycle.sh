#!/usr/bin/env bash
# Run Balun's real GTK/libadwaita window lifecycle under an isolated headless
# X server and session bus. The Rust smoke closes the window through its normal
# close-request path, requires a successful controller join, and proves that
# activation and shutdown never invoke local discovery.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
temporary_root=$(mktemp -d)
xvfb_pid=

cleanup()
{
    if [ -n "$xvfb_pid" ] && kill -0 "$xvfb_pid" 2>/dev/null; then
        kill "$xvfb_pid" 2>/dev/null || true
        wait "$xvfb_pid" 2>/dev/null || true
    fi
    rm -rf -- "$temporary_root"
}
trap cleanup EXIT HUP INT TERM

fail()
{
    printf '[balun] desktop lifecycle smoke failed: %s\n' "$*" >&2
    exit 1
}

require_command()
{
    command -v "$1" >/dev/null 2>&1 || \
        fail "required command '$1' is unavailable"
}

require_command cargo
require_command dbus-run-session
require_command timeout
require_command Xvfb

display_number_file="$temporary_root/display-number"
xvfb_log="$temporary_root/xvfb.log"

# -displayfd asks Xvfb to choose an unused display rather than racing another
# job over a hard-coded :99. TCP is disabled because this smoke only needs the
# inherited local X11 socket.
Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp -noreset \
    3>"$display_number_file" >"$xvfb_log" 2>&1 &
xvfb_pid=$!

for _ in {1..100}; do
    [ -s "$display_number_file" ] && break
    kill -0 "$xvfb_pid" 2>/dev/null || {
        sed -n '1,120p' "$xvfb_log" >&2
        fail "Xvfb exited before publishing its display number"
    }
    sleep 0.05
done

display_number=$(tr -d '[:space:]' < "$display_number_file")
case "$display_number" in
    ''|*[!0-9]*)
        sed -n '1,120p' "$xvfb_log" >&2
        fail "Xvfb did not publish a numeric display number"
        ;;
esac

runtime_dir="$temporary_root/runtime"
mkdir -p \
    "$runtime_dir" \
    "$temporary_root/cache" \
    "$temporary_root/config" \
    "$temporary_root/data" \
    "$temporary_root/state"
chmod 700 "$runtime_dir"

cd "$repository_root"
export DISPLAY=":$display_number"
export GDK_BACKEND=x11
export GSK_RENDERER=cairo
export GSETTINGS_BACKEND=memory
export G_DEBUG=fatal-criticals
export LIBGL_ALWAYS_SOFTWARE=1
export XDG_CACHE_HOME="$temporary_root/cache"
export XDG_CONFIG_HOME="$temporary_root/config"
export XDG_DATA_HOME="$temporary_root/data"
export XDG_RUNTIME_DIR="$runtime_dir"
export XDG_STATE_HOME="$temporary_root/state"

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --bin balun \
        app::tests::headless_window_close_joins_controller_without_discovery -- \
        --exact --ignored --nocapture

printf '[balun] desktop lifecycle smoke passed\n'
