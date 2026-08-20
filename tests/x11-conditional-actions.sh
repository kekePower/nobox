#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-conditional-actions.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 conditional-actions test"
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

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
cc "$source_dir/set-input-focus.c" -o "$test_dir/set-input-focus" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for conditional-action tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<EOF
[focus]
focus_new = true
raise_on_focus = false

[workspaces]
names = ["one", "two"]

[[keyboard.bindings]]
key = "W-F1"
action = { type = "if", query = [{ class = "NoboxTest", title = "group-one", focused = true, decorated = true, workspace = "current", output = 1, kind = "normal" }], then = [{ type = "shade" }], else = [{ type = "minimize" }] }

[[keyboard.bindings]]
key = "W-F2"
action = { type = "if", query = [{ shaded = true }], then = [{ type = "unshade" }], else = [{ type = "minimize" }] }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "if", query = [{ title = "group-renamed" }], then = [{ type = "send_to_layer", layer = "above" }], else = [{ type = "send_to_layer", layer = "below" }] }

[[keyboard.bindings]]
key = "W-F4"
action = { type = "for_each", query = [{ title = "group-*" }], then = [{ type = "send_to_layer", layer = "above" }], else = [{ type = "send_to_layer", layer = "below" }] }

[[keyboard.bindings]]
key = "W-F5"
actions = [
    { type = "for_each", query = [{ class = "NoboxTest" }], then = [{ type = "toggle_sticky" }, { type = "stop" }, { type = "kill" }] },
    { type = "execute", command = "printf continued > '$test_dir/continued'" },
]

[[keyboard.bindings]]
key = "W-F6"
action = { type = "for_each", query = [{ class = "DoesNotExist" }], then = [{ type = "raise" }], none = [{ type = "execute", command = "printf none > '$test_dir/none'" }] }

[[keyboard.bindings]]
key = "W-F7"
actions = [
    { type = "if", query = [{ title = "DoesNotMatch" }], then = [{ type = "raise" }], else = [{ type = "send_to_layer", layer = "normal" }, { type = "stop" }] },
    { type = "execute", command = "printf leaked > '$test_dir/after-stop'" },
]

[[keyboard.bindings]]
key = "W-F8"
action = { type = "if", query = [{ active_workspace = 2 }], then = [{ type = "execute", command = "printf active > '$test_dir/active'" }] }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "next_workspace" }

[[keyboard.bindings]]
key = "W-F10"
action = { type = "debug", message = "conditional-debug-marker" }

[[keyboard.bindings]]
key = "W-a"
action = { type = "for_each", query = [{ title = "group-two" }], then = [{ type = "focus" }] }

[[keyboard.bindings]]
key = "W-b"
action = { type = "next_workspace" }

[[keyboard.bindings]]
key = "W-c"
action = { type = "for_each", query = [{ title = "group-two" }], then = [{ type = "focus", here = true }] }

[[keyboard.bindings]]
key = "W-d"
action = { type = "minimize" }
EOF

display=
for number in $(seq 691 710); do
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
if ! DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
    grep -q 'window id'; then
    echo "nobox did not claim the nested X11 display" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

launch_client() {
    local title=$1
    local output=$test_dir/$title.window
    DISPLAY="$display" "$test_dir/presentation-client" --title "$title" \
        >"$output" 2>"$test_dir/$title.log" &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 50); do
        launched_window=$(awk 'NR == 1 { print; exit }' "$output" 2>/dev/null || true)
        if [[ -n "$launched_window" ]] && DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
            grep -qi "$launched_window"; then return 0; fi
        sleep 0.05
    done
    echo "$title did not map" >&2
    return 1
}

press() {
    DISPLAY="$display" "$test_dir/press-key" "$1"
}

wait_for_state() {
    local window=$1
    local state=$2
    local expected=$3
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE 2>&1 || true)
        if [[ "$expected" == yes && "$observed" == *"$state"* ]] ||
            [[ "$expected" == no && "$observed" != *"$state"* ]]; then return 0; fi
        sleep 0.05
    done
    echo "$window state was $observed; expected $state=$expected" >&2
    return 1
}

wait_for_workspace() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_DESKTOP)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$window workspace was $observed, expected $expected" >&2
    return 1
}

wait_for_file() {
    local path=$1
    for _ in $(seq 1 50); do
        if [[ -s "$path" ]]; then return 0; fi
        sleep 0.05
    done
    echo "$path was not created" >&2
    return 1
}

wait_for_active() {
    local expected=${1#0x}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>&1 || true)
        if grep -qi "window id # 0x$expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $1" >&2
    return 1
}

wait_for_current_workspace() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
        if grep -q "= $expected$" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "current workspace was $observed, expected $expected" >&2
    return 1
}

launch_client group-one
first_window=$launched_window
launch_client group-two
second_window=$launched_window
launch_client other
other_window=$launched_window

DISPLAY="$display" "$test_dir/set-input-focus" "$first_window"
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW | grep -qi "${first_window#0x}"; then
        break
    fi
    sleep 0.05
done

press F1
wait_for_state "$first_window" _NET_WM_STATE_SHADED yes
press F2
wait_for_state "$first_window" _NET_WM_STATE_SHADED no

DISPLAY="$display" xprop -id "$first_window" -f WM_NAME 8s -set WM_NAME group-renamed
press F3
wait_for_state "$first_window" _NET_WM_STATE_ABOVE yes

press F4
wait_for_state "$first_window" _NET_WM_STATE_ABOVE yes
wait_for_state "$second_window" _NET_WM_STATE_ABOVE yes
wait_for_state "$other_window" _NET_WM_STATE_BELOW yes

press F5
wait_for_workspace "$first_window" 4294967295
wait_for_workspace "$second_window" 0
wait_for_workspace "$other_window" 0
wait_for_file "$test_dir/continued"
for window in "$first_window" "$second_window" "$other_window"; do
    if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$window"; then
        echo "Stop did not prevent the nested kill action for $window" >&2
        exit 1
    fi
done

press F6
wait_for_file "$test_dir/none"

press F7
wait_for_state "$first_window" _NET_WM_STATE_ABOVE no
wait_for_state "$first_window" _NET_WM_STATE_BELOW no
sleep 0.1
if [[ -e "$test_dir/after-stop" ]]; then
    echo "Stop did not terminate the enclosing action list" >&2
    exit 1
fi

press F9
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP | grep -q '= 1$'; then break; fi
    sleep 0.05
done
press F8
wait_for_file "$test_dir/active"

press F10
for _ in $(seq 1 50); do
    if grep -q 'debug action debug_message=conditional-debug-marker' "$test_dir/nobox.log"; then
        break
    fi
    sleep 0.05
done
if ! grep -q 'debug action debug_message=conditional-debug-marker' "$test_dir/nobox.log"; then
    echo "typed Debug action did not reach structured runtime logging" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

press a
wait_for_current_workspace 0
wait_for_active "$second_window"
press b
wait_for_current_workspace 1
press c
wait_for_current_workspace 1
wait_for_workspace "$second_window" 1
wait_for_active "$second_window"
press d
wait_for_state "$second_window" _NET_WM_STATE_HIDDEN yes
press c
wait_for_state "$second_window" _NET_WM_STATE_HIDDEN no
wait_for_active "$second_window"

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during conditional action checks" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
echo "X11 If/ForEach/Stop conditional action checks passed on $display"
