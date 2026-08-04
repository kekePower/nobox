#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-focus-cycle.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 focus-cycle test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 focus-cycle test"
    exit 77
fi

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

if ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for focus-cycle tests"
    exit 77
fi

display=
for number in $(seq 191 210); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

wait_for_active() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)
        if grep -qi "window id # $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $expected" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

launch_client() {
    local title=$1
    local log=$2
    launched_window=
    DISPLAY="$display" xterm -title "$title" -geometry 30x8+30+40 \
        >"$test_dir/$log" 2>&1 &
    client_pids+=("$!")
    local window=
    for _ in $(seq 1 40); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
                grep -q "$title"; then
                window=$candidate
            fi
        done
        if [[ -n "$window" ]] && DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
            grep -qi "window id # $window"; then
            launched_window=$window
            return 0
        fi
        sleep 0.05
    done
    echo "client $title did not become active" >&2
    return 1
}

launched_window=
launch_client nobox-cycle-one first.log
first_window=$launched_window
launch_client nobox-cycle-two second.log
second_window=$launched_window
launch_client nobox-cycle-three third.log
third_window=$launched_window
echo "cycle clients: first=$first_window second=$second_window third=$third_window"

DISPLAY="$display" "$test_dir/press-key" --alt --repeat 2 Tab
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$second_window"

DISPLAY="$display" "$test_dir/press-key" --alt --shift Tab
wait_for_active "$third_window"

echo "X11 modifier-held MRU focus cycling passed on $display"
