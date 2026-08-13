#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-window-snapping.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 window-snapping test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/window-snap-client.c" -o "$test_dir/window-snap-client" -lX11 \
    || ! cc "$(dirname "$0")/interactive-drag.c" -o "$test_dir/interactive-drag" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for window-snapping tests"
    exit 77
fi

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
for _ in $(seq 1 80); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
    grep -q 'window id'; then
    echo "nobox did not publish its supporting window" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

launch_client() {
    local title=$1
    shift
    DISPLAY="$display" "$test_dir/window-snap-client" "$title" "$@" \
        >"$test_dir/$title.log" 2>&1 &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 80); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
                grep -q "$title"; then
                launched_window=$candidate
                return 0
            fi
        done
        sleep 0.05
    done
    echo "client $title did not map" >&2
    return 1
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk -F: '
        /Absolute upper-left X:/ { gsub(/ /, "", $2); x=$2 }
        /Absolute upper-left Y:/ { gsub(/ /, "", $2); y=$2 }
        /^  Width:/ { gsub(/ /, "", $2); w=$2 }
        /^  Height:/ { gsub(/ /, "", $2); h=$2 }
        END { print x "," y "," w "," h }
    '
}

launch_client snap-target 400 100 200 150
target_window=$launched_window
launch_client snap-mover 100 110 200 150
mover_window=$launched_window
if [[ "$(window_geometry "$target_window")" != '400,100,200,150' \
    || "$(window_geometry "$mover_window")" != '100,110,200,150' ]]; then
    echo "window-snapping clients started at unexpected geometry" >&2
    DISPLAY="$display" xwininfo -root -tree >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/interactive-drag" "$mover_window" move commit 90 -6
if [[ "$(window_geometry "$mover_window")" != '196,100,200,150' ]]; then
    echo "default window resistance did not join decorated edges and corners" >&2
    DISPLAY="$display" xwininfo -id "$mover_window" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/interactive-drag" "$mover_window" move commit -96 10
if [[ "$(window_geometry "$mover_window")" != '100,110,200,150' ]]; then
    echo "moving away from a snapped peer did not restore the test position" >&2
    exit 1
fi
printf '[mouse]\nsnap_to_windows = false\n' >"$test_dir/config.toml"
kill -HUP "$nobox_pid"
sleep 0.2
DISPLAY="$display" "$test_dir/interactive-drag" "$mover_window" move commit 90 -6
if [[ "$(window_geometry "$mover_window")" != '190,104,200,150' ]]; then
    echo "disabling peer snapping did not preserve unsnapped pointer geometry" >&2
    DISPLAY="$display" xwininfo -id "$mover_window" >&2
    exit 1
fi

echo "Default-on configurable decorated window snapping passed on $display"
