#!/usr/bin/env bash
# Run Balun's real GTK/libadwaita window and synthetic playback lifecycles under
# an isolated display compositor. Headless Wayland is preferred and is the CI
# route; Xvfb remains an optional local fallback. Each Rust smoke gets a
# separate session bus and bounded process: the first proves clean application
# shutdown without discovery, the second proves an active production session's
# URI-opaque paintable and shutdown, the third proves PlayerView binding, and
# the fourth proves that a checked-in MPEG-2 transport stream reaches EOS after
# rendering through gtk4paintablesink.

set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd -P)
temporary_root=$(mktemp -d)
compositor_pid=

stop_compositor()
{
    local kill_watchdog_pid=

    if [ -z "$compositor_pid" ]; then
        return
    fi

    if kill -0 "$compositor_pid" 2>/dev/null; then
        kill "$compositor_pid" 2>/dev/null || true
        (
            sleep 2
            kill -KILL "$compositor_pid" 2>/dev/null || true
        ) &
        kill_watchdog_pid=$!
    fi

    wait "$compositor_pid" 2>/dev/null || true
    compositor_pid=

    if [ -n "$kill_watchdog_pid" ]; then
        kill "$kill_watchdog_pid" 2>/dev/null || true
        wait "$kill_watchdog_pid" 2>/dev/null || true
    fi
}

cleanup()
{
    stop_compositor
    rm -rf -- "$temporary_root"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail()
{
    printf '[balun] desktop and synthetic playback lifecycle smoke failed: %s\n' "$*" >&2
    exit 1
}

require_command()
{
    command -v "$1" >/dev/null 2>&1 || \
        fail "required command '$1' is unavailable"
}

supports_headless_wayland()
{
    local weston_help

    command -v weston >/dev/null 2>&1 || return 1
    command -v wayland-info >/dev/null 2>&1 || return 1
    weston_help=$(weston --help 2>&1) || return 1

    case "$weston_help" in
        *--fake-seat*) return 0 ;;
        *) return 1 ;;
    esac
}

print_bounded_log()
{
    local log_path=$1

    if [ -f "$log_path" ]; then
        sed -n '1,120p' "$log_path" >&2
    fi
}

start_wayland()
{
    local wayland_display=balun-wayland-0
    local wayland_log="$temporary_root/weston.log"
    local wayland_probe_log="$temporary_root/wayland-info.log"

    unset DISPLAY WAYLAND_DEBUG WAYLAND_DISPLAY WAYLAND_SOCKET XAUTHORITY

    weston \
        --backend=headless \
        --renderer=pixman \
        --shell=kiosk-shell.so \
        --socket="$wayland_display" \
        --width=1280 \
        --height=800 \
        --idle-time=0 \
        --fake-seat \
        --no-config \
        >"$wayland_log" 2>&1 &
    compositor_pid=$!

    for _ in {1..100}; do
        [ -S "$XDG_RUNTIME_DIR/$wayland_display" ] && break
        if ! kill -0 "$compositor_pid" 2>/dev/null; then
            print_bounded_log "$wayland_log"
            return 1
        fi
        sleep 0.05
    done

    if [ ! -S "$XDG_RUNTIME_DIR/$wayland_display" ]; then
        print_bounded_log "$wayland_log"
        return 1
    fi

    if ! timeout --signal=TERM --kill-after=1s 5s \
        env WAYLAND_DISPLAY="$wayland_display" wayland-info \
        >"$wayland_probe_log" 2>&1; then
        print_bounded_log "$wayland_log"
        print_bounded_log "$wayland_probe_log"
        return 1
    fi

    export WAYLAND_DISPLAY="$wayland_display"
    export GDK_BACKEND=wayland
    export XDG_SESSION_TYPE=wayland
}

start_x11()
{
    local display_number
    local display_number_file="$temporary_root/display-number"
    local xvfb_log="$temporary_root/xvfb.log"

    unset WAYLAND_DEBUG WAYLAND_DISPLAY WAYLAND_SOCKET XAUTHORITY

    # -displayfd chooses an unused display without racing another job. TCP is
    # disabled because the fallback only needs its inherited local X11 socket.
    Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp -noreset \
        3>"$display_number_file" >"$xvfb_log" 2>&1 &
    compositor_pid=$!

    for _ in {1..100}; do
        [ -s "$display_number_file" ] && break
        kill -0 "$compositor_pid" 2>/dev/null || {
            print_bounded_log "$xvfb_log"
            fail "the fallback Xvfb server exited before publishing its display number"
        }
        sleep 0.05
    done

    display_number=$(tr -d '[:space:]' < "$display_number_file")
    case "$display_number" in
        ''|*[!0-9]*)
            print_bounded_log "$xvfb_log"
            fail "the fallback Xvfb server did not publish a numeric display number"
            ;;
    esac

    export DISPLAY=":$display_number"
    export GDK_BACKEND=x11
    export XDG_SESSION_TYPE=x11
}

require_command cargo
require_command dbus-run-session
require_command timeout

runtime_dir="$temporary_root/runtime"
mkdir -p \
    "$runtime_dir" \
    "$temporary_root/cache" \
    "$temporary_root/config" \
    "$temporary_root/data" \
    "$temporary_root/state"
chmod 700 "$runtime_dir"

cd "$repository_root"
export GSK_RENDERER=cairo
export GSETTINGS_BACKEND=memory
export G_DEBUG=fatal-criticals
export LIBGL_ALWAYS_SOFTWARE=1
export XDG_CACHE_HOME="$temporary_root/cache"
export XDG_CONFIG_HOME="$temporary_root/config"
export XDG_DATA_HOME="$temporary_root/data"
export XDG_RUNTIME_DIR="$runtime_dir"
export XDG_STATE_HOME="$temporary_root/state"

unset GDK_DEBUG GSK_DEBUG GTK_DEBUG

requested_backend=${BALUN_DESKTOP_TEST_BACKEND:-auto}
case "$requested_backend" in
    auto)
        if supports_headless_wayland; then
            selected_backend=wayland
        elif command -v Xvfb >/dev/null 2>&1; then
            selected_backend=x11
        else
            fail "neither supported headless Weston nor the optional Xvfb fallback is available"
        fi
        ;;
    wayland)
        supports_headless_wayland || \
            fail "headless Weston with wayland-info and fake-seat support is unavailable"
        selected_backend=wayland
        ;;
    x11)
        require_command Xvfb
        selected_backend=x11
        ;;
    *)
        fail "BALUN_DESKTOP_TEST_BACKEND must be auto, wayland, or x11"
        ;;
esac

case "$selected_backend" in
    wayland)
        if ! start_wayland; then
            stop_compositor
            if [ "$requested_backend" = auto ] && \
                command -v Xvfb >/dev/null 2>&1; then
                printf '%s\n' \
                    '[balun] headless Wayland unavailable; using the optional Xvfb fallback' \
                    >&2
                selected_backend=x11
                start_x11
            else
                fail "the required headless Wayland compositor failed its readiness check"
            fi
        fi
        ;;
    x11) start_x11 ;;
esac

# Keep caller configuration from redirecting plugin discovery, reusing an
# ambient registry, opening a separate gtk4paintablesink window, or making the
# bounded smoke emit unreviewed GStreamer debug and tracer output.
unset \
    GST_DEBUG \
    GST_DEBUG_COLOR_MODE \
    GST_DEBUG_DUMP_DOT_DIR \
    GST_DEBUG_FILE \
    GST_DEBUG_NO_COLOR \
    GST_DEBUG_OPTIONS \
    GST_GTK4_WINDOW \
    GST_GTK4_WINDOW_FULLSCREEN \
    GST_PLUGIN_FEATURE_RANK \
    GST_PLUGIN_LOADING_WHITELIST \
    GST_PLUGIN_PATH \
    GST_PLUGIN_PATH_1_0 \
    GST_PLUGIN_SCANNER \
    GST_PLUGIN_SCANNER_1_0 \
    GST_PLUGIN_SYSTEM_PATH \
    GST_PLUGIN_SYSTEM_PATH_1_0 \
    GST_REGISTRY \
    GST_REGISTRY_1_0 \
    GST_REGISTRY_DISABLE \
    GST_REGISTRY_FORK \
    GST_REGISTRY_MODE \
    GST_REGISTRY_REUSE_PLUGIN_SCANNER \
    GST_REGISTRY_UPDATE \
    GST_TRACE \
    GST_TRACERS

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --bin balun \
        app::tests::headless_window_close_joins_controller_without_discovery -- \
        --exact --ignored --nocapture

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --lib \
        playback::session::tests::active_production_session_exposes_opaque_paintable_and_shuts_down -- \
        --exact --ignored --nocapture

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --bin balun \
        ui::player_view::tests::opaque_paintable_binding_tracks_status_and_shutdown -- \
        --exact --ignored --nocapture

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --bin balun \
        ui::player_view::tests::accessible_audio_controls_update_the_session -- \
        --exact --ignored --nocapture

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --bin balun \
        ui::channel_sidebar::tests::ready_listview_activation_is_inert_on_selection_and_exact_on_activate -- \
        --exact --ignored --nocapture

# A bare Xvfb server has no window manager to acknowledge fullscreen state.
# Keep the compositor-confirmed round trip on the preferred/required Wayland
# route, while the pure key and reducer contracts still run on every platform.
if [ "$selected_backend" = wayland ]; then
    dbus-run-session -- \
        timeout --signal=TERM --kill-after=5s 30s \
        cargo test --locked --features desktop --bin balun \
            ui::window::tests::wayland_fullscreen_round_trip_protects_and_restores_navigation -- \
            --exact --ignored --nocapture
fi

dbus-run-session -- \
    timeout --signal=TERM --kill-after=5s 30s \
    cargo test --locked --features desktop --test playback_synthetic \
        synthetic_mpeg2_reaches_eos_and_renders_multiple_frames -- \
        --exact --ignored --nocapture --test-threads=1

printf '[balun] desktop and synthetic playback lifecycle smoke passed (%s)\n' \
    "$selected_backend"
