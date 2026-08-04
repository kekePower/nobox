#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-focus-events.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 focus-event test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 focus-event test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
grab_pid=
client_pids=()
cleanup() {
    if [[ -n "$grab_pid" ]]; then kill "$grab_pid" 2>/dev/null || true; fi
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/focus-tree-client.c" -o "$test_dir/focus-tree-client" -lX11
cc "$(dirname "$0")/set-input-focus.c" -o "$test_dir/set-input-focus" -lX11
cc "$(dirname "$0")/grab-keyboard.c" -o "$test_dir/grab-keyboard" -lX11

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

launch_client() {
    local title=$1
    local x=$2
    local y=$3
    local output=$test_dir/$title.windows
    DISPLAY="$display" "$test_dir/focus-tree-client" "$title" "$x" "$y" \
        >"$output" 2>&1 &
    client_pids+=("$!")
    local top=
    local child=
    for _ in $(seq 1 50); do
        if [[ -s "$output" ]]; then read -r top child <"$output"; fi
        if [[ -n "$top" && -n "$child" ]] &&
            DISPLAY="$display" xprop -id "$top" _NET_FRAME_EXTENTS >/dev/null 2>&1; then
            launched_top=$top
            launched_child=$child
            return 0
        fi
        sleep 0.05
    done
    echo "$title did not map with a focusable child" >&2
    return 1
}

active_window() {
    DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' | tail -n 1 || true
}

wait_for_active() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(active_window)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "active window was '$observed', expected '$1'" >&2
    return 1
}

wait_for_focused_state() {
    local window=$1
    local expected=$2
    local state=
    for _ in $(seq 1 50); do
        state=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE 2>/dev/null || true)
        if [[ "$expected" == yes && "$state" == *'_NET_WM_STATE_FOCUSED'* ]]; then
            return 0
        fi
        if [[ "$expected" == no && "$state" != *'_NET_WM_STATE_FOCUSED'* ]]; then
            return 0
        fi
        sleep 0.05
    done
    echo "unexpected focused state for $window: $state" >&2
    return 1
}

launch_client nobox-focus-tree-first 60 70
first_window=$launched_top
first_child=$launched_child
launch_client nobox-focus-tree-second 440 310
second_window=$launched_top
second_child=$launched_child

# Toolkit child focus is attributed to its managed top-level.
DISPLAY="$display" "$test_dir/set-input-focus" "$first_child"
wait_for_active "$first_window"
wait_for_focused_state "$first_window" yes
wait_for_focused_state "$second_window" no
DISPLAY="$display" "$test_dir/set-input-focus" "$second_child"
wait_for_active "$second_window"
wait_for_focused_state "$first_window" no
wait_for_focused_state "$second_window" yes

# Ancestor/inferior focus transitions stay within one logical client.
DISPLAY="$display" "$test_dir/set-input-focus" "$second_window"
wait_for_active "$second_window"
DISPLAY="$display" "$test_dir/set-input-focus" "$second_child"
wait_for_active "$second_window"

# A temporary keyboard grab emits grab-mode focus noise but changes no owner.
DISPLAY="$display" "$test_dir/grab-keyboard" "$first_window" 500 \
    >"$test_dir/grab.log" 2>&1 &
grab_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^grabbed$' "$test_dir/grab.log"; then break; fi
    sleep 0.02
done
if ! grep -q '^grabbed$' "$test_dir/grab.log"; then
    echo "keyboard grab did not start" >&2
    cat "$test_dir/grab.log" >&2
    exit 1
fi
wait_for_active "$second_window"
wait_for_focused_state "$second_window" yes
wait "$grab_pid"
grab_pid=
wait_for_active "$second_window"
wait_for_focused_state "$second_window" yes

# Focus leaving the managed tree clears EWMH ownership without stealing it back.
root_window=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')
DISPLAY="$display" "$test_dir/set-input-focus" "$root_window"
wait_for_active ''
wait_for_focused_state "$first_window" no
wait_for_focused_state "$second_window" no
DISPLAY="$display" "$test_dir/set-input-focus" "$first_child"
wait_for_active "$first_window"
wait_for_focused_state "$first_window" yes

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during focus-event reconciliation" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 focus-event reconciliation passed on $display"
