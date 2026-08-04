#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-net-moveresize.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 interactive moveresize test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 interactive moveresize test"
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

cc "$(dirname "$0")/placement-client.c" -o "$test_dir/placement-client" -lX11
cc "$(dirname "$0")/set-fixed-size.c" -o "$test_dir/set-fixed-size" -lX11
if ! cc "$(dirname "$0")/net-moveresize.c" -o "$test_dir/net-moveresize" \
    -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for interactive moveresize tests"
    exit 77
fi
printf '%s\n' \
    '[focus]' \
    'focus_new = false' \
    'follow_mouse = false' \
    'raise_on_focus = false' \
    '' \
    '[mouse]' \
    'edge_resistance = 0' >"$test_dir/config.toml"

display=
for number in $(seq 391 410); do
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
    if DISPLAY="$display" xprop -root _NET_SUPPORTED 2>/dev/null |
        grep -q '_NET_WM_MOVERESIZE'; then break; fi
    sleep 0.1
done
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED |
    grep -q '_NET_WM_MOVERESIZE'; then
    echo "nobox did not advertise client-initiated interactive moveresize" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/placement-client" moveresize positioned \
    >"$test_dir/client.log" 2>&1 &
client_pid=$!
window=
for _ in $(seq 1 50); do
    window=$(awk 'NR == 1 { print; exit }' "$test_dir/client.log" 2>/dev/null || true)
    if [[ -n "$window" ]] && DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS \
        2>/dev/null | grep -q '= '; then break; fi
    sleep 0.1
done
if [[ -z "$window" ]]; then
    echo "interactive moveresize client did not map" >&2
    exit 1
fi

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$4 }
        /Absolute upper-left Y:/ { y=$4 }
        /Width:/ { width=$2 }
        /Height:/ { height=$2 }
        END { print x, y, width, height }'
}

assert_geometry() {
    local expected=$1
    local operation=$2
    local observed
    for _ in $(seq 1 50); do
        observed=$(window_geometry "$window")
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "$operation produced '$observed', expected '$expected'" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

read -r x y width height < <(window_geometry "$window")
DISPLAY="$display" "$test_dir/net-moveresize" "$window" pointer-move
x=$((x + 48))
y=$((y + 32))
assert_geometry "$x $y $width $height" 'pointer move'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" pointer-resize
width=$((width + 40))
height=$((height + 24))
assert_geometry "$x $y $width $height" 'pointer resize'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" pointer-cancel
assert_geometry "$x $y $width $height" 'explicit pointer cancellation'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" keyboard-move
x=$((x + 16))
y=$((y + 8))
assert_geometry "$x $y $width $height" 'keyboard move'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" keyboard-resize
width=$((width + 8))
height=$((height + 8))
assert_geometry "$x $y $width $height" 'keyboard resize'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" keyboard-cancel
assert_geometry "$x $y $width $height" 'Escape keyboard cancellation'

DISPLAY="$display" "$test_dir/net-moveresize" "$window" keyboard-fine
x=$((x + 1))
y=$((y + 1))
assert_geometry "$x $y $width $height" 'fine-grained keyboard move'

root_width=$(DISPLAY="$display" xwininfo -root | awk '/Width:/ { print $2; exit }')
right_extent=$(DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS |
    awk -F'= ' '{ split($2, values, ","); gsub(/ /, "", values[2]); print values[2] }')
DISPLAY="$display" "$test_dir/net-moveresize" "$window" keyboard-edge
x=$((root_width - right_extent - width))
assert_geometry "$x $y $width $height" 'keyboard work-area edge jump'

DISPLAY="$display" "$test_dir/set-fixed-size" "$window"
for _ in $(seq 1 50); do
    if ! DISPLAY="$display" xprop -id "$window" _NET_WM_ALLOWED_ACTIONS |
        grep -q '_NET_WM_ACTION_RESIZE'; then break; fi
    sleep 0.05
done
DISPLAY="$display" "$test_dir/net-moveresize" "$window" pointer-resize
assert_geometry "$x $y $width $height" 'capability-rejected resize'

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during client-initiated interactive operations" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
echo "X11 client-initiated interactive moveresize checks passed"
