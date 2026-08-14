#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-w0-nested.sh /path/to/nobox /path/to/nobox-wayland /path/to/probe}
wayland_binary=${2:?missing nobox-wayland binary}
probe_binary=${3:?missing nobox-wayland-probe binary}

for dependency in xdpyinfo pgrep; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for W0 Wayland tests"
        exit 77
    fi
done

if [[ -z ${NOBOX_XSERVER:-} ]]; then
    if command -v Xvfb >/dev/null 2>&1; then
        export NOBOX_XSERVER=xvfb
    elif command -v Xephyr >/dev/null 2>&1; then
        export NOBOX_XSERVER=xephyr
    else
        echo "SKIP: Xvfb or Xephyr is required for W0 Wayland tests"
        exit 77
    fi
fi
if [[ ${NOBOX_XSERVER,,} == xnest ]]; then
    echo "SKIP: W0 rendering requires an EGL-capable Xvfb or Xephyr server"
    exit 77
fi

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
xserver_pid=
wayland_pid=
cleanup() {
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
grep -Fq '[ok] Wayland backend: Smithay 0.7.0' "$test_dir/doctor.log"
grep -Fq 'ready: yes (experimental nested-X11 infrastructure only)' "$test_dir/doctor.log"

expected_globals=$'wl_compositor\nwl_output\nwl_shm\nwl_subcompositor'
for run in $(seq 1 10); do
    socket="nobox-w0-$run"
    log="$test_dir/wayland-$run.log"
    env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
        "$wayland_binary" --socket "$socket" --exit-after-disconnects 1 \
        >"$log" 2>&1 &
    wayland_pid=$!

    for _ in $(seq 1 100); do
        if grep -Fq "ready: $socket" "$log" 2>/dev/null; then break; fi
        if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
        sleep 0.05
    done
    if ! grep -Fq "ready: $socket" "$log"; then
        echo "W0 compositor run $run did not render and become ready" >&2
        cat "$log" >&2
        exit 1
    fi
    if pgrep -P "$wayland_pid" >/dev/null; then
        echo "W0 compositor run $run unexpectedly created a child process" >&2
        exit 1
    fi

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" >"$test_dir/globals-$run"
    actual_globals=$(cut -d' ' -f1 "$test_dir/globals-$run")
    if [[ "$actual_globals" != "$expected_globals" ]]; then
        echo "W0 compositor run $run advertised an unexpected global set" >&2
        cat "$test_dir/globals-$run" >&2
        exit 1
    fi

    wait "$wayland_pid"
    wayland_pid=
    if [[ -e "$runtime_dir/$socket" || -e "$runtime_dir/$socket.lock" ]]; then
        echo "W0 compositor run $run leaked its socket or lock" >&2
        exit 1
    fi
done

echo "W0 Wayland protocol and 10-cycle lifecycle proof passed on $nested_x_server $display"
