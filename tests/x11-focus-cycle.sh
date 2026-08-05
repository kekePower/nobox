#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-focus-cycle.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
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
cycle_pid=
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$cycle_pid" ]]; then kill "$cycle_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for focus-cycle tests"
    exit 77
fi
cc "$(dirname "$0")/request-pager.c" -o "$test_dir/request-pager" -lX11
cc "$(dirname "$0")/request-state.c" -o "$test_dir/request-state" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11

cat >"$test_dir/config.toml" <<'EOF'
[focus]
raise_on_focus = false

[[keyboard.bindings]]
key = "A-Tab"
action = { type = "next_window" }

[[keyboard.bindings]]
key = "A-S-Tab"
action = { type = "previous_window" }

[[keyboard.bindings]]
key = "W-h"
action = { type = "focus_direction", direction = "left" }

[[keyboard.bindings]]
key = "W-j"
action = { type = "focus_direction", direction = "down" }

[[keyboard.bindings]]
key = "W-k"
action = { type = "focus_direction", direction = "up" }

[[keyboard.bindings]]
key = "W-l"
action = { type = "focus_direction", direction = "right" }

[[keyboard.bindings]]
key = "A-h"
action = { type = "cycle_direction", direction = "left" }

[[keyboard.bindings]]
key = "A-j"
action = { type = "cycle_direction", direction = "down" }

[[keyboard.bindings]]
key = "A-k"
action = { type = "cycle_direction", direction = "up" }

[[keyboard.bindings]]
key = "A-l"
action = { type = "cycle_direction", direction = "right" }

[[keyboard.bindings]]
key = "W-F5"
action = { type = "focus_to_bottom" }

[[keyboard.bindings]]
key = "W-F6"
action = { type = "unfocus" }

[[keyboard.bindings]]
key = "W-F7"
action = { type = "focus_fallback" }
EOF

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

wait_for_no_active() {
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>&1 || true)
        if ! grep -qi 'window id #' <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "an active window remained after fallback exhaustion: $observed" >&2
    return 1
}

wait_for_top_stacked() {
    local expected=${1,,}
    local observed=
    local top=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING)
        top=$(grep -o '0x[0-9a-fA-F]*' <<<"$observed" | tail -n 1)
        if [[ ${top,,} == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "top stacked window was $top, expected $expected: $observed" >&2
    return 1
}

wait_for_unshaded() {
    local window=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$observed" != *'_NET_WM_STATE_SHADED'* ]]; then return 0; fi
        sleep 0.05
    done
    echo "directional focus did not unshade $window: $observed" >&2
    return 1
}

wait_for_shaded() {
    local window=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$observed" == *'_NET_WM_STATE_SHADED'* ]]; then return 0; fi
        sleep 0.05
    done
    echo "$window did not remain shaded after cycle cancellation: $observed" >&2
    return 1
}

wait_for_overlay_state() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xwininfo -id "$focus_overlay" 2>/dev/null || true)
        if grep -q "Map State: $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "focus overlay state was not $expected" >&2
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

focus_overlay=
for _ in $(seq 1 40); do
    focus_overlay=$(DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk '/nobox:focus-switcher/ {print $1; exit}')
    if [[ -n "$focus_overlay" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$focus_overlay" ]]; then
    echo "persistent focus-switcher window was not created" >&2
    exit 1
fi
wait_for_overlay_state IsUnMapped

DISPLAY="$display" "$test_dir/press-key" --alt --repeat 2 Tab
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$second_window"

DISPLAY="$display" "$test_dir/press-key" --alt --shift Tab
wait_for_active "$third_window"

DISPLAY="$display" "$test_dir/press-key" --alt --hold-ms 1200 Tab &
cycle_pid=$!
wait_for_overlay_state IsViewable
expected_selected=$(printf '%d' "$second_window")
overlay_state=$(DISPLAY="$display" xprop -id "$focus_overlay" _NOBOX_FOCUS_SWITCHER)
if ! grep -q "= $expected_selected, 1, 3, 0" <<<"$overlay_state"; then
    echo "unexpected focus-switcher state: $overlay_state" >&2
    exit 1
fi
overlay_info=$(DISPLAY="$display" xwininfo -id "$focus_overlay")
for expected_geometry in \
    'Absolute upper-left X:  190' \
    'Absolute upper-left Y:  258' \
    'Width: 420' \
    'Height: 84'; do
    if ! grep -q "$expected_geometry" <<<"$overlay_info"; then
        echo "focus-switcher geometry did not contain '$expected_geometry'" >&2
        echo "$overlay_info" >&2
        exit 1
    fi
done
wait "$cycle_pid"
cycle_pid=
wait_for_overlay_state IsUnMapped
wait_for_active "$second_window"
if DISPLAY="$display" xprop -id "$focus_overlay" _NOBOX_FOCUS_SWITCHER 2>&1 |
    grep -q '= '; then
    echo "focus-switcher state remained published after modifier release" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" --alt --cancel Tab
wait_for_active "$second_window"
wait_for_overlay_state IsUnMapped
wait_for_top_stacked "$second_window"

DISPLAY="$display" "$test_dir/request-pager" geometry "$first_window" 1 xywh 50 200 240 120
DISPLAY="$display" "$test_dir/request-pager" geometry "$second_window" 1 xywh 500 350 240 120
DISPLAY="$display" "$test_dir/request-pager" geometry "$third_window" 1 xywh 500 50 240 120
sleep 0.1

DISPLAY="$display" "$test_dir/press-key" k
wait_for_active "$third_window"
DISPLAY="$display" "$test_dir/press-key" j
wait_for_active "$second_window"
DISPLAY="$display" "$test_dir/request-state" "$first_window" shade add
sleep 0.1
DISPLAY="$display" "$test_dir/press-key" h
wait_for_active "$first_window"
wait_for_unshaded "$first_window"
DISPLAY="$display" "$test_dir/press-key" l
wait_for_active "$second_window"
wait_for_top_stacked "$second_window"

DISPLAY="$display" "$test_dir/request-state" "$third_window" shade add
sleep 0.1
DISPLAY="$display" "$test_dir/press-key" --alt --hold-ms 1200 k &
cycle_pid=$!
wait_for_overlay_state IsViewable
wait_for_active "$third_window"
wait "$cycle_pid"
cycle_pid=
wait_for_overlay_state IsUnMapped
wait_for_active "$third_window"
wait_for_unshaded "$third_window"
wait_for_top_stacked "$third_window"

DISPLAY="$display" "$test_dir/request-state" "$second_window" shade add
sleep 0.1
DISPLAY="$display" "$test_dir/press-key" --alt --cancel j
wait_for_active "$third_window"
wait_for_overlay_state IsUnMapped
wait_for_shaded "$second_window"

DISPLAY="$display" "$test_dir/request-state" "$second_window" shade remove
wait_for_unshaded "$second_window"
DISPLAY="$display" "$test_dir/press-key" F5
wait_for_active "$third_window"
DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_active "$first_window"
DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$second_window"
DISPLAY="$display" "$test_dir/press-key" F6
wait_for_active "$first_window"
DISPLAY="$display" "$test_dir/press-key" F7
wait_for_active "$second_window"

DISPLAY="$display" "$test_dir/request-state" "$first_window" shade add
DISPLAY="$display" "$test_dir/request-state" "$third_window" shade add
wait_for_shaded "$first_window"
wait_for_shaded "$third_window"
DISPLAY="$display" "$test_dir/press-key" F6
wait_for_no_active

echo "X11 MRU, directional, and fallback focus policy passed on $display"
