#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-managed-shell.sh /path/to/nobox /path/to/nobox-wayland /path/to/probe}
wayland_binary=${2:?missing nobox-wayland binary}
probe_binary=${3:?missing nobox-wayland-probe binary}

for dependency in xdpyinfo pgrep; do
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
xserver_pid=
wayland_pid=
unresponsive_pid=
cleanup() {
    if [[ -n "$unresponsive_pid" ]]; then kill "$unresponsive_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_pid" ]]; then kill "$wayland_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

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
grep -Fq 'ready: yes (managed nested-X11 Wayland shell)' "$test_dir/doctor.log"

cat >"$test_dir/keyboard-config.toml" <<'EOF'
[panel]
enabled = false

[keyboard]
inherit_defaults = false

[[keyboard.bindings]]
key = "W-r"
action = { type = "resize" }
EOF
keyboard_log="$test_dir/keyboard-wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
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
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=

expected_globals=$'ext_foreign_toplevel_list_v1\next_workspace_manager_v1\nwl_compositor\nwl_output\nwl_seat\nwl_shm\nwl_subcompositor\nxdg_activation_v1\nxdg_wm_base\nzwlr_layer_shell_v1\nzxdg_decoration_manager_v1'
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
            "$probe_binary" --layer-shell >"$test_dir/layer-shell"
        grep -Fq 'layer-shell-ok size=' "$test_dir/layer-shell"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-configure >"$test_dir/invalid-configure"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --invalid-role >"$test_dir/invalid-role"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-configure"
        grep -Fq 'protocol-error-ok' "$test_dir/invalid-role"
        DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
            "$probe_binary" --popup-grab >"$test_dir/popup-grab"
        grep -Fq 'popup-grab-ok' "$test_dir/popup-grab"
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
