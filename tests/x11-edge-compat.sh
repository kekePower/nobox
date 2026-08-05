#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-edge-compat.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 edge compatibility test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

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

cc "$(dirname "$0")/edge-compat-client.c" -o "$test_dir/edge-compat-client" -lX11
cc "$(dirname "$0")/request-border.c" -o "$test_dir/request-border" -lX11
cc "$(dirname "$0")/request-state.c" -o "$test_dir/request-state" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11

display=
for number in $(seq 231 250); do
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

DISPLAY="$display" "$test_dir/edge-compat-client" >"$test_dir/windows" 2>&1 &
client_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/windows" ]]; then break; fi
    sleep 0.05
done
read -r override_window input_window parent_window child_window <"$test_dir/windows"

wait_for_managed() {
    local window=${1,,}
    local clients=
    for _ in $(seq 1 50); do
        clients=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null || true)
        if grep -qi "$window" <<<"$clients" &&
            DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS 2>/dev/null |
                grep -q '='; then
            return 0
        fi
        sleep 0.05
    done
    echo "window $window was not managed: $clients" >&2
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

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$NF }
        /Absolute upper-left Y:/ { y=$NF }
        /^  Width:/ { w=$NF }
        /^  Height:/ { h=$NF }
        END { print x "," y "-" w "x" h }'
}

wait_for_managed "$parent_window"
wait_for_managed "$child_window"
client_list=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST)
root_window=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')
for window in "$override_window" "$input_window"; do
    if grep -qi "$window" <<<"$client_list"; then
        echo "override-redirect window $window entered _NET_CLIENT_LIST" >&2
        exit 1
    fi
    parent=$(DISPLAY="$display" xwininfo -tree -id "$window" |
        awk '/Parent window id:/ { print $4; exit }')
    if [[ "${parent,,}" != "${root_window,,}" ]]; then
        echo "override-redirect window $window was reparented to $parent" >&2
        exit 1
    fi
    if DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS 2>/dev/null |
        grep -q '='; then
        echo "override-redirect window $window received frame extents" >&2
        exit 1
    fi
done
echo "Override-redirect input/output windows remained unmanaged on $display"

initial_geometry=$(window_geometry "$parent_window")
for width in 50 0; do
    DISPLAY="$display" "$test_dir/request-border" "$parent_window" "$width"
    for _ in $(seq 1 30); do
        border_width=$(DISPLAY="$display" xwininfo -id "$parent_window" |
            awk -F: '/Border width:/ { gsub(/ /, "", $2); print $2; exit }')
        if [[ "$border_width" == 0 ]]; then break; fi
        sleep 0.05
    done
    if [[ "$border_width" != 0 ]]; then
        echo "managed client retained requested border width $border_width" >&2
        exit 1
    fi
    if [[ "$(window_geometry "$parent_window")" != "$initial_geometry" ]]; then
        echo "border request changed framed content geometry" >&2
        exit 1
    fi
    extents=$(DISPLAY="$display" xprop -id "$parent_window" _NET_FRAME_EXTENTS)
    if ! grep -q '= 2, 2, 26, 2' <<<"$extents"; then
        echo "border request changed frame extents: $extents" >&2
        exit 1
    fi
done
echo "Managed client border requests preserved framed geometry on $display"

DISPLAY="$display" "$test_dir/request-activation" "$parent_window"
wait_for_active "$parent_window"
DISPLAY="$display" "$test_dir/request-state" "$child_window" modal add
wait_for_active "$child_window"
DISPLAY="$display" "$test_dir/request-activation" "$parent_window"
wait_for_active "$child_window"
DISPLAY="$display" "$test_dir/request-state" "$child_window" modal remove
DISPLAY="$display" "$test_dir/request-activation" "$parent_window"
wait_for_active "$parent_window"
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during edge compatibility checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "Live modal-state focus redirection passed on $display"
