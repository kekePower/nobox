#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-mouse-bindings.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 mouse-binding test"
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

if ! cc "$(dirname "$0")/pointer-gesture.c" -o "$test_dir/pointer-gesture" -lX11 -lXtst ||
    ! cc "$(dirname "$0")/mouse-client.c" -o "$test_dir/mouse-client" -lX11; then
    echo "SKIP: XTest development libraries are required for mouse-binding tests"
    exit 77
fi

display=
for number in $(seq 251 270); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
if [[ -z "$display" ]]; then
    echo "no unused nested X11 display found" >&2
    exit 1
fi

cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["one", "two", "three"]

[mouse]
drag_threshold = 20
double_click_ms = 500

[[mouse.bindings]]
context = "client"
button = "Left"
trigger = "press"
action = { type = "focus" }

[[mouse.bindings]]
context = "client"
button = "Left"
trigger = "click"
action = { type = "raise" }

[[mouse.bindings]]
context = "titlebar"
button = "Left"
trigger = "press"
actions = [{ type = "focus" }, { type = "raise" }]

[[mouse.bindings]]
context = "titlebar"
button = "Left"
trigger = "drag"
action = { type = "move" }

[[mouse.bindings]]
context = "titlebar"
button = "Left"
trigger = "double_click"
action = { type = "toggle_maximize" }

[[mouse.bindings]]
context = "titlebar"
button = "Middle"
trigger = "click"
action = { type = "lower" }

[[mouse.bindings]]
context = "border"
button = "Left"
trigger = "drag"
action = { type = "resize" }

[[mouse.bindings]]
context = "titlebar"
button = "Right"
trigger = "click"
action = { type = "next_workspace" }

[[mouse.bindings]]
context = "root"
button = "Down"
trigger = "click"
actions = [{ type = "next_workspace" }, { type = "next_workspace" }]

[[mouse.bindings]]
context = "client"
button = "W-Right"
trigger = "click"
action = { type = "next_workspace" }

[[mouse.bindings]]
context = "client"
button = "W-Left"
trigger = "drag"
action = { type = "resize", edge = "left" }
EOF

"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
wm_ready=false
for _ in $(seq 1 80); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then
        wm_ready=true
        break
    fi
    sleep 0.05
done
if [[ "$wm_ready" != true ]]; then
    echo "nobox did not publish its supporting window" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

launch_client() {
    local title=$1
    local geometry=$2
    launched_window=
    DISPLAY="$display" xterm -title "$title" -geometry "$geometry" \
        >"$test_dir/$title.log" 2>&1 &
    client_pids+=("$!")
    for _ in $(seq 1 80); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
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
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    tail -n 40 "$test_dir/$title.log" >&2 || true
    return 1
}

launch_mouse_client() {
    local title=$1
    launched_window=
    DISPLAY="$display" "$test_dir/mouse-client" "$title" \
        >"$test_dir/$title.log" 2>&1 &
    client_pids+=("$!")
    for _ in $(seq 1 80); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
                grep -q "$title"; then
                launched_window=$candidate
                return 0
            fi
        done
        sleep 0.05
    done
    echo "mouse client $title did not map" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

frame_for() {
    DISPLAY="$display" xwininfo -tree -id "$1" |
        awk '/Parent window id:/ { print $4; exit }'
}

window_position() {
    DISPLAY="$display" xwininfo -id "$1" |
        awk -F: '
            /Absolute upper-left X:/ { gsub(/ /, "", $2); x=$2 }
            /Absolute upper-left Y:/ { gsub(/ /, "", $2); y=$2 }
            END { print x "," y }
        '
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" |
        awk -F: '
            /Absolute upper-left X:/ { gsub(/ /, "", $2); x=$2 }
            /Absolute upper-left Y:/ { gsub(/ /, "", $2); y=$2 }
            /^  Width:/ { gsub(/ /, "", $2); w=$2 }
            /^  Height:/ { gsub(/ /, "", $2); h=$2 }
            END { print x "," y "," w "," h }
        '
}

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

wait_for_top() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
            grep -o '0x[0-9a-fA-F]*' | tail -n 1)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "top client was $observed, expected $expected" >&2
    return 1
}

wait_for_maximized() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == yes ]] && grep -q '_NET_WM_STATE_MAXIMIZED_HORZ' <<<"$observed"; then
            return 0
        fi
        if [[ "$expected" == no ]] && ! grep -q '_NET_WM_STATE_MAXIMIZED_HORZ' <<<"$observed"; then
            return 0
        fi
        sleep 0.05
    done
    echo "maximize state for $window was $observed, expected $expected" >&2
    return 1
}

launch_client nobox-mouse-one 30x8+80+100
first_window=$launched_window
launch_client nobox-mouse-two 30x8+420+100
second_window=$launched_window
first_frame=$(frame_for "$first_window")
second_frame=$(frame_for "$second_window")
if [[ -z "$first_frame" || -z "$second_frame" ]]; then
    echo "could not discover client frames" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/pointer-gesture" "$first_window" 1 click 10 10 0 0
wait_for_active "$first_window"
wait_for_top "$first_window"

DISPLAY="$display" "$test_dir/pointer-gesture" "$second_frame" 2 click 10 10 0 0
wait_for_top "$first_window"

DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 click 10 10 0 0
wait_for_active "$first_window"
wait_for_top "$first_window"

DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 double 10 10 0 0
wait_for_maximized "$first_window" yes
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 double 10 10 0 0
wait_for_maximized "$first_window" no

DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 3 drag 10 10 -15 -15
desktop=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
if ! grep -q '= 0' <<<"$desktop"; then
    echo "release outside the titlebar incorrectly fired its click: $desktop" >&2
    exit 1
fi

before=$(window_position "$first_window")
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 drag 10 10 80 60
after=$(window_position "$first_window")
IFS=, read -r before_x before_y <<<"$before"
expected="$((before_x + 80)),$((before_y + 60))"
if [[ "$after" != "$expected" ]]; then
    echo "titlebar drag moved client from $before to $after, expected $expected" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

before=$(window_geometry "$first_window")
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 drag 4 60 -40 0
after=$(window_geometry "$first_window")
IFS=, read -r before_x before_y before_width before_height <<<"$before"
IFS=, read -r after_x after_y after_width after_height <<<"$after"
if (( after_x >= before_x
      || after_x + after_width != before_x + before_width
      || after_y != before_y
      || after_height != before_height )); then
    echo "left-border resize did not preserve its opposite anchor: $before to $after" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

before=$(window_geometry "$first_window")
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 drag 4 4 -30 -20
after=$(window_geometry "$first_window")
IFS=, read -r before_x before_y before_width before_height <<<"$before"
IFS=, read -r after_x after_y after_width after_height <<<"$after"
if (( after_x >= before_x
      || after_y >= before_y
      || after_x + after_width != before_x + before_width
      || after_y + after_height != before_y + before_height )); then
    echo "top-left resize handle did not preserve its opposite corner: $before to $after" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

before=$(window_geometry "$first_window")
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_window" 1 drag 10 10 -30 0 super
after=$(window_geometry "$first_window")
IFS=, read -r before_x before_y before_width before_height <<<"$before"
IFS=, read -r after_x after_y after_width after_height <<<"$after"
if (( after_x >= before_x
      || after_x + after_width != before_x + before_width
      || after_y != before_y
      || after_height != before_height )); then
    echo "fixed-edge client resize did not preserve its right anchor: $before to $after" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

root=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_window" 3 click 10 10 0 0 super
desktop=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
if ! grep -q '= 1' <<<"$desktop"; then
    echo "modified client-context mouse binding did not fire: $desktop" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/pointer-gesture" "$root" 5 click 10 10 0 0
desktop=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
if ! grep -q '= 0' <<<"$desktop"; then
    echo "ordered root mouse actions did not advance two workspaces: $desktop" >&2
    exit 1
fi

launch_mouse_client nobox-mouse-observer
observer_window=$launched_window
DISPLAY="$display" "$test_dir/pointer-gesture" "$first_frame" 1 click 10 10 0 0
wait_for_active "$first_window"
DISPLAY="$display" "$test_dir/pointer-gesture" "$observer_window" 1 click 10 10 0 0
wait_for_active "$observer_window"
pressed=
for _ in $(seq 1 40); do
    pressed=$(DISPLAY="$display" xprop -root _NOBOX_TEST_BUTTON_PRESS 2>/dev/null || true)
    if grep -qi "window id # $observer_window" <<<"$pressed"; then break; fi
    sleep 0.05
done
if ! grep -qi "window id # $observer_window" <<<"$pressed"; then
    echo "client did not receive the replayed focus click: $pressed" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

echo "X11 context-aware mouse bindings and gesture actions passed on $display"
