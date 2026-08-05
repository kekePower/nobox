#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-shading.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 shading test"
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

source_dir=$(dirname "$0")
cc "$source_dir/shade-client.c" -o "$test_dir/shade-client" -lX11
cc "$source_dir/request-state.c" -o "$test_dir/request-state" -lX11
cc "$source_dir/request-geometry.c" -o "$test_dir/request-geometry" -lX11
cc "$source_dir/set-decoration-policy.c" -o "$test_dir/set-decoration-policy" -lX11

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

DISPLAY="$display" "$test_dir/shade-client" initial >"$test_dir/window" 2>&1 &
client_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/window" ]]; then break; fi
    sleep 0.05
done
read -r window <"$test_dir/window"
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null | grep -qi "$window"; then
        break
    fi
    sleep 0.05
done
frame=$(DISPLAY="$display" xwininfo -tree -id "$window" |
    awk '/Parent window id:/ { print $4; exit }')

map_state() {
    DISPLAY="$display" xwininfo -id "$1" 2>/dev/null |
        awk -F: '/Map State:/ { gsub(/ /, "", $2); print $2; exit }'
}

frame_size() {
    DISPLAY="$display" xwininfo -id "$frame" | awk '
        /^  Width:/ { w=$NF }
        /^  Height:/ { h=$NF }
        END { print w "x" h }'
}

wait_for_state() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present && "$observed" == *'_NET_WM_STATE_SHADED'* ]]; then return 0; fi
        if [[ "$expected" == absent && "$observed" != *'_NET_WM_STATE_SHADED'* ]]; then return 0; fi
        sleep 0.05
    done
    echo "unexpected shaded state: $observed" >&2
    return 1
}

wait_for_frame_size() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(frame_size)
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "frame size was $observed, expected $expected" >&2
    return 1
}

wait_for_state present
wait_for_frame_size 360x24
if [[ "$(map_state "$window")" != IsUnMapped ]]; then
    echo "initially shaded client content was mapped" >&2
    exit 1
fi
supported=$(DISPLAY="$display" xprop -root _NET_SUPPORTED)
allowed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_ALLOWED_ACTIONS)
for atom in _NET_WM_STATE_SHADED _NET_WM_ACTION_SHADE; do
    if ! grep -q "$atom" <<<"$supported $allowed"; then
        echo "$atom was not published" >&2
        exit 1
    fi
done

DISPLAY="$display" "$test_dir/request-state" "$window" shade remove
wait_for_state absent
wait_for_frame_size 360x144
if [[ "$(map_state "$window")" != IsViewable ]]; then
    echo "unshaded client content was not viewable" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-state" "$window" shade add
wait_for_state present
wait_for_frame_size 360x24
DISPLAY="$display" "$test_dir/request-geometry" "$window"
wait_for_frame_size 320x24
DISPLAY="$display" "$test_dir/request-state" "$window" shade remove
wait_for_state absent
wait_for_frame_size 320x264
geometry=$(DISPLAY="$display" xwininfo -id "$window" | awk '
    /Absolute upper-left X:/ { x=$NF }
    /Absolute upper-left Y:/ { y=$NF }
    /^  Width:/ { w=$NF }
    /^  Height:/ { h=$NF }
    END { print x "," y "-" w "x" h }')
if [[ "$geometry" != '100,100-320x240' ]]; then
    echo "shade round trip lost requested content geometry: $geometry" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-state" "$window" shade add
wait_for_state present
DISPLAY="$display" "$test_dir/request-state" "$window" fullscreen add
wait_for_state absent
if [[ "$(map_state "$window")" != IsViewable ]]; then
    echo "fullscreen did not unshade client content" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-state" "$window" fullscreen remove

DISPLAY="$display" "$test_dir/set-decoration-policy" "$window" motif-none
for _ in $(seq 1 50); do
    allowed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_ALLOWED_ACTIONS)
    if [[ "$allowed" != *'_NET_WM_ACTION_SHADE'* ]]; then break; fi
    sleep 0.05
done
DISPLAY="$display" "$test_dir/request-state" "$window" shade add
sleep 0.1
wait_for_state absent

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during shading checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "Initial/live X11 shading and exact content restoration passed on $display"
