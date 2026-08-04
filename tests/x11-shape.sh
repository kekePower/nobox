#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-shape.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: x11-shape.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X Shape regression"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X Shape regression"
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

if ! cc "$openbox_source/tests/shape.c" -o "$test_dir/shape" -lX11 -lXext \
    || ! cc "$(dirname "$0")/shape-control.c" -o "$test_dir/shape-control" -lX11 -lXext; then
    echo "SKIP: X Shape development libraries are required"
    exit 77
fi

display=
for number in $(seq 171 190); do
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
if ! DISPLAY="$display" xdpyinfo | grep -q 'SHAPE'; then
    echo "SKIP: nested X server does not provide X Shape"
    exit 77
fi

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox did not claim the nested X server" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi

find_client() {
    local candidate
    candidate=$(DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk '/400x100/ { print $1; exit }' || true)
    if [[ -n "$candidate" ]] \
        && DISPLAY="$display" xprop -id "$candidate" _NET_FRAME_EXTENTS 2>/dev/null |
            grep -q '='; then
        printf '%s\n' "$candidate"
    fi
}

find_frame() {
    DISPLAY="$display" xwininfo -tree -id "$1" 2>/dev/null |
        awk '/Parent window id:/ { print $4; exit }'
}

wait_for_bounding_shape() {
    local frame=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" "$test_dir/shape-control" "$frame" bounding)
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "frame bounding shape was '$observed', expected '$expected'" >&2
    return 1
}

DISPLAY="$display" "$test_dir/shape" >"$test_dir/client.log" 2>&1 &
client_pid=$!
client_window=
for _ in $(seq 1 40); do
    client_window=$(find_client)
    if [[ -n "$client_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$client_window" ]]; then
    echo "Openbox bounding-shape client did not map" >&2
    exit 1
fi
frame_window=$(find_frame "$client_window")
wait_for_bounding_shape "$frame_window" '1 0 0 400 114'

DISPLAY="$display" "$test_dir/shape-control" "$client_window" clear
wait_for_bounding_shape "$frame_window" '0 -2 -2 404 128'
DISPLAY="$display" "$test_dir/shape-control" "$client_window" inset
wait_for_bounding_shape "$frame_window" '1 0 0 400 114'
echo "Initial and live X Shape bounding regions passed on $display"

kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=
for _ in $(seq 1 30); do
    if [[ -z "$(find_client)" ]]; then break; fi
    sleep 0.05
done

DISPLAY="$display" "$test_dir/shape" -i >"$test_dir/input-client.log" 2>&1 &
client_pid=$!
input_window=
for _ in $(seq 1 40); do
    input_window=$(find_client)
    if [[ -n "$input_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$input_window" ]]; then
    echo "Openbox input-shape client did not map" >&2
    exit 1
fi
input_frame=$(find_frame "$input_window")
input_shape=
for _ in $(seq 1 40); do
    input_shape=$(DISPLAY="$display" "$test_dir/shape-control" "$input_frame" input)
    if [[ "$input_shape" == *' 0 0 400 114' ]]; then break; fi
    sleep 0.05
done
if [[ "$input_shape" != *' 0 0 400 114' ]]; then
    echo "frame input shape was '$input_shape', expected bounds '0 0 400 114'" >&2
    exit 1
fi

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during X Shape handling" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
if grep -q 'non-fatal X11 protocol error' "$test_dir/nobox.log"; then
    echo "X11 protocol errors occurred during X Shape handling" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X Shape input regions passed on $display"
