#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-ping.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 ping test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 ping test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/ping-client.c" -o "$test_dir/ping-client" -lX11
cc "$(dirname "$0")/request-pager.c" -o "$test_dir/request-pager" -lX11
printf '%s\n' \
    '[focus]' \
    'focus_new = false' \
    'follow_mouse = false' \
    'raise_on_focus = false' >"$test_dir/config.toml"

display=
for number in $(seq 311 330); do
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
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED | grep -q '_NET_WM_PING'; then
    echo "nobox did not advertise _NET_WM_PING" >&2
    exit 1
fi

launch_client() {
    local mode=$1
    local title=$2
    local output=$test_dir/$mode.client
    DISPLAY="$display" "$test_dir/ping-client" "$mode" "$title" >"$output" 2>&1 &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 50); do
        if [[ -s "$output" ]]; then
            launched_window=$(awk '/^window / { print $2; exit }' "$output")
        fi
        if [[ -n "$launched_window" ]] &&
            DISPLAY="$display" xprop -id "$launched_window" _NET_FRAME_EXTENTS \
                >/dev/null 2>&1; then
            launched_output=$output
            return 0
        fi
        sleep 0.05
    done
    echo "$mode ping client did not map" >&2
    return 1
}

request_close() {
    DISPLAY="$display" "$test_dir/request-pager" close "$1"
}

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

launch_client responsive nobox-responsive-ping
responsive_window=$launched_window
responsive_output=$launched_output
request_close "$responsive_window"
wait_for_line "$responsive_output" '^delete '
wait_for_line "$responsive_output" "^pong .* ${responsive_window,,}$"
sleep 3.2
if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$responsive_window"; then
    echo "responsive ping client was disconnected" >&2
    exit 1
fi
if grep -q 'did not answer _NET_WM_PING' "$test_dir/nobox.log"; then
    echo "responsive ping client was marked unresponsive" >&2
    exit 1
fi

launch_client late nobox-late-ping
late_window=$launched_window
late_output=$launched_output
request_close "$late_window"
wait_for_line "$test_dir/nobox.log" 'did not answer _NET_WM_PING'
visible_title=
for _ in $(seq 1 40); do
    visible_title=$(DISPLAY="$display" xprop -id "$late_window" _NET_WM_VISIBLE_NAME \
        2>/dev/null || true)
    if [[ "$visible_title" == *'Not Responding'* ]]; then break; fi
    sleep 0.05
done
if [[ "$visible_title" != *'Not Responding'* ]]; then
    echo "timed-out client did not publish a visible unresponsive title" >&2
    exit 1
fi
wait_for_line "$late_output" "^pong .* ${late_window,,}$"
wait_for_line "$test_dir/nobox.log" 'resumed responding to pings'
if DISPLAY="$display" xprop -id "$late_window" _NET_WM_VISIBLE_NAME 2>/dev/null |
    grep -q 'Not Responding'; then
    echo "late pong did not clear the visible unresponsive title" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$late_window"; then
    echo "late ping client was disconnected instead of recovering" >&2
    exit 1
fi

launch_client hung nobox-hung-ping
hung_window=$launched_window
warning_count=$(grep -c 'did not answer _NET_WM_PING' "$test_dir/nobox.log" || true)
request_close "$hung_window"
for _ in $(seq 1 100); do
    observed=$(grep -c 'did not answer _NET_WM_PING' "$test_dir/nobox.log" || true)
    if (( observed > warning_count )); then break; fi
    sleep 0.05
done
if (( observed <= warning_count )); then
    echo "hung ping client was not marked unresponsive" >&2
    exit 1
fi
request_close "$hung_window"
for _ in $(seq 1 50); do
    if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$hung_window"; then
        break
    fi
    sleep 0.05
done
if DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$hung_window"; then
    echo "repeated close did not disconnect the timed-out client" >&2
    exit 1
fi
if ! grep -q 'force-disconnecting an unresponsive X11 client' "$test_dir/nobox.log"; then
    echo "forced disconnect was not diagnosed" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during ping handling" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 EWMH ping responsiveness handling passed on $display"
