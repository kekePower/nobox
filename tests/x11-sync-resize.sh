#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-sync-resize.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 synchronized-resize test"
        exit 77
    fi
done
if command -v Xnest >/dev/null 2>&1; then
    x_server=(Xnest)
    x_server_args=(-geometry 800x600 -depth 24 -ac)
elif command -v Xephyr >/dev/null 2>&1; then
    x_server=(Xephyr)
    x_server_args=(-screen 800x600x24 -ac)
elif command -v Xvfb >/dev/null 2>&1; then
    x_server=(Xvfb)
    x_server_args=(-screen 0 800x600x24 -ac)
else
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 synchronized-resize test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
driver_pid=
client_pids=()
cleanup() {
    if [[ -n "$driver_pid" ]]; then kill "$driver_pid" 2>/dev/null || true; fi
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/sync-resize-client.c" -o "$test_dir/sync-resize-client" \
    -lX11 -lXext; then
    echo "SKIP: X Sync development libraries are required for synchronized-resize tests"
    exit 77
fi
if ! cc "$(dirname "$0")/interactive-drag.c" -o "$test_dir/interactive-drag" \
    -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for synchronized-resize tests"
    exit 77
fi
printf '%s\n' \
    '[focus]' \
    'focus_new = false' \
    'follow_mouse = false' \
    'raise_on_focus = false' >"$test_dir/config.toml"

display=
for number in $(seq 331 350); do
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
if ! DISPLAY="$display" xdpyinfo | grep -q 'SYNC'; then
    echo "SKIP: the nested X server does not provide the X Sync extension"
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
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED |
    grep -q '_NET_WM_SYNC_REQUEST_COUNTER'; then
    echo "nobox did not advertise synchronized resizing" >&2
    exit 1
fi

wait_for_line() {
    local file=$1
    local pattern=$2
    for _ in $(seq 1 100); do
        if grep -q "$pattern" "$file"; then return 0; fi
        sleep 0.05
    done
    echo "did not observe '$pattern' in $file" >&2
    tail -n 50 "$file" >&2 || true
    return 1
}

launch_client() {
    local mode=$1
    local output=$test_dir/$mode.client
    DISPLAY="$display" "$test_dir/sync-resize-client" "$mode" >"$output" 2>&1 &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 100); do
        if [[ -s "$output" ]]; then
            launched_window=$(awk '/^window / { print $2; exit }' "$output")
        fi
        if [[ -n "$launched_window" ]] && grep -q '^initial 0$' "$output"; then
            launched_output=$output
            return 0
        fi
        sleep 0.05
    done
    echo "$mode synchronized-resize client did not map with an initialized counter" >&2
    tail -n 50 "$output" >&2 || true
    return 1
}

window_size() {
    DISPLAY="$display" xwininfo -id "$1" |
        awk '/Width:/ { width=$2 } /Height:/ { height=$2 } END { print width "x" height }'
}

launch_client responsive
responsive_window=$launched_window
responsive_output=$launched_output
DISPLAY="$display" "$test_dir/interactive-drag" \
    "$responsive_window" resize commit 50 35
wait_for_line "$responsive_output" '^request 1$'
wait_for_line "$responsive_output" '^ack 1$'

launch_client stalled
stalled_window=$launched_window
stalled_output=$launched_output
DISPLAY="$display" "$test_dir/interactive-drag" \
    "$stalled_window" resize commit 40 30 80 50 2200 &
driver_pid=$!
sleep 0.6
paced_size=$(window_size "$stalled_window")
sleep 1.2
fallback_size=$(window_size "$stalled_window")
if [[ "$paced_size" == "$fallback_size" ]]; then
    echo "stalled client did not receive the pending geometry after the timeout" >&2
    echo "size remained $paced_size" >&2
    exit 1
fi
wait "$driver_pid"
driver_pid=
wait_for_line "$stalled_output" '^request 1$'
wait_for_line "$test_dir/nobox.log" \
    'client did not acknowledge synchronized resize; continuing without pacing'
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during synchronized-resize fallback" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

echo "X11 synchronized-resize checks passed"
