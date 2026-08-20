#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-opacity.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 opacity test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
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

cc "$(dirname "$0")/opacity-client.c" -o "$test_dir/opacity-client" -lX11

display=
for number in $(seq 171 190); do
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

supported=$(DISPLAY="$display" xprop -root _NET_SUPPORTED)
for atom in _NET_WM_WINDOW_OPACITY _NET_WM_PID; do
    if ! grep -Fq "$atom" <<<"$supported"; then
        echo "manager did not advertise $atom" >&2
        exit 1
    fi
done
support_window=$(DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK |
    grep -o '0x[0-9a-fA-F]*' | head -n 1)
manager_pid=$(DISPLAY="$display" xprop -id "$support_window" _NET_WM_PID)
if ! grep -Eq '= [1-9][0-9]*$' <<<"$manager_pid"; then
    echo "support window did not publish a valid manager PID" >&2
    echo "$manager_pid" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/opacity-client" >"$test_dir/client-window" 2>&1 &
client_pid=$!
client_window=
for _ in $(seq 1 40); do
    if [[ -s "$test_dir/client-window" ]]; then
        client_window=$(head -n 1 "$test_dir/client-window")
    fi
    if [[ -n "$client_window" ]] && DISPLAY="$display" xprop -id "$client_window" \
        _NET_FRAME_EXTENTS >/dev/null 2>&1; then break; fi
    sleep 0.05
done
if [[ -z "$client_window" ]]; then
    echo "opacity client did not map" >&2
    exit 1
fi
frame_window=$(DISPLAY="$display" xwininfo -id "$client_window" -tree |
    sed -n 's/.*Parent window id: \(0x[0-9a-fA-F]*\).*/\1/p')
if [[ -z "$frame_window" ]]; then
    echo "opacity client frame was not found" >&2
    exit 1
fi

wait_for_opacity() {
    local expected=$1
    for _ in $(seq 1 40); do
        if DISPLAY="$display" xprop -id "$frame_window" _NET_WM_WINDOW_OPACITY 2>/dev/null |
            grep -Fq "= $expected"; then return 0; fi
        sleep 0.05
    done
    DISPLAY="$display" xprop -id "$frame_window" _NET_WM_WINDOW_OPACITY >&2 || true
    return 1
}

wait_for_opacity 2147483647
DISPLAY="$display" xprop -id "$client_window" -f _NET_WM_WINDOW_OPACITY 32c \
    -set _NET_WM_WINDOW_OPACITY 305419896 >/dev/null
wait_for_opacity 305419896
DISPLAY="$display" xprop -id "$client_window" -remove _NET_WM_WINDOW_OPACITY
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -id "$frame_window" _NET_WM_WINDOW_OPACITY 2>&1 |
        grep -Eq 'not found|no such atom'; then
        echo "X11 frame opacity synchronization passed on $display"
        exit 0
    fi
    sleep 0.05
done
echo "deleted client opacity remained on the frame" >&2
exit 1
