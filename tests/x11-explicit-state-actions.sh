#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-explicit-state-actions.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 explicit-state-actions test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 explicit-state-actions test"
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

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for explicit-state-actions tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<'EOF'
[focus]
raise_on_focus = false

[theme]
border_width = 2
titlebar_height = 24

[[keyboard.bindings]]
key = "W-F1"
action = { type = "maximize" }

[[keyboard.bindings]]
key = "W-F2"
action = { type = "unmaximize", direction = "horizontal" }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "maximize", direction = "horizontal" }

[[keyboard.bindings]]
key = "W-F4"
action = { type = "unmaximize" }

[[keyboard.bindings]]
key = "W-F5"
action = { type = "undecorate" }

[[keyboard.bindings]]
key = "W-F6"
action = { type = "decorate" }

[[keyboard.bindings]]
key = "W-F7"
action = { type = "shade" }

[[keyboard.bindings]]
key = "W-F8"
action = { type = "unshade" }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "send_to_layer", layer = "above" }

[[keyboard.bindings]]
key = "W-F10"
action = { type = "send_to_layer", layer = "normal" }

[[keyboard.bindings]]
key = "W-F11"
action = { type = "send_to_layer", layer = "below" }
EOF

display=
for number in $(seq 631 650); do
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

DISPLAY="$display" "$test_dir/presentation-client" --title explicit-state-actions \
    >"$test_dir/window" 2>"$test_dir/client.log" &
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
DISPLAY="$display" "$test_dir/request-activation" "$window"

wait_for_state() {
    local atom=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present && "$observed" == *"$atom"* ]]; then return 0; fi
        if [[ "$expected" == absent && "$observed" != *"$atom"* ]]; then return 0; fi
        sleep 0.05
    done
    echo "$atom was not $expected for $window: $observed" >&2
    return 1
}

wait_for_extents() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS)
        if [[ "$expected" == zero && "$observed" == *'= 0, 0, 0, 0'* ]]; then return 0; fi
        if [[ "$expected" == present && "$observed" != *'= 0, 0, 0, 0'* ]]; then return 0; fi
        sleep 0.05
    done
    echo "frame extents were not $expected for $window: $observed" >&2
    return 1
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$window" | awk '
        /Absolute upper-left X:/ { x=$NF }
        /Absolute upper-left Y:/ { y=$NF }
        /^  Width:/ { w=$NF }
        /^  Height:/ { h=$NF }
        END { print x "," y "-" w "x" h }'
}

press() {
    DISPLAY="$display" "$test_dir/press-key" "$1"
}

press F1
wait_for_state _NET_WM_STATE_MAXIMIZED_HORZ present
wait_for_state _NET_WM_STATE_MAXIMIZED_VERT present
maximized_geometry=$(window_geometry)
press F1
if [[ "$(window_geometry)" != "$maximized_geometry" ]]; then
    echo "repeated explicit maximize changed geometry" >&2
    exit 1
fi

press F2
wait_for_state _NET_WM_STATE_MAXIMIZED_HORZ absent
wait_for_state _NET_WM_STATE_MAXIMIZED_VERT present
press F3
wait_for_state _NET_WM_STATE_MAXIMIZED_HORZ present
wait_for_state _NET_WM_STATE_MAXIMIZED_VERT present
press F4
wait_for_state _NET_WM_STATE_MAXIMIZED_HORZ absent
wait_for_state _NET_WM_STATE_MAXIMIZED_VERT absent

wait_for_extents present
press F7
wait_for_state _NET_WM_STATE_SHADED present
press F5
wait_for_state _NET_WM_STATE_SHADED absent
wait_for_extents zero
press F5
wait_for_extents zero
press F6
wait_for_extents present
press F7
wait_for_state _NET_WM_STATE_SHADED present
press F7
wait_for_state _NET_WM_STATE_SHADED present
press F8
wait_for_state _NET_WM_STATE_SHADED absent

press F9
wait_for_state _NET_WM_STATE_ABOVE present
wait_for_state _NET_WM_STATE_BELOW absent
press F9
wait_for_state _NET_WM_STATE_ABOVE present
press F10
wait_for_state _NET_WM_STATE_ABOVE absent
wait_for_state _NET_WM_STATE_BELOW absent
press F11
wait_for_state _NET_WM_STATE_BELOW present
wait_for_state _NET_WM_STATE_ABOVE absent
press F10
wait_for_state _NET_WM_STATE_ABOVE absent
wait_for_state _NET_WM_STATE_BELOW absent

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during explicit state-action checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 explicit maximize, decoration, shade, and layer actions passed on $display"
