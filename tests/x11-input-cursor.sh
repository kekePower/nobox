#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-input-cursor.sh /path/to/nobox}
for dependency in cc pkg-config xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 input-cursor test"
        exit 77
    fi
done
if ! pkg-config --exists xfixes; then
    echo "SKIP: XFixes development files are required for the X11 input-cursor test"
    exit 77
fi
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

cc "$(dirname "$0")/cursor-input-client.c" -o "$test_dir/cursor-input-client" -lX11
cc "$(dirname "$0")/cursor-image.c" -o "$test_dir/cursor-image" \
    $(pkg-config --cflags --libs xfixes) -lX11
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
root=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')
server_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$root" 10 10)
DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done
nobox_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$root" 10 10)
if [[ "$nobox_cursor" == "$server_cursor" ]]; then
    echo "nobox did not replace the X server's default root cursor: $nobox_cursor" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/cursor-input-client" >"$test_dir/client.windows" 2>&1 &
client_pid=$!
top=
child=
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/client.windows" ]]; then
        read -r top child <"$test_dir/client.windows"
    fi
    if [[ -n "$top" ]] &&
        DISPLAY="$display" xprop -id "$top" _NET_FRAME_EXTENTS >/dev/null 2>&1; then
        break
    fi
    sleep 0.05
done
if [[ -z "$top" || -z "$child" ]]; then
    echo "input-only cursor client did not map" >&2
    exit 1
fi

parent=$(DISPLAY="$display" xwininfo -id "$child" -tree |
    awk '/Parent window id:/ { print $4; exit }')
if [[ "${parent,,}" != "${top,,}" ]]; then
    echo "input-only child parent changed from $top to $parent" >&2
    exit 1
fi

frame=$(DISPLAY="$display" xwininfo -id "$top" -tree |
    awk '/Parent window id:/ { print $4; exit }')
left_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$frame" 1 60)
corner_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$frame" 4 4)
if [[ "$left_cursor" == "$nobox_cursor" || "$corner_cursor" == "$nobox_cursor" ||
      "$left_cursor" == "$corner_cursor" ]]; then
    echo "resize handles did not expose distinct directional cursors" >&2
    exit 1
fi

top_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$top" 10 10)
child_cursor=$(DISPLAY="$display" "$test_dir/cursor-image" "$child" 120 45)
if [[ "$top_cursor" == "$child_cursor" ]]; then
    echo "input-only child's watch cursor was not selected: $child_cursor" >&2
    exit 1
fi

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited while testing an input-only cursor child" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 input-only child cursor preservation passed on $display"
