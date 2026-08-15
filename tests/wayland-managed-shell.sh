#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-managed-shell.sh /path/to/nobox /path/to/nobox-wayland /path/to/probe}
wayland_binary=${2:?missing nobox-wayland binary}
probe_binary=${3:?missing nobox-wayland-probe binary}

for dependency in cc xdpyinfo pgrep; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for nested Wayland shell tests"
        exit 77
    fi
done

if [[ -z ${NOBOX_XSERVER:-} ]]; then
    if command -v Xvfb >/dev/null 2>&1; then
        export NOBOX_XSERVER=xvfb
    elif command -v Xephyr >/dev/null 2>&1; then
        export NOBOX_XSERVER=xephyr
    else
        echo "SKIP: Xvfb or Xephyr is required for nested Wayland shell tests"
        exit 77
    fi
fi
if [[ ${NOBOX_XSERVER,,} == xnest ]]; then
    echo "SKIP: the forced GLES2 renderer proof requires EGL-capable Xvfb or Xephyr"
    exit 77
fi

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
mkdir -p "$test_dir/data/applications" "$test_dir/empty-data"
application_marker="$test_dir/application-launched"
application_helper="$test_dir/activation-launch"
cat >"$application_helper" <<EOF
#!/usr/bin/env bash
printf '%s\n%s\n' "\${XDG_ACTIVATION_TOKEN:-}" "\${WAYLAND_DISPLAY:-}" >"$application_marker"
EOF
chmod 700 "$application_helper"
cat >"$test_dir/data/applications/nobox-wayland-probe.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Nobox dynamic menu probe
Exec=$application_helper
StartupNotify=true
Categories=Utility;
EOF
export XDG_DATA_HOME="$test_dir/data"
export XDG_DATA_DIRS="$test_dir/empty-data"
xserver_pid=
wayland_pid=
unresponsive_pid=
session_client_pid=
gtk_pid=
selection_owner_pid=
selection_observer_pid=
cleanup() {
    if [[ -n "$selection_observer_pid" ]]; then kill "$selection_observer_pid" 2>/dev/null || true; fi
    if [[ -n "$selection_owner_pid" ]]; then kill "$selection_owner_pid" 2>/dev/null || true; fi
    if [[ -n "$gtk_pid" ]]; then kill "$gtk_pid" 2>/dev/null || true; fi
    if [[ -n "$session_client_pid" ]]; then kill "$session_client_pid" 2>/dev/null || true; fi
    if [[ -n "$unresponsive_pid" ]]; then kill "$unresponsive_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_pid" ]]; then kill "$wayland_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for Wayland lifecycle tests"
    exit 77
fi
if ! cc "$(dirname "$0")/x11-largest-window-pixel.c" \
    -o "$test_dir/x11-largest-window-pixel" -lX11; then
    echo "SKIP: X11 development libraries are required for Wayland lock-screen tests"
    exit 77
fi

display=
for number in $(seq 241 260); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
if [[ -z "$display" ]]; then
    echo "no unused nested X11 display found" >&2
    exit 1
fi

"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
if ! DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then
    echo "$nested_x_server did not become ready" >&2
    exit 1
fi

env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" doctor --backend wayland --nested-x11 --display "$display" \
    >"$test_dir/doctor.log"
grep -Fq '[ok] Wayland backend: Smithay 0.7.0 (managed nested shell)' "$test_dir/doctor.log"
grep -Fq '[ok] renderers: Smithay GLES2 with Pixman fallback' "$test_dir/doctor.log"
grep -Fq '[info] surface protocols: wp_viewporter v1; wp_fractional_scale_manager_v1 v1' \
    "$test_dir/doctor.log"
grep -Fq '[info] selection protocols: wl_data_device_manager v3; zwp_primary_selection_device_manager_v1 v1' \
    "$test_dir/doctor.log"
grep -Fq '[info] selection limits per client: 64 sources; 16 devices; 32 MIME types/source; 256 bytes/MIME type' \
    "$test_dir/doctor.log"
grep -Fq '[info] pointer protocols: zwp_relative_pointer_manager_v1; zwp_pointer_constraints_v1 v1; zwp_pointer_gestures_v1 v3; wp_cursor_shape_manager_v1 v2; 64 extension objects/client; 64 gesture objects/client; 64 cursor-shape devices/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] touch protocol: wl_touch via wl_seat v9; 16 touch devices/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] tablet protocol: zwp_tablet_manager_v2 v1; 16 tablet seats/client; 16 tablets/seat; 64 tools/seat' \
    "$test_dir/doctor.log"
grep -Fq '[info] text input protocols when [wayland].input_method is configured: zwp_text_input_manager_v3 v1; private zwp_input_method_manager_v2 v1; 32 text inputs/client; 1 input-method objects/authorized connection; 8 popups and 8 keyboard grabs/input method' \
    "$test_dir/doctor.log"
grep -Fq '[info] timing protocol: wp_presentation v2; 256 feedbacks/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] inhibition and idle protocols: zwp_keyboard_shortcuts_inhibit_manager_v1 v1 (64 inhibitors/client); zwp_idle_inhibit_manager_v1 v1 (64 inhibitors/client); ext_idle_notifier_v1 v2 (64 notifications/client)' \
    "$test_dir/doctor.log"
grep -Fq '[info] session lock protocol: ext_session_lock_manager_v1 v1; 8 locks/client; 16 lock surfaces/client' \
    "$test_dir/doctor.log"
grep -Fq 'ready: yes (managed nested-X11 Wayland shell)' "$test_dir/doctor.log"

cat >"$test_dir/keyboard-config.toml" <<'EOF'
[panel]
enabled = false

[menu]
max_rows = 4

[focus]
follow_mouse = true
prevent_focus_stealing = true

[keyboard]
inherit_defaults = false

[[keyboard.bindings]]
key = "W-r"
action = { type = "resize" }

[[keyboard.bindings]]
key = "W-m"
action = { type = "show_menu", menu = "command-test" }

[[keyboard.bindings]]
key = "W-a"
action = { type = "show_menu", menu = "application-test" }

[[keyboard.bindings]]
key = "A-h"
action = { type = "cycle_direction", direction = "left" }

[[keyboard.bindings]]
key = "A-l"
action = { type = "cycle_direction", direction = "right" }

[[keyboard.bindings]]
key = "A-j"
action = { type = "cycle_direction", direction = "down" }

[[keyboard.bindings]]
key = "A-k"
action = { type = "cycle_direction", direction = "up" }

[[menu.definitions]]
id = "command-test"
title = "Generated"
source = "command"
command = 'for item in One Two Three Four Five; do printf "[[entries]]\ntype = \"item\"\nlabel = \"$item\"\naction = { type = \"debug\", message = \"$item\" }\n"; done; printf "[[entries]]\ntype = \"item\"\nlabel = \"_Close\"\naction = { type = \"close\" }\n"'

[[menu.definitions]]
id = "application-test"
title = "Applications"
source = "applications"

[mouse]
inherit_defaults = false
drag_threshold = 4

[[mouse.bindings]]
context = "client"
button = "W-Left"
trigger = "drag"
action = { type = "resize", edge = "right" }

[[mouse.bindings]]
context = "root"
button = "Up"
trigger = "click"
action = { type = "next_workspace" }

[[mouse.bindings]]
context = "close"
button = "Left"
trigger = "click"
action = { type = "close" }

[[applications]]
match = { title = "nobox follow A" }
position = { x = 80, y = 80, force = true }

[[applications]]
match = { title = "nobox follow B" }
position = { x = 400, y = 200, force = true }
EOF
keyboard_log="$test_dir/keyboard-wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_STATE_FILE="$test_dir/keyboard-session.toml" \
    "$nobox_binary" --backend wayland --config "$test_dir/keyboard-config.toml" \
    run --nested-x11 --no-autostart >"$keyboard_log" 2>&1 &
wayland_pid=$!
keyboard_socket=
for _ in $(seq 1 100); do
    keyboard_socket=$(sed -n 's/^ready: //p' "$keyboard_log" 2>/dev/null | head -n 1)
    if [[ -n "$keyboard_socket" ]]; then break; fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ -z "$keyboard_socket" ]]; then
    echo "configured Wayland compositor did not become ready" >&2
    cat "$keyboard_log" >&2
    exit 1
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --keyboard-resize >"$test_dir/keyboard-resize"
grep -Fq 'keyboard-resize-ok' "$test_dir/keyboard-resize"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --decoration-close >"$test_dir/decoration-close"
grep -Fq 'decoration-close-ok' "$test_dir/decoration-close"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --mouse-resize >"$test_dir/mouse-resize"
grep -Fq 'mouse-resize-ok' "$test_dir/mouse-resize"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --directional-cycle >"$test_dir/directional-cycle"
grep -Fq 'directional-cycle-ok center=' "$test_dir/directional-cycle"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --command-menu >"$test_dir/command-menu"
grep -Fq 'command-menu-ok center=' "$test_dir/command-menu"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --application-menu >"$test_dir/application-menu"
grep -Fq 'application-menu-ok center=' "$test_dir/application-menu"
for _ in $(seq 1 50); do
    if [[ -e "$application_marker" ]]; then break; fi
    sleep 0.02
done
[[ -e "$application_marker" ]]
activation_token=$(sed -n '1p' "$application_marker")
[[ "$activation_token" =~ ^[[:alnum:]]{32}$ ]]
[[ $(sed -n '2p' "$application_marker") == "$keyboard_socket" ]]
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --follow-mouse >"$test_dir/follow-mouse"
grep -Fq 'follow-mouse-ok' "$test_dir/follow-mouse"
sed -i 's/prevent_focus_stealing = true/prevent_focus_stealing = false/' \
    "$test_dir/keyboard-config.toml"
kill -HUP "$wayland_pid"
sleep 0.1
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$keyboard_socket" \
    "$probe_binary" --activation-permissive >"$test_dir/activation-permissive"
grep -Fq 'activation-permissive-ok' "$test_dir/activation-permissive"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=

cat >"$test_dir/input-method-config.toml" <<EOF
[panel]
enabled = false

[wayland]
input_method = ["$probe_binary", "--input-method"]
EOF
input_method_log="$test_dir/input-method-wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --config "$test_dir/input-method-config.toml" \
    run --nested-x11 --no-autostart >"$input_method_log" 2>&1 &
wayland_pid=$!
input_method_socket=
for _ in $(seq 1 100); do
    input_method_socket=$(sed -n 's/^ready: //p' "$input_method_log" 2>/dev/null | head -n 1)
    if [[ -n "$input_method_socket" ]] && \
        grep -Fq 'input-method-ready' "$input_method_log" 2>/dev/null; then
        break
    fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ -z "$input_method_socket" ]] || \
    ! grep -Fq 'input-method-ready' "$input_method_log"; then
    echo "configured Wayland input method did not become ready" >&2
    cat "$input_method_log" >&2
    exit 1
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$input_method_socket" \
    "$probe_binary" >"$test_dir/input-method-globals"
grep -Fxq 'zwp_text_input_manager_v3 1' "$test_dir/input-method-globals"
if grep -Fq 'zwp_input_method_manager_v2' "$test_dir/input-method-globals"; then
    echo "ordinary client saw the privileged input-method global" >&2
    exit 1
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$input_method_socket" \
    "$probe_binary" --text-input-limit >"$test_dir/text-input-limit"
grep -Fq 'text-input-limit-ok' "$test_dir/text-input-limit"
if ! DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$input_method_socket" \
    "$probe_binary" --text-input >"$test_dir/text-input"; then
    cat "$input_method_log" >&2
    cat "$test_dir/text-input" >&2
    exit 1
fi
grep -Fq 'text-input-ok focus commit ime-death' "$test_dir/text-input"
grep -Fq 'input-method-commit-ok' "$input_method_log"
for _ in $(seq 1 50); do
    if ! pgrep -P "$wayland_pid" >/dev/null; then break; fi
    sleep 0.05
done
if pgrep -P "$wayland_pid" >/dev/null; then
    echo "dead Wayland input method was not reaped" >&2
    exit 1
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$input_method_socket" \
    "$probe_binary" --shell >"$test_dir/shell-after-input-method-death"
grep -Fq 'shell-ok configures=' "$test_dir/shell-after-input-method-death"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=

cat >"$test_dir/session-config.toml" <<EOF
[panel]
enabled = false

[workspaces]
names = ["one", "two"]

[keyboard]
inherit_defaults = false

[[keyboard.bindings]]
key = "W-r"
action = { type = "resize" }

[[keyboard.bindings]]
key = "W-Right"
action = { type = "move_to_next_workspace", follow = true }

[[keyboard.bindings]]
key = "W-F8"
action = { type = "restart" }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "restart", command = "/usr/bin/touch $test_dir/wayland-handoff" }

[[keyboard.bindings]]
key = "W-F10"
action = { type = "session_logout", prompt = false }
EOF
cat >"$test_dir/autostart" <<EOF
printf 'started\n' >>'$test_dir/wayland-autostart.log'
EOF

session_log="$test_dir/session-wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/session-config.toml" \
    NOBOX_STATE_FILE="$test_dir/wayland-session.toml" \
    "$nobox_binary" --backend wayland run --nested-x11 >"$session_log" 2>&1 &
wayland_pid=$!
session_socket=
for _ in $(seq 1 100); do
    session_socket=$(sed -n 's/^ready: //p' "$session_log" 2>/dev/null | head -n 1)
    if [[ -n "$session_socket" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$session_socket" ]]; then
    echo "Wayland session lifecycle compositor did not become ready" >&2
    cat "$session_log" >&2
    exit 1
fi

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$session_socket" \
    "$probe_binary" --session-client >"$test_dir/session-client-first" 2>&1 &
session_client_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'session-client-ready' "$test_dir/session-client-first" 2>/dev/null; then break; fi
    sleep 0.05
done
grep -Fq 'session-client-ready' "$test_dir/session-client-first"
DISPLAY="$display" "$test_dir/press-key" r
DISPLAY="$display" "$test_dir/press-key" --plain Right
DISPLAY="$display" "$test_dir/press-key" --plain Right
DISPLAY="$display" "$test_dir/press-key" --plain Return
DISPLAY="$display" "$test_dir/press-key" Right
DISPLAY="$display" "$test_dir/press-key" F8
wait "$session_client_pid"
session_client_pid=
for _ in $(seq 1 100); do
    if [[ $(grep -c "ready: $session_socket" "$session_log" 2>/dev/null || true) -ge 2 ]]; then
        break
    fi
    sleep 0.05
done
if [[ $(grep -c "ready: $session_socket" "$session_log" 2>/dev/null || true) -lt 2 ]]; then
    echo "Wayland self-restart did not reclaim its socket" >&2
    cat "$session_log" >&2
    exit 1
fi
if [[ $(wc -l <"$test_dir/wayland-autostart.log") -ne 1 ]]; then
    echo "Wayland self-restart reran autostart" >&2
    exit 1
fi
for pattern in 'application_id = "org.nobox.shell-probe"' 'workspace = 1'; do
    if ! grep -Fq "$pattern" "$test_dir/wayland-session.toml"; then
        echo "Wayland session snapshot is missing $pattern" >&2
        cat "$test_dir/wayland-session.toml" >&2
        exit 1
    fi
done
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$session_socket" \
    "$probe_binary" --session-restore >"$test_dir/session-restore"
grep -Fq 'session-restore-ok size=' "$test_dir/session-restore"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$session_socket" \
    "$probe_binary" --session-client >"$test_dir/session-client-handoff" 2>&1 &
session_client_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'session-client-ready' "$test_dir/session-client-handoff" 2>/dev/null; then break; fi
    sleep 0.05
done
DISPLAY="$display" "$test_dir/press-key" F9
wait "$session_client_pid"
session_client_pid=
wait "$wayland_pid"
wayland_pid=
[[ -e "$test_dir/wayland-handoff" ]]
if [[ -e "$runtime_dir/$session_socket" || -e "$runtime_dir/$session_socket.lock" ]]; then
    echo "Wayland restart handoff retained its compositor socket" >&2
    exit 1
fi

logout_log="$test_dir/logout-wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/session-config.toml" \
    NOBOX_STATE_FILE="$test_dir/wayland-session.toml" \
    "$nobox_binary" --backend wayland run --nested-x11 --no-autostart >"$logout_log" 2>&1 &
wayland_pid=$!
logout_socket=
for _ in $(seq 1 100); do
    logout_socket=$(sed -n 's/^ready: //p' "$logout_log" 2>/dev/null | head -n 1)
    if [[ -n "$logout_socket" ]]; then break; fi
    sleep 0.05
done
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$logout_socket" \
    "$probe_binary" --session-client >"$test_dir/session-client-logout" 2>&1 &
session_client_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'session-client-ready' "$test_dir/session-client-logout" 2>/dev/null; then break; fi
    sleep 0.05
done
DISPLAY="$display" "$test_dir/press-key" F10
wait "$session_client_pid"
session_client_pid=
wait "$wayland_pid"
wayland_pid=

expected_globals=$'ext_foreign_toplevel_list_v1\next_idle_notifier_v1\next_session_lock_manager_v1\next_workspace_manager_v1\nwl_compositor\nwl_data_device_manager\nwl_output\nwl_seat\nwl_shm\nwl_subcompositor\nwp_cursor_shape_manager_v1\nwp_fractional_scale_manager_v1\nwp_presentation\nwp_viewporter\nxdg_activation_v1\nxdg_wm_base\nzwlr_layer_shell_v1\nzwp_idle_inhibit_manager_v1\nzwp_keyboard_shortcuts_inhibit_manager_v1\nzwp_pointer_constraints_v1\nzwp_pointer_gestures_v1\nzwp_primary_selection_device_manager_v1\nzwp_relative_pointer_manager_v1\nzwp_tablet_manager_v2\nzxdg_decoration_manager_v1'
for run in $(seq 1 10); do
    socket="nobox-w2-$run"
    log="$test_dir/wayland-$run.log"
    exit_count=4
    if [[ "$run" == 1 ]]; then exit_count=0; fi
    renderer=auto
    if [[ "$run" == 1 ]]; then renderer=gles2; fi
    if [[ "$run" == 2 ]]; then renderer=pixman; fi
    if [[ "$run" == 2 ]]; then exit_count=5; fi
    if [[ "$run" == 9 || "$run" == 10 ]]; then exit_count=0; fi
    env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
        "$wayland_binary" --socket "$socket" --renderer "$renderer" \
        --exit-after-disconnects "$exit_count" \
        >"$log" 2>&1 &
    wayland_pid=$!

    for _ in $(seq 1 100); do
        if grep -Fq "ready: $socket" "$log" 2>/dev/null; then break; fi
        if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
        sleep 0.05
    done
    if ! grep -Fq "ready: $socket" "$log"; then
        echo "managed compositor run $run did not render and become ready" >&2
        cat "$log" >&2
        exit 1
    fi
    if pgrep -P "$wayland_pid" >/dev/null; then
        echo "managed compositor run $run unexpectedly created a child process" >&2
        exit 1
    fi

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" >"$test_dir/globals-$run"
    actual_globals=$(cut -d' ' -f1 "$test_dir/globals-$run")
    if [[ "$actual_globals" != "$expected_globals" ]]; then
        echo "managed compositor run $run advertised an unexpected global set" >&2
        cat "$test_dir/globals-$run" >&2
        exit 1
    fi
    grep -Fxq 'wp_cursor_shape_manager_v1 2' "$test_dir/globals-$run"
    grep -Fxq 'ext_idle_notifier_v1 2' "$test_dir/globals-$run"
    grep -Fxq 'ext_session_lock_manager_v1 1' "$test_dir/globals-$run"
    grep -Fxq 'zwp_idle_inhibit_manager_v1 1' "$test_dir/globals-$run"
    grep -Fxq 'zwp_tablet_manager_v2 1' "$test_dir/globals-$run"

    if [[ "$run" == 2 ]]; then
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-lock >"$test_dir/pointer-lock-pixman"
        grep -Fq 'pointer-lock-ok relative hint' "$test_dir/pointer-lock-pixman"
    fi

    if [[ "$run" == 1 ]]; then
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --outputs >"$test_dir/outputs"
        if ! grep -Eq '^output id=[0-9]+ name=nobox-1 position=0,0 mode=[1-9][0-9]*x[1-9][0-9]*@60\.000 transform=Normal scale=1$' \
            "$test_dir/outputs"; then
            echo "managed output probe reported unexpected state" >&2
            cat "$test_dir/outputs" >&2
            exit 1
        fi
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --layer-shell >"$test_dir/layer-shell"
        grep -Fq 'layer-shell-ok size=' "$test_dir/layer-shell"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-configure >"$test_dir/invalid-configure"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-role >"$test_dir/invalid-role"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-viewport >"$test_dir/invalid-viewport"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-fractional-scale >"$test_dir/invalid-fractional-scale"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --surface-limit >"$test_dir/surface-limit"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-configure"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-role"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-viewport"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-fractional-scale"
        grep -Fq 'surface-limit-ok' "$test_dir/surface-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --surface-protocols >"$test_dir/surface-protocols"
        grep -Fq 'surface-protocols-ok preferred-scale=120' "$test_dir/surface-protocols"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --selection >"$test_dir/selection"
        grep -Fq 'selection-ok clipboard primary cancellation' "$test_dir/selection"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --selection-owner >"$test_dir/selection-owner" 2>&1 &
        selection_owner_pid=$!
        for _ in $(seq 1 100); do
            if grep -Fq 'selection-owner-ready' "$test_dir/selection-owner" 2>/dev/null; then break; fi
            sleep 0.05
        done
        grep -Fq 'selection-owner-ready' "$test_dir/selection-owner"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --selection-observer >"$test_dir/selection-observer" 2>&1 &
        selection_observer_pid=$!
        for _ in $(seq 1 100); do
            if grep -Fq 'selection-observer-ready' "$test_dir/selection-observer" 2>/dev/null; then break; fi
            sleep 0.05
        done
        grep -Fq 'selection-observer-ready' "$test_dir/selection-observer"
        kill "$selection_owner_pid"
        wait "$selection_owner_pid" 2>/dev/null || true
        selection_owner_pid=
        wait "$selection_observer_pid"
        selection_observer_pid=
        grep -Fq 'selection-owner-death-ok' "$test_dir/selection-observer"
        for limit in source device mime mime-size; do
            DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
                "$probe_binary" "--selection-$limit-limit" >"$test_dir/selection-$limit-limit"
            grep -Fq 'selection-limit-ok' "$test_dir/selection-$limit-limit"
        done
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --selection >"$test_dir/selection-after-limits"
        grep -Fq 'selection-ok clipboard primary cancellation' \
            "$test_dir/selection-after-limits"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --dnd >"$test_dir/dnd"
        grep -Fq 'dnd-ok copy transfer drop finish icon-frame' "$test_dir/dnd"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --dnd-cancel >"$test_dir/dnd-cancel"
        grep -Fq 'dnd-cancel-ok' "$test_dir/dnd-cancel"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-constraint-duplicate \
            >"$test_dir/pointer-constraint-duplicate"
        grep -Fq 'pointer-constraint-duplicate-ok' \
            "$test_dir/pointer-constraint-duplicate"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-extension-limit \
            >"$test_dir/pointer-extension-limit"
        grep -Fq 'pointer-extension-limit-ok' \
            "$test_dir/pointer-extension-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --presentation-limit >"$test_dir/presentation-limit"
        grep -Fq 'presentation-limit-ok' "$test_dir/presentation-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --presentation >"$test_dir/presentation"
        grep -Fq 'presentation-ok monotonic refresh sequence' "$test_dir/presentation"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --shortcut-inhibit-limit \
            >"$test_dir/shortcut-inhibit-limit"
        grep -Fq 'shortcut-inhibit-limit-ok' "$test_dir/shortcut-inhibit-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --shortcut-inhibit >"$test_dir/shortcut-inhibit"
        grep -Fq 'shortcut-inhibit-ok forward restore' "$test_dir/shortcut-inhibit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-gesture-limit \
            >"$test_dir/pointer-gesture-limit"
        grep -Fq 'pointer-gesture-limit-ok' "$test_dir/pointer-gesture-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-gestures >"$test_dir/pointer-gestures"
        grep -Fq 'pointer-gestures-ok swipe pinch hold' "$test_dir/pointer-gestures"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --cursor-shape-limit >"$test_dir/cursor-shape-limit"
        grep -Fq 'cursor-shape-limit-ok' "$test_dir/cursor-shape-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --cursor-shape >"$test_dir/cursor-shape"
        grep -Fq 'cursor-shape-ok text ew-resize' "$test_dir/cursor-shape"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --touch-limit >"$test_dir/touch-limit"
        grep -Fq 'touch-limit-ok' "$test_dir/touch-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --touch >"$test_dir/touch"
        grep -Fq 'touch-ok capability device' "$test_dir/touch"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --tablet-limit >"$test_dir/tablet-limit"
        grep -Fq 'tablet-limit-ok' "$test_dir/tablet-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --tablet >"$test_dir/tablet"
        grep -Fq 'tablet-ok manager seat' "$test_dir/tablet"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-confine >"$test_dir/pointer-confine"
        grep -Fq 'pointer-confine-ok relative boundary' "$test_dir/pointer-confine"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --pointer-lock >"$test_dir/pointer-lock"
        grep -Fq 'pointer-lock-ok relative hint' "$test_dir/pointer-lock"
        if command -v gtk4-demo >/dev/null 2>&1; then
            env -u DISPLAY GDK_BACKEND=wayland NO_AT_BRIDGE=1 \
                XDG_DATA_DIRS=/usr/local/share:/usr/share \
                XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
                gtk4-demo >"$test_dir/gtk4-demo" 2>&1 &
            gtk_pid=$!
            sleep 2
            if ! kill -0 "$gtk_pid" 2>/dev/null; then
                echo "GTK4 native Wayland smoke exited early" >&2
                cat "$test_dir/gtk4-demo" >&2
                exit 1
            fi
            kill "$gtk_pid"
            wait "$gtk_pid" 2>/dev/null || true
            gtk_pid=
        fi
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --popup-grab >"$test_dir/popup-grab"
        grep -Fq 'popup-grab-ok' "$test_dir/popup-grab"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --attention >"$test_dir/attention"
        grep -Fq 'attention-ok changed_pixels=' "$test_dir/attention"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --focus-cycle >"$test_dir/focus-cycle"
        grep -Fq 'focus-cycle-ok center=' "$test_dir/focus-cycle"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --menu >"$test_dir/menu"
        grep -Fq 'menu-ok center=' "$test_dir/menu"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --unresponsive >"$test_dir/unresponsive" &
        unresponsive_pid=$!
    fi

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" --shell-input >"$test_dir/shell-input-$run"
    grep -Fq 'shell-ok configures=' "$test_dir/shell-input-$run"

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" --shell >"$test_dir/shell-a-$run" &
    shell_a=$!
    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" --shell >"$test_dir/shell-b-$run" &
    shell_b=$!
    wait "$shell_a"
    wait "$shell_b"
    grep -Fq 'shell-ok configures=' "$test_dir/shell-a-$run"
    grep -Fq 'shell-ok configures=' "$test_dir/shell-b-$run"

    if [[ "$run" == 1 ]]; then
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --idle-inhibit-limit >"$test_dir/idle-inhibit-limit"
        grep -Fq 'idle-inhibitor-limit-ok' "$test_dir/idle-inhibit-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --idle-notify-limit >"$test_dir/idle-notify-limit"
        grep -Fq 'idle-notification-limit-ok' "$test_dir/idle-notify-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --idle >"$test_dir/idle"
        grep -Fq 'idle-ok inhibit input-idle resume' "$test_dir/idle"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --shell >"$test_dir/shell-after-idle"
        grep -Fq 'shell-ok configures=' "$test_dir/shell-after-idle"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock >"$test_dir/session-lock"
        grep -Fq 'session-lock-ok secure-frame keyboard unlock' "$test_dir/session-lock"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --shell >"$test_dir/shell-after-session-lock"
        grep -Fq 'shell-ok configures=' "$test_dir/shell-after-session-lock"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --close >"$test_dir/close"
        grep -Fq 'close-ok' "$test_dir/close"
        wait "$unresponsive_pid"
        unresponsive_pid=
        grep -Fq 'unresponsive-ok' "$test_dir/unresponsive"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
            "$nobox_binary" --backend wayland --exit
    fi

    if [[ "$run" == 10 ]]; then
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock-abandon >"$test_dir/session-lock-abandon"
        grep -Fq 'session-lock-abandon-ok locked secure-frame' \
            "$test_dir/session-lock-abandon"
        for _ in $(seq 1 50); do
            if DISPLAY="$display" "$test_dir/x11-largest-window-pixel" \
                >"$test_dir/session-lock-pixel" 2>/dev/null; then
                break
            fi
            sleep 0.02
        done
        grep -Fxq 'pixel=0x000000' "$test_dir/session-lock-pixel"
        if DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --shell >"$test_dir/shell-during-abandoned-lock" 2>&1; then
            echo "ordinary shell rendered during an abandoned session lock" >&2
            exit 1
        fi
        grep -Fq 'mapped surface received no frame callback' \
            "$test_dir/shell-during-abandoned-lock"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock-competitor >"$test_dir/session-lock-competitor"
        grep -Fq 'session-lock-competitor-ok finished' \
            "$test_dir/session-lock-competitor"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock-limit >"$test_dir/session-lock-limit"
        grep -Fq 'session-lock-limit-ok' "$test_dir/session-lock-limit"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" >"$test_dir/globals-after-abandoned-lock"
        grep -Fxq 'ext_session_lock_manager_v1 1' \
            "$test_dir/globals-after-abandoned-lock"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
            "$nobox_binary" --backend wayland --exit
    fi

    if [[ "$run" == 9 ]]; then
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock-invalid-unlock \
            >"$test_dir/session-lock-invalid-unlock"
        grep -Fq 'session-lock-invalid-unlock-ok secure-disconnect' \
            "$test_dir/session-lock-invalid-unlock"
        for _ in $(seq 1 50); do
            if DISPLAY="$display" "$test_dir/x11-largest-window-pixel" \
                >"$test_dir/session-lock-invalid-pixel" 2>/dev/null; then
                break
            fi
            sleep 0.02
        done
        grep -Fxq 'pixel=0x000000' "$test_dir/session-lock-invalid-pixel"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --session-lock-competitor \
            >"$test_dir/session-lock-invalid-competitor"
        grep -Fq 'session-lock-competitor-ok finished' \
            "$test_dir/session-lock-invalid-competitor"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
            "$nobox_binary" --backend wayland --exit
    fi

    wait "$wayland_pid"
    wayland_pid=
    if [[ -e "$runtime_dir/$socket" || -e "$runtime_dir/$socket.lock" ]]; then
        echo "managed compositor run $run leaked its socket or lock" >&2
        exit 1
    fi
done

echo "Wayland globals, two-client shell, rendering, and 10-cycle lifecycle proof passed on $nested_x_server $display"
