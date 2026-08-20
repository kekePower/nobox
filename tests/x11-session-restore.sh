#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-session-restore.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 session-restore test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
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

cc "$(dirname "$0")/session-client.c" -o "$test_dir/session-client" -lX11
cc "$(dirname "$0")/request-workspace.c" -o "$test_dir/request-workspace" -lX11
cc "$(dirname "$0")/request-pager.c" -o "$test_dir/request-pager" -lX11
cc "$(dirname "$0")/request-state.c" -o "$test_dir/request-state" -lX11
cc "$(dirname "$0")/set-input-focus.c" -o "$test_dir/set-input-focus" -lX11
cc "$(dirname "$0")/set-window-geometry.c" -o "$test_dir/set-window-geometry" -lX11
cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["one", "two", "three"]

[focus]
focus_new = false
follow_mouse = false
raise_on_focus = false
EOF

display=
for number in $(seq 371 390); do
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

start_nobox() {
    local log=$1
    DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
        "$nobox_binary" run --no-autostart >"$log" 2>&1 &
    nobox_pid=$!
    for _ in $(seq 1 50); do
        if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
            grep -q 'window id'; then return 0; fi
        sleep 0.1
    done
    echo "nobox did not start" >&2
    tail -n 100 "$log" >&2 || true
    return 1
}

launch_client() {
    local id=$1
    local title=$2
    local x=$3
    local y=$4
    local output=$test_dir/$title.client
    DISPLAY="$display" "$test_dir/session-client" "$id" "$title" "$x" "$y" \
        >"$output" 2>&1 &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 50); do
        launched_window=$(awk 'NR == 1 { print; exit }' "$output" 2>/dev/null || true)
        if [[ -n "$launched_window" ]] &&
            DISPLAY="$display" xprop -id "$launched_window" _NET_FRAME_EXTENTS \
                >/dev/null 2>&1; then return 0; fi
        sleep 0.05
    done
    echo "session client $id did not map" >&2
    return 1
}

wait_for_property() {
    local window=$1
    local property=$2
    local pattern=$3
    for _ in $(seq 1 100); do
        if DISPLAY="$display" xprop -id "$window" "$property" 2>/dev/null |
            grep -q "$pattern"; then return 0; fi
        sleep 0.05
    done
    echo "did not observe $property matching $pattern on $window" >&2
    return 1
}

start_nobox "$test_dir/first-wm.log"
launch_client session-one first 80 90
first_window=$launched_window
launch_client session-two second 420 120
second_window=$launched_window
launch_client duplicate-session duplicate-a 40 410
duplicate_one=$launched_window
launch_client duplicate-session duplicate-b 420 410
duplicate_two=$launched_window

DISPLAY="$display" "$test_dir/request-pager" \
    geometry "$first_window" 0 xywh 310 230 330 190
DISPLAY="$display" "$test_dir/request-workspace" move "$first_window" 1
DISPLAY="$display" "$test_dir/request-workspace" current 1
DISPLAY="$display" "$test_dir/request-state" "$first_window" above add
DISPLAY="$display" "$test_dir/request-state" "$first_window" skip-taskbar add
DISPLAY="$display" "$test_dir/set-input-focus" "$first_window"
wait_for_property "$first_window" _NET_WM_DESKTOP '= 1$'
wait_for_property "$first_window" _NET_WM_STATE '_NET_WM_STATE_ABOVE'
wait_for_property "$first_window" _NET_WM_STATE '_NET_WM_STATE_SKIP_TASKBAR'

kill -TERM "$nobox_pid"
wait "$nobox_pid"
nobox_pid=
if [[ ! -s "$test_dir/session.toml" ]]; then
    echo "nobox did not persist session.toml" >&2
    exit 1
fi
for pattern in 'version = 1' 'session-one' 'focused = true' 'workspace = 1'; do
    if ! grep -q "$pattern" "$test_dir/session.toml"; then
        echo "saved session is missing '$pattern'" >&2
        cat "$test_dir/session.toml" >&2
        exit 1
    fi
done

DISPLAY="$display" "$test_dir/set-window-geometry" "$first_window" 20 30 180 90
DISPLAY="$display" "$test_dir/set-window-geometry" "$duplicate_one" 20 410 150 80
DISPLAY="$display" "$test_dir/set-window-geometry" "$duplicate_two" 500 410 170 90
DISPLAY="$display" xprop -id "$first_window" -remove _NET_WM_STATE
DISPLAY="$display" xprop -id "$first_window" -f _NET_WM_DESKTOP 32c \
    -set _NET_WM_DESKTOP 0

start_nobox "$test_dir/second-wm.log"
wait_for_property "$first_window" _NET_WM_DESKTOP '= 1$'
wait_for_property "$first_window" _NET_WM_STATE '_NET_WM_STATE_ABOVE'
wait_for_property "$first_window" _NET_WM_STATE '_NET_WM_STATE_SKIP_TASKBAR'
if ! DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP | grep -q '= 1$'; then
    echo "current workspace was not restored" >&2
    exit 1
fi
restored_size=$(DISPLAY="$display" xwininfo -id "$first_window" |
    awk '/Width:/ { width=$2 } /Height:/ { height=$2 } END { print width "x" height }')
if [[ "$restored_size" != 330x190 ]]; then
    echo "restored client size was $restored_size, expected 330x190" >&2
    exit 1
fi
absolute_position=$(DISPLAY="$display" xwininfo -id "$first_window" |
    awk '/Absolute upper-left X:/ { x=$4 } /Absolute upper-left Y:/ { y=$4 } END { print x "," y }')
if [[ "$absolute_position" != 310,230 ]]; then
    echo "restored client position was $absolute_position, expected 310,230" >&2
    exit 1
fi
duplicate_one_size=$(DISPLAY="$display" xwininfo -id "$duplicate_one" |
    awk '/Width:/ { width=$2 } /Height:/ { height=$2 } END { print width "x" height }')
duplicate_two_size=$(DISPLAY="$display" xwininfo -id "$duplicate_two" |
    awk '/Width:/ { width=$2 } /Height:/ { height=$2 } END { print width "x" height }')
if [[ "$duplicate_one_size" != 150x80 || "$duplicate_two_size" != 170x90 ]]; then
    echo "duplicate session IDs were restored ambiguously" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
    grep -qi "${first_window#0x}"; then
    echo "focused session client was not restored" >&2
    exit 1
fi

kill -TERM "$nobox_pid"
wait "$nobox_pid"
nobox_pid=
echo "X11 session persistence and restore checks passed"
