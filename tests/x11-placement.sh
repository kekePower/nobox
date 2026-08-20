#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-placement.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 placement test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
dock_pid=
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$dock_pid" ]]; then kill "$dock_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/placement-client.c" -o "$test_dir/placement-client" -lX11
cc "$(dirname "$0")/strut-dock.c" -o "$test_dir/strut-dock" -lX11

display=
for number in $(seq 211 230); do
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
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED |
    grep -q '_NET_WM_FULL_PLACEMENT'; then
    echo "manager did not advertise complete initial placement policy" >&2
    exit 1
fi

launch_client() {
    local output=$1
    shift
    DISPLAY="$display" "$test_dir/placement-client" "$@" >"$test_dir/$output" 2>&1 &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 40); do
        if [[ -s "$test_dir/$output" ]]; then
            launched_window=$(head -n 1 "$test_dir/$output")
        fi
        if [[ -n "$launched_window" ]] && DISPLAY="$display" xprop -id "$launched_window" \
            _NET_FRAME_EXTENTS 2>/dev/null | grep -q '= 2, 2, 26, 2'; then
            return 0
        fi
        sleep 0.05
    done
    echo "placement client $* did not become managed" >&2
    return 1
}

assert_position() {
    local window=$1
    local expected_x=$2
    local expected_y=$3
    local info=
    local observed_x=
    local observed_y=
    for _ in $(seq 1 40); do
        info=$(DISPLAY="$display" xwininfo -id "$window")
        observed_x=$(awk '/Absolute upper-left X:/ { print $4; exit }' <<<"$info")
        observed_y=$(awk '/Absolute upper-left Y:/ { print $4; exit }' <<<"$info")
        if [[ "$observed_x" == "$expected_x" && "$observed_y" == "$expected_y" ]]; then
            return 0
        fi
        sleep 0.05
    done
    echo "window $window was at $observed_x,$observed_y; expected $expected_x,$expected_y" >&2
    echo "$info" >&2
    tail -n 60 "$test_dir/nobox.log" >&2 || true
    return 1
}

launched_window=
launch_client first.window nobox-placement-one normal
first_window=$launched_window
assert_position "$first_window" 300 262

launch_client second.window nobox-placement-two normal
second_window=$launched_window
assert_position "$second_window" 49 80

launch_client positioned.window nobox-placement-explicit positioned
positioned_window=$launched_window
assert_position "$positioned_window" 200 200

launch_client dialog.window nobox-placement-dialog dialog "$first_window"
dialog_window=$launched_window
assert_position "$dialog_window" 350 282

DISPLAY="$display" "$test_dir/strut-dock" >"$test_dir/strut-dock.log" 2>&1 &
dock_pid=$!
for _ in $(seq 1 40); do
    work_area=$(DISPLAY="$display" xprop -root _NET_WORKAREA 2>/dev/null || true)
    if grep -q '= 0, 30, 800, 570' <<<"$work_area"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 30, 800, 570' <<<"${work_area:-}"; then
    echo "placement strut did not establish the expected work area: ${work_area:-missing}" >&2
    exit 1
fi

launch_client origin.window nobox-placement-origin origin
origin_window=$launched_window
assert_position "$origin_window" 2 56

echo "X11 smart, explicit, parent-relative, and strut-safe origin placement passed on $display"
