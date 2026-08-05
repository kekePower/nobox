#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-menus.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
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
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
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
command_marker=$test_dir/command-selected
mkdir -m 700 "$test_dir/runtime"
cat >"$test_dir/command-menu.toml" <<EOF
[[entries]]
type = "item"
label = "_Generated action"
action = { type = "execute", command = "touch $command_marker" }
EOF
cat >"$test_dir/config.toml" <<EOF
[menu]
width = 260
row_height = 26
max_rows = 8
command_timeout_ms = 100

[[menu.definitions]]
id = "root"
title = "nobox test"

[[menu.definitions.entries]]
type = "item"
label = "_Keyboard action"
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

[[menu.definitions]]
id = "windows"
title = "Windows"
source = "windows"

[[menu.definitions]]
id = "client"
title = "Window"
source = "client"

[[menu.definitions]]
id = "client-workspaces"
title = "Send to workspace"
source = "client_workspaces"

[[menu.definitions]]
id = "command"
title = "Generated"
source = "command"
command = "cat $test_dir/command-menu.toml"

[[menu.definitions]]
id = "slow-command"
title = "Too slow"
source = "command"
command = "sleep 2"

[keyboard]
[[keyboard.bindings]]
key = "A-space"
action = { type = "show_menu", menu = "client" }

[[keyboard.bindings]]
key = "A-F11"
action = { type = "toggle_fullscreen" }

[[keyboard.bindings]]
key = "A-F12"
action = { type = "toggle_always_on_top" }

[[keyboard.bindings]]
key = "A-S-F12"
action = { type = "toggle_always_on_bottom" }

[[keyboard.bindings]]
key = "A-F10"
action = { type = "toggle_decorations" }

[[keyboard.bindings]]
key = "A-F8"
action = { type = "toggle_maximize_horizontal" }

[[keyboard.bindings]]
key = "A-F9"
action = { type = "toggle_maximize_vertical" }

[[keyboard.bindings]]
key = "W-r"
action = { type = "reconfigure" }

[[keyboard.bindings]]
key = "W-p"
action = { type = "show_menu", menu = "command" }

[[keyboard.bindings]]
key = "W-o"
action = { type = "show_menu", menu = "slow-command" }

[mouse]
[[mouse.bindings]]
context = "root"
button = "Right"
trigger = "press"
action = { type = "show_menu", menu = "root" }

[[mouse.bindings]]
context = "root"
button = "Middle"
trigger = "press"
action = { type = "show_menu", menu = "windows" }
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

DISPLAY="$display" XDG_RUNTIME_DIR="$test_dir/runtime" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
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

DISPLAY="$display" "$test_dir/press-key" p
wait_for_menu_state IsViewable
wait_for_menu_property _NOBOX_MENU '"command"'
wait_for_menu_property _NOBOX_MENU_SELECTION '= 0, 1, 0'
DISPLAY="$display" "$test_dir/press-key" --plain Return
for _ in $(seq 1 40); do
    if [[ -e "$command_marker" ]]; then break; fi
    sleep 0.05
done
if [[ ! -e "$command_marker" ]]; then
    echo "command-generated menu action did not run" >&2
    exit 1
fi
wait_for_menu_state IsUnMapped

timeout_count=$(grep -c 'command exceeded 100ms' "$test_dir/nobox.log" || true)
DISPLAY="$display" "$test_dir/press-key" o
for _ in $(seq 1 40); do
    current_timeout_count=$(grep -c 'command exceeded 100ms' "$test_dir/nobox.log" || true)
    if (( current_timeout_count > timeout_count )); then break; fi
    sleep 0.05
done
if (( current_timeout_count <= timeout_count )); then
    echo "slow command menu was not stopped at its configured deadline" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
wait_for_menu_state IsUnMapped
if find "$test_dir/runtime" -mindepth 1 -print -quit | grep -q .; then
    echo "command menu left a runtime output file behind" >&2
    find "$test_dir/runtime" -mindepth 1 -maxdepth 1 -print >&2
    exit 1
fi

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
DISPLAY="$display" "$test_dir/press-key" --plain k
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

wait_for_active() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)
        if grep -qi "window id # $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $expected" >&2
    return 1
}

wait_for_state() {
    local window=$1
    local atom=$2
    local expected=$3
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present ]] && grep -q "$atom" <<<"$observed"; then return 0; fi
        if [[ "$expected" == absent ]] && ! grep -q "$atom" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$atom was unexpectedly $expected for $window: $observed" >&2
    return 1
}

wait_for_frame_extents() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS)
        if grep -q "$expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "frame extents were $observed, expected $expected" >&2
    return 1
}

launch_client() {
    local title=$1
    launched_window=
    DISPLAY="$display" xterm -title "$title" -geometry 30x8+30+40 \
        >"$test_dir/$title.log" 2>&1 &
    client_pids+=("$!")
    for _ in $(seq 1 60); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
                grep -q "$title"; then
                launched_window=$candidate
            fi
        done
        if [[ -n "$launched_window" ]] && DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
            grep -qi "window id # $launched_window"; then return 0; fi
        sleep 0.05
    done
    echo "client $title did not become active" >&2
    return 1
}

launched_window=
launch_client nobox-menu-one
first_window=$launched_window
launch_client nobox-menu-two
second_window=$launched_window

DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 2 click 400 300 0 0
wait_for_menu_property _NOBOX_MENU '"windows"'
wait_for_menu_property _NOBOX_MENU_SELECTION '= 1, 3, 0'
DISPLAY="$display" "$test_dir/press-key" --plain Home
DISPLAY="$display" "$test_dir/press-key" --plain Return
wait_for_active "$first_window"
wait_for_menu_state IsUnMapped

DISPLAY="$display" "$test_dir/press-key" --alt space
wait_for_menu_property _NOBOX_MENU '"client"'
wait_for_menu_property _NOBOX_MENU_SELECTION '= 0, 13, 0'
DISPLAY="$display" "$test_dir/press-key" --plain x
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -id "$first_window" _NET_WM_STATE |
        grep -q '_NET_WM_STATE_MAXIMIZED_HORZ'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -id "$first_window" _NET_WM_STATE |
    grep -q '_NET_WM_STATE_MAXIMIZED_HORZ'; then
    echo "client-menu maximize accelerator did not run" >&2
    exit 1
fi
wait_for_menu_state IsUnMapped

DISPLAY="$display" "$test_dir/press-key" --alt space
DISPLAY="$display" "$test_dir/press-key" --plain s
wait_for_menu_property _NOBOX_MENU '"client-workspaces"'
DISPLAY="$display" "$test_dir/press-key" --plain 2
for _ in $(seq 1 40); do
    desktop=$(DISPLAY="$display" xprop -id "$first_window" _NET_WM_DESKTOP)
    if grep -q '= 1' <<<"$desktop"; then break; fi
    sleep 0.05
done
if ! grep -q '= 1' <<<"$desktop"; then
    echo "client workspace menu did not move the target: $desktop" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/pointer-gesture" "$root_window" 2 click 400 300 0 0
wait_for_menu_property _NOBOX_MENU_SELECTION '= 1, 4, 0'
DISPLAY="$display" "$test_dir/press-key" --plain End
DISPLAY="$display" "$test_dir/press-key" --plain Return
wait_for_active "$first_window"
current_desktop=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
if ! grep -q '= 1' <<<"$current_desktop"; then
    echo "window-list activation did not switch workspace: $current_desktop" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" --alt space
DISPLAY="$display" "$test_dir/press-key" --plain s
DISPLAY="$display" "$test_dir/press-key" --plain a
for _ in $(seq 1 40); do
    desktop=$(DISPLAY="$display" xprop -id "$first_window" _NET_WM_DESKTOP)
    if grep -q '= 4294967295' <<<"$desktop"; then break; fi
    sleep 0.05
done
if ! grep -q '= 4294967295' <<<"$desktop"; then
    echo "all-workspaces accelerator did not make the client sticky: $desktop" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" --alt space
DISPLAY="$display" "$test_dir/press-key" --plain f
wait_for_state "$first_window" _NET_WM_STATE_FULLSCREEN present
wait_for_menu_state IsUnMapped
DISPLAY="$display" "$test_dir/press-key" --alt F11
wait_for_state "$first_window" _NET_WM_STATE_FULLSCREEN absent

DISPLAY="$display" "$test_dir/press-key" --alt F12
wait_for_state "$first_window" _NET_WM_STATE_ABOVE present
wait_for_state "$first_window" _NET_WM_STATE_BELOW absent
DISPLAY="$display" "$test_dir/press-key" --alt --shift F12
wait_for_state "$first_window" _NET_WM_STATE_ABOVE absent
wait_for_state "$first_window" _NET_WM_STATE_BELOW present
DISPLAY="$display" "$test_dir/press-key" --alt --shift F12
wait_for_state "$first_window" _NET_WM_STATE_ABOVE absent
wait_for_state "$first_window" _NET_WM_STATE_BELOW absent

DISPLAY="$display" "$test_dir/press-key" --alt F10
wait_for_frame_extents "$first_window" '= 0, 0, 0, 0'
DISPLAY="$display" "$test_dir/press-key" --alt F10
wait_for_frame_extents "$first_window" '= 2, 2, 26, 2'

DISPLAY="$display" "$test_dir/press-key" --alt F8
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_HORZ absent
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_VERT present
DISPLAY="$display" "$test_dir/press-key" --alt F8
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_HORZ present
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_VERT present
DISPLAY="$display" "$test_dir/press-key" --alt F9
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_HORZ present
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_VERT absent
DISPLAY="$display" "$test_dir/press-key" --alt F9
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_HORZ present
wait_for_state "$first_window" _NET_WM_STATE_MAXIMIZED_VERT present

reload_count=$(grep -c 'configuration reload contained no changes' "$test_dir/nobox.log" || true)
DISPLAY="$display" "$test_dir/press-key" r
for _ in $(seq 1 40); do
    current_reload_count=$(
        grep -c 'configuration reload contained no changes' "$test_dir/nobox.log" || true
    )
    if (( current_reload_count > reload_count )); then break; fi
    sleep 0.05
done
if (( current_reload_count <= reload_count )); then
    echo "typed reconfigure action did not reload the active configuration" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

echo "X11 static, generated, and client menus plus state actions passed on $display"
