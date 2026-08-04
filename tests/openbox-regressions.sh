#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: openbox-regressions.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: openbox-regressions.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for Openbox regression tests"
        exit 77
    fi
done
if command -v Xnest >/dev/null 2>&1; then
    x_server=(Xnest)
    x_server_args=(-geometry 800x600 -ac)
elif command -v Xephyr >/dev/null 2>&1; then
    x_server=(Xephyr)
    x_server_args=(-screen 800x600 -ac)
elif command -v Xvfb >/dev/null 2>&1; then
    x_server=(Xvfb)
    x_server_args=(-screen 0 800x600x24 -ac)
else
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for Openbox regression tests"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$openbox_source/tests/aspect.c" -o "$test_dir/aspect" -lX11
cc -include unistd.h "$openbox_source/tests/grav.c" -o "$test_dir/grav" -lX11
cc "$openbox_source/tests/modal.c" -o "$test_dir/modal" -lX11
cc "$openbox_source/tests/modal2.c" -o "$test_dir/modal2" -lX11
cc -include unistd.h "$openbox_source/tests/groupmodal.c" -o "$test_dir/groupmodal" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11

display=
for number in $(seq 111 130); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
sleep 0.4
DISPLAY="$display" "$test_dir/aspect" >"$test_dir/client.log" 2>&1 &
client_pid=$!
sleep 0.7

window_tree=$(DISPLAY="$display" xwininfo -root -tree)
if ! grep -q '400x400+10+10' <<<"$window_tree"; then
    echo "Openbox aspect regression did not produce a constrained square" >&2
    echo "$window_tree" >&2
    exit 1
fi
echo "Openbox aspect regression passed on $display"

kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=
DISPLAY="$display" "$test_dir/grav" >"$test_dir/client.log" 2>&1 &
client_pid=$!
sleep 1.4

window_tree=$(DISPLAY="$display" xwininfo -root -tree)
if ! grep -q '900x275+252+373' <<<"$window_tree"; then
    echo "Openbox gravity regression did not preserve the south-east anchor" >&2
    echo "$window_tree" >&2
    exit 1
fi
echo "Openbox gravity regression passed on $display"

kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

window_for_geometry() {
    local geometry=$1
    DISPLAY="$display" xwininfo -root -tree |
        awk -v geometry="$geometry" 'index($0, geometry) { print $1; exit }'
}

run_modal_regression() {
    local program=$1
    local parent_geometry=$2
    local child_geometry=$3
    local parent_window=
    local child_window=
    local active_window=

    DISPLAY="$display" "$test_dir/$program" >"$test_dir/$program.log" 2>&1 &
    client_pid=$!
    for _ in $(seq 1 30); do
        parent_window=$(window_for_geometry "$parent_geometry")
        child_window=$(window_for_geometry "$child_geometry")
        if [[ -n "$parent_window" && -n "$child_window" ]]; then break; fi
        sleep 0.1
    done
    if [[ -z "$parent_window" || -z "$child_window" ]]; then
        echo "Openbox $program regression windows did not map" >&2
        DISPLAY="$display" xwininfo -root -tree >&2
        exit 1
    fi

    DISPLAY="$display" "$test_dir/request-activation" "$parent_window"
    sleep 0.2
    active_window=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW | awk '{ print $NF }')
    if (( active_window != child_window )); then
        echo "Openbox $program regression activated $active_window instead of modal $child_window" >&2
        exit 1
    fi
    echo "Openbox $program regression passed on $display"

    kill "$client_pid" 2>/dev/null || true
    wait "$client_pid" 2>/dev/null || true
    client_pid=
}

run_modal_regression modal 400x400+10+10 200x200+10+10
run_modal_regression modal2 400x400+10+10 200x200+10+10
run_modal_regression groupmodal 300x300+0+0 100x100+0+0
