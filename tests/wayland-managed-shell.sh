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
cleanup() {
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

expected_globals=$'ext_foreign_toplevel_list_v1\next_workspace_manager_v1\nwl_compositor\nwl_output\nwl_seat\nwl_shm\nwl_subcompositor\nwp_fractional_scale_manager_v1\nwp_viewporter\nxdg_activation_v1\nxdg_wm_base\nzwlr_layer_shell_v1\nzxdg_decoration_manager_v1'
for run in $(seq 1 10); do
    socket="nobox-w2-$run"
    log="$test_dir/wayland-$run.log"
    exit_count=4
    if [[ "$run" == 1 ]]; then exit_count=0; fi
    renderer=auto
    if [[ "$run" == 1 ]]; then renderer=gles2; fi
    if [[ "$run" == 2 ]]; then renderer=pixman; fi
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
            "$probe_binary" --close >"$test_dir/close"
        grep -Fq 'close-ok' "$test_dir/close"
        wait "$unresponsive_pid"
        unresponsive_pid=
        grep -Fq 'unresponsive-ok' "$test_dir/unresponsive"
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
