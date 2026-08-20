#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-colormaps.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 colormap test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
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

cc "$(dirname "$0")/colormap-client.c" -o "$test_dir/colormap-client" -lX11
cc "$(dirname "$0")/colormap-control.c" -o "$test_dir/colormap-control" -lX11
cc "$(dirname "$0")/set-input-focus.c" -o "$test_dir/set-input-focus" -lX11

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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done

launch_client() {
    local title=$1
    local x=$2
    local y=$3
    local output=$test_dir/$title.colormaps
    DISPLAY="$display" "$test_dir/colormap-client" "$title" "$x" "$y" \
        >"$output" 2>&1 &
    client_pids+=("$!")
    for _ in $(seq 1 50); do
        if [[ -s "$output" ]]; then
            read -r launched_top launched_child launched_topmap launched_childmap launched_default \
                <"$output"
        fi
        if [[ -n "${launched_top:-}" ]] &&
            DISPLAY="$display" xprop -id "$launched_top" _NET_FRAME_EXTENTS >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.05
    done
    echo "$title did not map" >&2
    return 1
}

installed_colormaps() {
    DISPLAY="$display" "$test_dir/colormap-control" list
}

wait_for_installed() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(installed_colormaps)
        if [[ " ${observed,,} " == *" $expected "* ]]; then return 0; fi
        sleep 0.05
    done
    echo "installed colormaps were '$observed', expected '$1'" >&2
    return 1
}

focus_window() {
    DISPLAY="$display" "$test_dir/set-input-focus" "$1"
}

launch_client nobox-colormap-first 60 70
first_top=$launched_top
first_child=$launched_child
first_topmap=$launched_topmap
first_childmap=$launched_childmap
default_colormap=$launched_default
launch_client nobox-colormap-second 440 310
second_top=$launched_top
second_child=$launched_child
second_childmap=$launched_childmap

# Explicit property order makes the child colormap the highest priority.
focus_window "$first_child"
wait_for_installed "$first_childmap"
focus_window "$second_child"
wait_for_installed "$second_childmap"

# Property changes are applied immediately to the client with colormap focus.
DISPLAY="$display" "$test_dir/colormap-control" property "$first_top" "$first_top"
focus_window "$first_child"
wait_for_installed "$first_topmap"
DISPLAY="$display" "$test_dir/colormap-control" property \
    "$first_top" "$first_child" "$first_top"
wait_for_installed "$first_childmap"

# ColormapNotify(new=True) updates a watched subwindow without a property rewrite.
replacement=$(DISPLAY="$display" "$test_dir/colormap-control" replace "$first_child")
wait_for_installed "$replacement"

# Wrongly typed, empty, and oversized duplicate properties safely retain the implicit top level.
DISPLAY="$display" "$test_dir/colormap-control" malformed "$first_top"
wait_for_installed "$first_topmap"
DISPLAY="$display" "$test_dir/colormap-control" property "$first_top"
wait_for_installed "$first_topmap"
DISPLAY="$display" "$test_dir/colormap-control" repeat "$first_top" "$first_child" 300
wait_for_installed "$first_topmap"

# With no managed client focused, the screen's default colormap is restored.
root_window=$(DISPLAY="$display" xwininfo -root 2>/dev/null |
    awk '/Window id:/ { print $4; exit }')
focus_window "$root_window"
wait_for_installed "$default_colormap"

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during ICCCM colormap handling" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 ICCCM colormap handling passed on $display"
