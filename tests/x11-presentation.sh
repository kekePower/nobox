#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-presentation.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 presentation test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

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

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
cc "$source_dir/request-state.c" -o "$test_dir/request-state" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$source_dir/request-iconic.c" -o "$test_dir/request-iconic" -lX11
cc "$source_dir/set-urgency.c" -o "$test_dir/set-urgency" -lX11
cc "$source_dir/set-fixed-size.c" -o "$test_dir/set-fixed-size" -lX11
cc "$source_dir/request-pager.c" -o "$test_dir/request-pager" -lX11
cc "$source_dir/request-fullscreen-monitors.c" \
    -o "$test_dir/request-fullscreen-monitors" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for presentation tests"
    exit 77
fi

display=
for number in $(seq 251 270); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

supported=$(DISPLAY="$display" xprop -root _NET_SUPPORTED)
for atom in _NET_CLOSE_WINDOW _NET_MOVERESIZE_WINDOW _NET_WM_FULLSCREEN_MONITORS \
    _NET_WM_STATE_SKIP_TASKBAR _NET_WM_STATE_SKIP_PAGER \
    _NET_WM_STATE_DEMANDS_ATTENTION _NET_WM_STATE_HIDDEN \
    _NET_WM_STATE_FOCUSED _NET_WM_ALLOWED_ACTIONS _NET_WM_ACTION_MOVE \
    _NET_WM_ACTION_RESIZE _NET_WM_ACTION_MINIMIZE \
    _NET_WM_ACTION_MAXIMIZE_HORZ _NET_WM_ACTION_MAXIMIZE_VERT \
    _NET_WM_ACTION_FULLSCREEN _NET_WM_ACTION_CHANGE_DESKTOP \
    _NET_WM_ACTION_CLOSE _NET_WM_ACTION_ABOVE _NET_WM_ACTION_BELOW; do
    if ! grep -q "$atom" <<<"$supported"; then
        echo "_NET_SUPPORTED omitted $atom" >&2
        exit 1
    fi
done

launch_client() {
    local name=$1
    shift
    DISPLAY="$display" "$test_dir/presentation-client" --title "$name" "$@" \
        >"$test_dir/$name.window" 2>"$test_dir/$name.log" &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 40); do
        if [[ -s "$test_dir/$name.window" ]]; then
            launched_window=$(head -n 1 "$test_dir/$name.window")
            if DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
                grep -qi "$launched_window"; then
                return 0
            fi
        fi
        sleep 0.05
    done
    echo "client $name was not managed" >&2
    return 1
}

wait_for_active() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)
        if grep -qi "window id # $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $expected" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

wait_for_top() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
            grep -o '0x[0-9a-fA-F]*' | tail -n 1)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "top client was $observed, expected $expected" >&2
    return 1
}

wait_for_state() {
    local window=$1
    local atom=$2
    local expected=$3
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present ]] && grep -q "$atom" <<<"$observed"; then return 0; fi
        if [[ "$expected" == absent ]] && ! grep -q "$atom" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$atom was unexpectedly $expected for $window: $observed" >&2
    return 1
}

wait_for_action() {
    local window=$1
    local atom=$2
    local expected=$3
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_ALLOWED_ACTIONS)
        if [[ "$expected" == present ]] && grep -q "$atom" <<<"$observed"; then return 0; fi
        if [[ "$expected" == absent ]] && ! grep -q "$atom" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$atom was unexpectedly $expected for $window: $observed" >&2
    return 1
}

wait_for_geometry() {
    local window=$1
    local expected="$2,$3 ${4}x${5}"
    local observed=
    for _ in $(seq 1 40); do
        local info
        info=$(DISPLAY="$display" xwininfo -id "$window")
        local x y width height
        x=$(awk -F: '/Absolute upper-left X:/ { gsub(/ /, "", $2); print $2 }' <<<"$info")
        y=$(awk -F: '/Absolute upper-left Y:/ { gsub(/ /, "", $2); print $2 }' <<<"$info")
        width=$(awk -F: '/Width:/ { gsub(/ /, "", $2); print $2; exit }' <<<"$info")
        height=$(awk -F: '/Height:/ { gsub(/ /, "", $2); print $2; exit }' <<<"$info")
        observed="$x,$y ${width}x${height}"
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "geometry was $observed, expected $expected" >&2
    return 1
}

launched_window=
launch_client presentation-one
first_window=$launched_window
launch_client presentation-skipped --skip-taskbar --skip-pager
skipped_window=$launched_window
launch_client presentation-three
third_window=$launched_window
wait_for_active "$third_window"
wait_for_state "$third_window" _NET_WM_STATE_FOCUSED present
wait_for_state "$first_window" _NET_WM_STATE_FOCUSED absent
for action in _NET_WM_ACTION_CHANGE_DESKTOP _NET_WM_ACTION_MOVE \
    _NET_WM_ACTION_RESIZE _NET_WM_ACTION_MINIMIZE \
    _NET_WM_ACTION_MAXIMIZE_HORZ _NET_WM_ACTION_MAXIMIZE_VERT \
    _NET_WM_ACTION_FULLSCREEN _NET_WM_ACTION_CLOSE \
    _NET_WM_ACTION_ABOVE _NET_WM_ACTION_BELOW; do
    wait_for_action "$third_window" "$action" present
done
DISPLAY="$display" xprop -id "$third_window" -remove _NET_WM_ALLOWED_ACTIONS
wait_for_action "$third_window" _NET_WM_ACTION_MOVE present

DISPLAY="$display" "$test_dir/request-fullscreen-monitors" "$third_window" 0 0 0 0
for _ in $(seq 1 40); do
    fullscreen_monitors=$(DISPLAY="$display" xprop -id "$third_window" \
        _NET_WM_FULLSCREEN_MONITORS)
    if grep -q '= 0, 0, 0, 0' <<<"$fullscreen_monitors"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 0, 0, 0' <<<"$fullscreen_monitors"; then
    echo "fullscreen monitor request was not published: $fullscreen_monitors" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-fullscreen-monitors" "$third_window" 99 99 99 99
sleep 0.1
fullscreen_monitors=$(DISPLAY="$display" xprop -id "$third_window" \
    _NET_WM_FULLSCREEN_MONITORS)
if ! grep -q '= 0, 0, 0, 0' <<<"$fullscreen_monitors"; then
    echo "invalid fullscreen monitor request replaced valid state: $fullscreen_monitors" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-state" "$third_window" fullscreen add
wait_for_state "$third_window" _NET_WM_STATE_FULLSCREEN present
wait_for_geometry "$third_window" 0 0 800 600
for action in _NET_WM_ACTION_RESIZE _NET_WM_ACTION_MAXIMIZE_HORZ \
    _NET_WM_ACTION_MAXIMIZE_VERT _NET_WM_ACTION_ABOVE _NET_WM_ACTION_BELOW; do
    wait_for_action "$third_window" "$action" absent
done
wait_for_action "$third_window" _NET_WM_ACTION_MOVE present
wait_for_action "$third_window" _NET_WM_ACTION_FULLSCREEN present
launch_client nobox-presentation-over-fullscreen
over_fullscreen_window=$launched_window
over_fullscreen_pid=${client_pids[${#client_pids[@]}-1]}
DISPLAY="$display" "$test_dir/request-activation" "$over_fullscreen_window"
wait_for_active "$over_fullscreen_window"
wait_for_top "$over_fullscreen_window"
kill "$over_fullscreen_pid"
wait "$over_fullscreen_pid" 2>/dev/null || true
wait_for_active "$third_window"
DISPLAY="$display" "$test_dir/request-state" "$third_window" fullscreen remove
wait_for_state "$third_window" _NET_WM_STATE_FULLSCREEN absent
wait_for_action "$third_window" _NET_WM_ACTION_RESIZE present

DISPLAY="$display" "$test_dir/request-pager" geometry "$third_window" 1 xywh \
    140 150 350 210
wait_for_geometry "$third_window" 140 150 350 210
DISPLAY="$display" "$test_dir/request-pager" geometry "$third_window" 9 wh \
    0 0 400 240
wait_for_geometry "$third_window" 90 120 400 240
DISPLAY="$display" "$test_dir/request-pager" geometry "$third_window" 255 xywh \
    10 10 200 100
sleep 0.1
wait_for_geometry "$third_window" 90 120 400 240

DISPLAY="$display" "$test_dir/set-fixed-size" "$first_window"
wait_for_action "$first_window" _NET_WM_ACTION_RESIZE absent
wait_for_action "$first_window" _NET_WM_ACTION_MAXIMIZE_HORZ absent
wait_for_action "$first_window" _NET_WM_ACTION_MAXIMIZE_VERT absent
wait_for_action "$first_window" _NET_WM_ACTION_MOVE present

initial_state=$(DISPLAY="$display" xprop -id "$skipped_window" _NET_WM_STATE)
if ! grep -q _NET_WM_STATE_SKIP_TASKBAR <<<"$initial_state" \
    || ! grep -q _NET_WM_STATE_SKIP_PAGER <<<"$initial_state"; then
    echo "initial skip-taskbar/skip-pager hints were not retained: $initial_state" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$first_window"
wait_for_state "$first_window" _NET_WM_STATE_FOCUSED present
wait_for_state "$third_window" _NET_WM_STATE_FOCUSED absent

DISPLAY="$display" "$test_dir/request-state" "$first_window" focused remove
DISPLAY="$display" "$test_dir/request-state" "$third_window" focused add
DISPLAY="$display" "$test_dir/request-state" "$first_window" hidden add
sleep 0.1
wait_for_state "$first_window" _NET_WM_STATE_FOCUSED present
wait_for_state "$third_window" _NET_WM_STATE_FOCUSED absent
wait_for_state "$first_window" _NET_WM_STATE_HIDDEN absent

DISPLAY="$display" "$test_dir/request-iconic" "$first_window"
wait_for_state "$first_window" _NET_WM_STATE_HIDDEN present
wait_for_state "$first_window" _NET_WM_STATE_FOCUSED absent
DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_active "$first_window"
wait_for_state "$first_window" _NET_WM_STATE_HIDDEN absent
wait_for_state "$first_window" _NET_WM_STATE_FOCUSED present

DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-taskbar remove
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_TASKBAR absent
DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-taskbar toggle
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_TASKBAR present
DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-pager remove
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_PAGER absent

DISPLAY="$display" "$test_dir/request-state" "$third_window" attention add
wait_for_state "$third_window" _NET_WM_STATE_DEMANDS_ATTENTION present
DISPLAY="$display" "$test_dir/request-activation" "$third_window"
wait_for_active "$third_window"
wait_for_state "$third_window" _NET_WM_STATE_DEMANDS_ATTENTION absent

DISPLAY="$display" "$test_dir/set-urgency" "$skipped_window" on
for _ in $(seq 1 40); do
    if grep -q "skip_taskbar: true, skip_pager: false, urgent: true" \
        "$test_dir/nobox.log"; then break; fi
    sleep 0.05
done
if ! grep -q "skip_taskbar: true, skip_pager: false, urgent: true" \
    "$test_dir/nobox.log"; then
    echo "nobox did not observe the live ICCCM urgency hint" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
DISPLAY="$display" "$test_dir/request-activation" "$skipped_window"
wait_for_active "$skipped_window"
if ! DISPLAY="$display" xprop -id "$skipped_window" WM_HINTS | grep -qi urgency; then
    echo "nobox rewrote the client-owned ICCCM urgency hint" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/set-urgency" "$skipped_window" off
for _ in $(seq 1 40); do
    if grep -q "skip_taskbar: true, skip_pager: false, urgent: false" \
        "$test_dir/nobox.log"; then break; fi
    sleep 0.05
done
if ! grep -q "skip_taskbar: true, skip_pager: false, urgent: false" \
    "$test_dir/nobox.log"; then
    echo "nobox did not observe urgency being cleared" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-pager" close "$first_window"
for _ in $(seq 1 40); do
    if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -qi "$first_window"; then break; fi
    sleep 0.05
done
if DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$first_window"; then
    echo "_NET_CLOSE_WINDOW did not close $first_window" >&2
    exit 1
fi

echo "X11 taskbar, pager, lifecycle, and urgency semantics passed on $display"
