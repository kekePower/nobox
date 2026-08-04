#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-menus.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 menu test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 menu test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/pointer-gesture.c" -o "$test_dir/pointer-gesture" -lX11 -lXtst ||
    ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for menu tests"
    exit 77
fi

keyboard_marker=$test_dir/keyboard-selected
pointer_marker=$test_dir/pointer-selected
cat >"$test_dir/config.toml" <<EOF
[menu]
width = 260
row_height = 26
max_rows = 8

[[menu.definitions]]
id = "root"
title = "nobox test"

[[menu.definitions.entries]]
type = "item"
label = "Keyboard action"
action = { type = "execute", command = "touch $keyboard_marker" }

[[menu.definitions.entries]]
type = "submenu"
label = "Session"
menu = "session"

[[menu.definitions]]
id = "session"
title = "Session"

[[menu.definitions.entries]]
type = "item"
label = "Pointer action"
action = { type = "execute", command = "touch $pointer_marker" }

[mouse]
[[mouse.bindings]]
context = "root"
button = "Right"
trigger = "press"
action = { type = "show_menu", menu = "root" }
EOF

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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 80); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

root_window=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ {print $4; exit}')
menu_window=
for _ in $(seq 1 40); do
    menu_window=$(DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk '/nobox:menu/ {print $1; exit}')
    if [[ -n "$menu_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$menu_window" ]]; then
    echo "persistent menu window was not created" >&2
    exit 1
fi

# Let the first XTest pointer and keyboard connections' MappingNotify refreshes
# settle before opening a grabbed menu; a live mapping change intentionally
# dismisses menus.
DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 1 click 10 10 0 0
DISPLAY="$display" "$test_dir/press-key" --plain Escape
sleep 0.2

wait_for_menu_state() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xwininfo -id "$menu_window" 2>/dev/null || true)
        if grep -q "Map State: $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "menu state was not $expected" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    return 1
}

wait_for_menu_property() {
    local atom=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$menu_window" "$atom" 2>/dev/null || true)
        if grep -q "$expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$atom was '$observed', expected '$expected'" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    return 1
}

open_root_menu() {
    DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 3 click 760 580 0 0
    wait_for_menu_state IsViewable
    wait_for_menu_property _NOBOX_MENU '"root"'
}

wait_for_menu_state IsUnMapped
open_root_menu
wait_for_menu_property _NOBOX_MENU_SELECTION '= 0, 2, 0'
menu_info=$(DISPLAY="$display" xwininfo -id "$menu_window")
for expected_geometry in \
    'Absolute upper-left X:  500' \
    'Absolute upper-left Y:  502' \
    'Width: 260' \
    'Height: 78'; do
    if ! grep -q "$expected_geometry" <<<"$menu_info"; then
        echo "menu geometry did not contain '$expected_geometry'" >&2
        echo "$menu_info" >&2
        exit 1
    fi
done

DISPLAY="$display" "$test_dir/press-key" --plain Down
wait_for_menu_property _NOBOX_MENU_SELECTION '= 1, 2, 0'
DISPLAY="$display" "$test_dir/press-key" --plain Right
wait_for_menu_property _NOBOX_MENU '"session"'
wait_for_menu_property _NOBOX_MENU_SELECTION '= 0, 1, 0'
DISPLAY="$display" "$test_dir/press-key" --plain Left
wait_for_menu_property _NOBOX_MENU '"root"'
wait_for_menu_property _NOBOX_MENU_SELECTION '= 1, 2, 0'
DISPLAY="$display" "$test_dir/press-key" --plain Home
DISPLAY="$display" "$test_dir/press-key" --plain Return
for _ in $(seq 1 40); do
    if [[ -e "$keyboard_marker" ]]; then break; fi
    sleep 0.05
done
if [[ ! -e "$keyboard_marker" ]]; then
    echo "keyboard-selected menu action did not run" >&2
    exit 1
fi
wait_for_menu_state IsUnMapped

open_root_menu
DISPLAY="$display" "$test_dir/press-key" --plain Escape
wait_for_menu_state IsUnMapped

open_root_menu
DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 1 click 510 564 0 0
wait_for_menu_property _NOBOX_MENU '"session"'
DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 1 click 510 565 0 0
for _ in $(seq 1 40); do
    if [[ -e "$pointer_marker" ]]; then break; fi
    sleep 0.05
done
if [[ ! -e "$pointer_marker" ]]; then
    echo "pointer-selected submenu action did not run" >&2
    exit 1
fi
wait_for_menu_state IsUnMapped

echo "X11 configured menus, keyboard navigation, pointer activation, and dismissal passed on $display"
