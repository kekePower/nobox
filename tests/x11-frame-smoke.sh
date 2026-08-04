#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-frame-smoke.sh /path/to/nobox}
for dependency in xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 frame smoke test"
        exit 77
    fi
done
if command -v Xnest >/dev/null 2>&1; then
    x_server=(Xnest)
    x_server_args=(-geometry 800x600 -ac)
elif command -v Xephyr >/dev/null 2>&1; then
    x_server=(Xephyr)
    x_server_args=(-screen 800x600 -ac)
elif command -v Xvfb >/dev/null 2>&1; then
    x_server=(Xvfb)
    x_server_args=(-screen 0 800x600x24 -ac)
else
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 frame smoke test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 131 150); do
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

DISPLAY="$display" xterm -title nobox-adoption-probe -geometry 40x10+50+50 \
    >"$test_dir/client.log" 2>&1 &
client_pid=$!
client_window=
for _ in $(seq 1 30); do
    client_window=$(DISPLAY="$display" xwininfo -root -tree |
        awk '/"nobox-adoption-probe"/ { print $1; exit }')
    if [[ -n "$client_window" ]]; then break; fi
    sleep 0.1
done
root_window=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
framed=false
for _ in $(seq 1 40); do
    parent_window=$(DISPLAY="$display" xwininfo -tree -id "$client_window" |
        awk '/Parent window id:/ { print $4; exit }')
    frame_extents=$(DISPLAY="$display" xprop -id "$client_window" _NET_FRAME_EXTENTS)
    if [[ "$parent_window" != "$root_window" ]] && grep -q '= 2, 2, 26, 2' <<<"$frame_extents"; then
        framed=true
        break
    fi
    sleep 0.1
done
if [[ "$framed" != true ]]; then
    echo "nobox did not adopt the existing client into a frame" >&2
    exit 1
fi

kill -KILL "$nobox_pid"
wait "$nobox_pid" 2>/dev/null || true
nobox_pid=
restored=false
for _ in $(seq 1 40); do
    parent_window=$(DISPLAY="$display" xwininfo -tree -id "$client_window" |
        awk '/Parent window id:/ { print $4; exit }')
    map_state=$(DISPLAY="$display" xwininfo -id "$client_window" |
        awk -F: '/Map State:/ { gsub(/ /, "", $2); print $2; exit }')
    if [[ "$parent_window" == "$root_window" && "$map_state" == IsViewable ]]; then
        restored=true
        break
    fi
    sleep 0.1
done
if [[ "$restored" != true ]]; then
    echo "X save set did not restore the client after nobox terminated" >&2
    DISPLAY="$display" xwininfo -root -tree >&2 || true
    DISPLAY="$display" xwininfo -id "$client_window" >&2 || true
    echo "expected root=$root_window observed parent=${parent_window:-missing} state=${map_state:-missing}" >&2
    tail -n 40 "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 frame adoption and crash recovery passed on $display"
