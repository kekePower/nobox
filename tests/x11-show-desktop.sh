#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-show-desktop.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 show-desktop test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 show-desktop test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
surface_pid=
late_pid=
cleanup() {
    if [[ -n "$late_pid" ]]; then kill "$late_pid" 2>/dev/null || true; fi
    if [[ -n "$surface_pid" ]]; then kill "$surface_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

source_dir=$(dirname "$0")
cc "$source_dir/show-desktop-client.c" -o "$test_dir/show-desktop-client" -lX11
cc "$source_dir/request-show-desktop.c" -o "$test_dir/request-show-desktop" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$source_dir/request-iconic.c" -o "$test_dir/request-iconic" -lX11
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for the X11 show-desktop test"
    exit 77
fi

display=
for number in $(seq 291 310); do
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

DISPLAY="$display" "$test_dir/show-desktop-client" >"$test_dir/windows" 2>&1 &
surface_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/windows" ]]; then break; fi
    sleep 0.05
done
read -r desktop_window dock_window first_window second_window <"$test_dir/windows"

wait_for_map_state() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xwininfo -id "$window" 2>/dev/null |
            awk -F: '/Map State:/ { gsub(/ /, "", $2); print $2; exit }')
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "map state for $window was '$observed', expected '$expected'" >&2
    return 1
}

wait_for_showing() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_SHOWING_DESKTOP 2>/dev/null || true)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "show-desktop property was '$observed', expected $expected" >&2
    return 1
}

wait_for_active() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null |
            grep -o '0x[0-9a-fA-F]*' | tail -n 1 || true)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "active window was '$observed', expected '$1'" >&2
    return 1
}

for window in "$desktop_window" "$dock_window" "$first_window" "$second_window"; do
    for _ in $(seq 1 50); do
        if DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null | grep -qi "$window"; then
            break
        fi
        sleep 0.05
    done
done
wait_for_showing 0
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED | grep -q '_NET_SHOWING_DESKTOP'; then
    echo "_NET_SHOWING_DESKTOP was not advertised" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-iconic" "$second_window"
wait_for_map_state "$second_window" IsUnviewable
DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/request-show-desktop" 1
wait_for_showing 1
wait_for_map_state "$first_window" IsUnviewable
wait_for_map_state "$desktop_window" IsViewable
wait_for_map_state "$dock_window" IsViewable
if ! DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW | grep -q 'not found'; then
    echo "show-desktop mode retained an ordinary active window" >&2
    exit 1
fi
if DISPLAY="$display" xprop -id "$first_window" _NET_WM_STATE |
    grep -q '_NET_WM_STATE_HIDDEN'; then
    echo "show-desktop mode misreported a normal client as minimized" >&2
    exit 1
fi

# Invalid EWMH boolean values are ignored.
DISPLAY="$display" "$test_dir/request-show-desktop" 2
wait_for_showing 1
DISPLAY="$display" "$test_dir/request-show-desktop" 0
wait_for_showing 0
wait_for_map_state "$first_window" IsViewable
wait_for_map_state "$second_window" IsUnviewable
wait_for_active "$first_window"
if ! DISPLAY="$display" xprop -id "$second_window" _NET_WM_STATE |
    grep -q '_NET_WM_STATE_HIDDEN'; then
    echo "show-desktop restore lost genuine minimized state" >&2
    exit 1
fi

# The typed default action enters the same policy state.
DISPLAY="$display" "$test_dir/press-key" d
wait_for_showing 1
wait_for_map_state "$first_window" IsUnviewable

# A newly mapped ordinary client remains hidden until explicit activation.
DISPLAY="$display" "$test_dir/presentation-client" --title nobox-show-desktop-late \
    >"$test_dir/late.window" 2>"$test_dir/late.log" &
late_pid=$!
late_window=
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/late.window" ]]; then late_window=$(head -n 1 "$test_dir/late.window"); fi
    if [[ -n "$late_window" ]] && DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -qi "$late_window"; then break; fi
    sleep 0.05
done
wait_for_map_state "$late_window" IsUnMapped
wait_for_showing 1

# Pager activation leaves show-desktop mode and restores ordinary clients.
DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_showing 0
wait_for_map_state "$first_window" IsViewable
wait_for_map_state "$late_window" IsViewable
wait_for_active "$first_window"
wait_for_map_state "$second_window" IsUnviewable

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during show-desktop checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "EWMH and typed-action X11 show-desktop policy passed on $display"
