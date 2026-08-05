#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-geometry-actions.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 relative-actions test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pid=
obstacle_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$obstacle_pid" ]]; then kill "$obstacle_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
cc "$source_dir/request-pager.c" -o "$test_dir/request-pager" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$source_dir/set-fixed-size.c" -o "$test_dir/set-fixed-size" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for relative-actions tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<'EOF'
[theme]
border_width = 0
titlebar_height = 0

[[keyboard.bindings]]
key = "W-F5"
action = { type = "move_relative", x = 50, y = -20 }

[[keyboard.bindings]]
key = "W-F6"
action = { type = "move_relative", x = "10%", y = "10%" }

[[keyboard.bindings]]
key = "W-F7"
action = { type = "resize_relative", left = 20, right = 30, top = 10, bottom = 40 }

[[keyboard.bindings]]
key = "W-F8"
action = { type = "resize_relative", left = "-10%", bottom = "-20%" }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "move_relative", x = -10000, y = -10000 }

[[keyboard.bindings]]
key = "W-F10"
action = { type = "move_to_edge", direction = "right" }

[[keyboard.bindings]]
key = "W-F11"
action = { type = "move_to_edge", direction = "left" }

[[keyboard.bindings]]
key = "W-F12"
action = { type = "grow_to_edge", direction = "right" }

[[keyboard.bindings]]
key = "W-F1"
action = { type = "shrink_to_edge", direction = "right" }

[[keyboard.bindings]]
key = "W-F2"
action = { type = "grow_to_fill" }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "move_resize_to", x = "center", y = "-10%", width = "50%", height = "1/2" }

[[keyboard.bindings]]
key = "W-F4"
action = { type = "move_to_center" }
EOF

display=
for number in $(seq 571 590); do
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
if ! DISPLAY="$display" xdpyinfo | grep -q 'XTEST'; then
    echo "SKIP: the nested X server does not provide the XTest extension"
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

DISPLAY="$display" "$test_dir/presentation-client" --title relative-actions \
    >"$test_dir/client.window" 2>"$test_dir/client.log" &
client_pid=$!
window=
for _ in $(seq 1 50); do
    window=$(head -n 1 "$test_dir/client.window" 2>/dev/null || true)
    if [[ -n "$window" ]] && DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
        grep -qi "window id # $window"; then break; fi
    sleep 0.1
done
if [[ -z "$window" ]]; then
    echo "relative-actions client did not map" >&2
    exit 1
fi

window_geometry_for() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$4 }
        /Absolute upper-left Y:/ { y=$4 }
        /Width:/ { width=$2 }
        /Height:/ { height=$2 }
        END { print x, y, width, height }'
}

assert_window_geometry() {
    local target_window=$1
    local expected=$2
    local operation=$3
    local observed=
    for _ in $(seq 1 50); do
        observed=$(window_geometry_for "$target_window")
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "$operation produced '$observed', expected '$expected'" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

assert_geometry() {
    local expected=$1
    local operation=$2
    assert_window_geometry "$window" "$expected" "$operation"
}

set_geometry() {
    DISPLAY="$display" "$test_dir/request-pager" geometry "$window" 1 xywh \
        "$1" "$2" "$3" "$4"
    assert_geometry "$1 $2 $3 $4" 'geometry reset'
}

set_geometry 100 100 200 100
DISPLAY="$display" "$test_dir/press-key" F5
assert_geometry '150 80 200 100' 'pixel-relative move'
DISPLAY="$display" "$test_dir/press-key" F6
assert_geometry '230 140 200 100' 'work-area-relative move'
DISPLAY="$display" "$test_dir/press-key" F9
assert_geometry '0 0 200 100' 'offscreen move clamping'

set_geometry 100 100 200 100
DISPLAY="$display" "$test_dir/press-key" F7
assert_geometry '80 90 250 150' 'edge-relative resize'
DISPLAY="$display" "$test_dir/press-key" F8
assert_geometry '105 90 225 120' 'client-size-relative resize'

DISPLAY="$display" "$test_dir/presentation-client" --title edge-obstacle \
    >"$test_dir/obstacle.window" 2>"$test_dir/obstacle.log" &
obstacle_pid=$!
obstacle_window=
for _ in $(seq 1 50); do
    obstacle_window=$(head -n 1 "$test_dir/obstacle.window" 2>/dev/null || true)
    if [[ -n "$obstacle_window" ]] && DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -qi "$obstacle_window"; then break; fi
    sleep 0.1
done
if [[ -z "$obstacle_window" ]]; then
    echo "edge obstacle did not map" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-pager" geometry "$obstacle_window" 1 xywh \
    400 100 100 200
assert_window_geometry "$obstacle_window" '400 100 100 200' 'obstacle placement'
set_geometry 250 150 100 100
DISPLAY="$display" "$test_dir/request-activation" "$window"
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
        grep -qi "window id # $window"; then break; fi
    sleep 0.05
done

DISPLAY="$display" "$test_dir/press-key" F10
assert_geometry '300 150 100 100' 'move to obstacle near edge'
DISPLAY="$display" "$test_dir/press-key" F10
assert_geometry '500 150 100 100' 'move across obstacle far edge'
DISPLAY="$display" "$test_dir/press-key" F11
assert_geometry '300 150 100 100' 'move back to obstacle near edge'
DISPLAY="$display" "$test_dir/press-key" F11
assert_geometry '0 150 100 100' 'move from obstacle to work-area edge'

set_geometry 250 150 100 100
DISPLAY="$display" "$test_dir/press-key" F12
assert_geometry '250 150 150 100' 'grow to obstacle near edge'
DISPLAY="$display" "$test_dir/press-key" F12
assert_geometry '250 150 250 100' 'grow across obstacle far edge'
set_geometry 250 150 550 100
DISPLAY="$display" "$test_dir/press-key" F12
assert_geometry '400 150 400 100' 'blocked grow falls back to opposite-edge shrink'

DISPLAY="$display" "$test_dir/request-pager" geometry "$obstacle_window" 1 xywh \
    300 100 20 200
assert_window_geometry "$obstacle_window" '300 100 20 200' 'shrink obstacle placement'
set_geometry 250 150 200 100
DISPLAY="$display" "$test_dir/press-key" F1
assert_geometry '300 150 150 100' 'shrink toward obstacle edge'

DISPLAY="$display" "$test_dir/request-pager" geometry "$obstacle_window" 1 xywh \
    400 100 100 200
assert_window_geometry "$obstacle_window" '400 100 100 200' 'fill obstacle placement'
set_geometry 300 200 100 100
DISPLAY="$display" "$test_dir/press-key" F2
assert_geometry '0 0 400 600' 'grow to fill around one blocked edge'

DISPLAY="$display" "$test_dir/press-key" F3
assert_geometry '200 240 400 300' 'absolute fractional move and resize'
set_geometry 100 80 200 100
DISPLAY="$display" "$test_dir/press-key" F4
assert_geometry '300 250 200 100' 'move to work-area center'
DISPLAY="$display" "$test_dir/set-fixed-size" "$window"
sleep 0.1
DISPLAY="$display" "$test_dir/press-key" F3
assert_geometry '300 440 200 100' 'fixed-size absolute move ignores requested resize'

echo "X11 relative and directional geometry actions passed on $display"
