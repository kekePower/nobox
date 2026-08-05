#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-configure-notify.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 ConfigureNotify test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 ConfigureNotify test"
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
cc "$source_dir/configure-notify-client.c" -o "$test_dir/configure-notify-client" -lX11
cc "$source_dir/configure-window.c" -o "$test_dir/configure-window" -lX11
cc "$source_dir/request-geometry.c" -o "$test_dir/request-geometry" -lX11
cc "$source_dir/request-maximize.c" -o "$test_dir/request-maximize" -lX11

display=
for number in $(seq 711 730); do
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

wait_for_synthetic() {
    local log=$1
    local expected=$2
    for _ in $(seq 1 50); do
        if grep -Fq "$expected" "$log"; then return 0; fi
        sleep 0.05
    done
    echo "missing synthetic ConfigureNotify: $expected" >&2
    cat "$log" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

wait_for_unmanaged() {
    local old_window=$1
    local clients=
    for _ in $(seq 1 50); do
        clients=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null || true)
        if ! grep -Fqi "$old_window" <<<"$clients"; then return 0; fi
        sleep 0.05
    done
    echo "client $old_window remained in _NET_CLIENT_LIST after exiting" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

launch_client() {
    local mode=$1
    local log=$test_dir/$mode.log
    if [[ "$mode" == normal ]]; then
        DISPLAY="$display" "$test_dir/configure-notify-client" >"$log" 2>&1 &
    else
        DISPLAY="$display" "$test_dir/configure-notify-client" southeast >"$log" 2>&1 &
    fi
    client_pid=$!
    window=
    for _ in $(seq 1 50); do
        window=$(sed -n 's/^window=//p' "$log" | head -n 1)
        if [[ -n "$window" ]] && DISPLAY="$display" xprop -id "$window" \
            _NET_FRAME_EXTENTS 2>/dev/null | grep -q '= '; then return 0; fi
        sleep 0.05
    done
    echo "$mode ConfigureNotify client did not become managed" >&2
    cat "$log" >&2
    return 1
}

launch_client normal
normal_log=$test_dir/normal.log
DISPLAY="$display" "$test_dir/configure-window" move "$window" 110 120
wait_for_synthetic "$normal_log" \
    "synthetic=1 event=$window window=$window x=110 y=120 width=200 height=120 border=0 above=0x0 override=0"
DISPLAY="$display" "$test_dir/configure-window" resize "$window" 333 222
wait_for_synthetic "$normal_log" \
    "synthetic=1 event=$window window=$window x=110 y=120 width=333 height=222 border=0 above=0x0 override=0"
DISPLAY="$display" "$test_dir/request-geometry" "$window"
wait_for_synthetic "$normal_log" \
    "synthetic=1 event=$window window=$window x=100 y=100 width=320 height=240 border=0 above=0x0 override=0"

DISPLAY="$display" "$test_dir/request-maximize" "$window" add
wait_for_synthetic "$normal_log" \
    "synthetic=1 event=$window window=$window x=2 y=26 width=796 height=572 border=0 above=0x0 override=0"
before=$(grep -c 'synthetic=1' "$normal_log")
DISPLAY="$display" "$test_dir/request-geometry" "$window"
for _ in $(seq 1 50); do
    after=$(grep -c 'synthetic=1' "$normal_log")
    if (( after > before )); then break; fi
    sleep 0.05
done
last=$(grep 'synthetic=1' "$normal_log" | tail -n 1)
expected="synthetic=1 event=$window window=$window x=2 y=26 width=796 height=572 border=0 above=0x0 override=0"
if [[ "$last" != *"$expected"* ]]; then
    echo "denied maximized request reported the wrong geometry: $last" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-maximize" "$window" remove
wait_for_synthetic "$normal_log" \
    "synthetic=1 event=$window window=$window x=100 y=100 width=320 height=240 border=0 above=0x0 override=0"
old_window=$window
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=
wait_for_unmanaged "$old_window"

launch_client southeast
southeast_log=$test_dir/southeast.log
DISPLAY="$display" "$test_dir/configure-window" resize "$window" 240 160
wait_for_synthetic "$southeast_log" \
    "synthetic=1 event=$window window=$window x=10 y=20 width=240 height=160 border=0 above=0x0 override=0"

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during ConfigureNotify checks" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
echo "ICCCM root-relative synthetic ConfigureNotify ordering and geometry passed on $display"
