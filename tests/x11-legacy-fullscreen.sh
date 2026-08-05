#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-legacy-fullscreen.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: x11-legacy-fullscreen.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 legacy-fullscreen test"
        exit 77
    fi
done
if [[ ! -f "$openbox_source/tests/oldfullscreen.c" ]]; then
    echo "SKIP: Openbox oldfullscreen fixture is unavailable"
    exit 77
fi
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
legacy_pid=
normal_pid=
cleanup() {
    if [[ -n "$normal_pid" ]]; then kill "$normal_pid" 2>/dev/null || true; fi
    if [[ -n "$legacy_pid" ]]; then kill "$legacy_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$openbox_source/tests/oldfullscreen.c" -o "$test_dir/oldfullscreen" -lX11
cc "$(dirname "$0")/decoration-client.c" -o "$test_dir/normal-client" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$(dirname "$0")/request-state.c" -o "$test_dir/request-state" -lX11
cc "$(dirname "$0")/request-maximize.c" -o "$test_dir/request-maximize" -lX11
cc "$(dirname "$0")/set-window-geometry.c" -o "$test_dir/set-window-geometry" -lX11

printf '%s\n' \
    '[focus]' \
    'focus_new = false' \
    'follow_mouse = false' \
    'raise_on_focus = true' >"$test_dir/config.toml"

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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done

client_list() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' || true
}

wait_for_client_count() {
    local expected=$1
    local count=0
    for _ in $(seq 1 50); do
        count=$(client_list | wc -l)
        if (( count == expected )); then return 0; fi
        sleep 0.05
    done
    echo "managed client count was $count, expected $expected" >&2
    return 1
}

stacking_top() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' | tail -n 1 || true
}

wait_for_top() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(stacking_top)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "stacking top was '$observed', expected '$1'" >&2
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING >&2
    return 1
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$NF }
        /Absolute upper-left Y:/ { y=$NF }
        /^  Width:/ { w=$NF }
        /^  Height:/ { h=$NF }
        END { print x "," y "-" w "x" h }'
}

wait_for_geometry() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(window_geometry "$window")
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "geometry for $window was $observed, expected $expected" >&2
    return 1
}

wait_for_state() {
    local window=$1
    local atom=$2
    local expected=$3
    local state=
    for _ in $(seq 1 50); do
        state=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE 2>/dev/null || true)
        if [[ "$expected" == yes && "$state" == *"$atom"* ]]; then return 0; fi
        if [[ "$expected" == no && "$state" != *"$atom"* ]]; then return 0; fi
        sleep 0.05
    done
    echo "unexpected $atom state for $window: $state" >&2
    return 1
}

DISPLAY="$display" "$test_dir/oldfullscreen" >"$test_dir/legacy.log" 2>&1 &
legacy_pid=$!
wait_for_client_count 1
legacy_window=$(client_list | head -n 1)
wait_for_geometry "$legacy_window" '0,0-800x600'
if ! DISPLAY="$display" xprop -id "$legacy_window" _NET_FRAME_EXTENTS | grep -q '= 0, 0, 0, 0'; then
    echo "legacy fullscreen client was unexpectedly decorated" >&2
    exit 1
fi
wait_for_state "$legacy_window" _NET_WM_STATE_FULLSCREEN no

DISPLAY="$display" "$test_dir/normal-client" >"$test_dir/normal.log" 2>&1 &
normal_pid=$!
wait_for_client_count 2
normal_window=$(client_list | tail -n 1)
DISPLAY="$display" "$test_dir/request-state" "$normal_window" above add
wait_for_state "$normal_window" _NET_WM_STATE_ABOVE yes

# With no same-output competitor focused, exact coverage outranks Above.
DISPLAY="$display" "$test_dir/request-activation" "$legacy_window"
wait_for_top "$legacy_window"

# Focusing another client on that output demotes legacy coverage below Above.
DISPLAY="$display" "$test_dir/request-activation" "$normal_window"
wait_for_top "$normal_window"
DISPLAY="$display" "$test_dir/request-activation" "$legacy_window"
wait_for_top "$legacy_window"

# Managed maximize is distinct and suppresses the compatibility promotion.
DISPLAY="$display" "$test_dir/request-maximize" "$legacy_window" add
wait_for_state "$legacy_window" _NET_WM_STATE_MAXIMIZED_HORZ yes
wait_for_state "$legacy_window" _NET_WM_STATE_MAXIMIZED_VERT yes
wait_for_top "$normal_window"
DISPLAY="$display" "$test_dir/request-maximize" "$legacy_window" remove
wait_for_state "$legacy_window" _NET_WM_STATE_MAXIMIZED_HORZ no
wait_for_state "$legacy_window" _NET_WM_STATE_MAXIMIZED_VERT no
wait_for_geometry "$legacy_window" '0,0-800x600'
wait_for_top "$legacy_window"

# Client-controlled geometry exits and re-enters coverage without restore state.
DISPLAY="$display" "$test_dir/set-window-geometry" "$legacy_window" 100 100 320 240
wait_for_geometry "$legacy_window" '100,100-320x240'
wait_for_top "$normal_window"
DISPLAY="$display" "$test_dir/set-window-geometry" "$legacy_window" 0 0 800 600
wait_for_geometry "$legacy_window" '0,0-800x600'
wait_for_top "$legacy_window"
wait_for_state "$legacy_window" _NET_WM_STATE_FULLSCREEN no

DISPLAY="$display" "$test_dir/request-activation" "$normal_window"
wait_for_top "$normal_window"
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during legacy-fullscreen compatibility checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "Openbox legacy-fullscreen compatibility passed on $display"
