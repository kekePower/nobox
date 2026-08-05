#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-workspace-actions.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 workspace-actions test"
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

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for workspace-actions tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<'EOF'
[focus]
focus_new = true
raise_on_focus = false

[workspaces]
names = ["one", "two", "three"]

[[keyboard.bindings]]
key = "W-F1"
action = { type = "next_workspace" }

[[keyboard.bindings]]
key = "W-F2"
action = { type = "last_workspace" }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "move_to_last_workspace" }

[[keyboard.bindings]]
key = "W-F4"
action = { type = "add_workspace", at = "current" }

[[keyboard.bindings]]
key = "W-F5"
action = { type = "remove_workspace", at = "current" }

[[keyboard.bindings]]
key = "W-F6"
action = { type = "add_workspace" }

[[keyboard.bindings]]
key = "W-F7"
action = { type = "remove_workspace" }
EOF

display=
for number in $(seq 651 670); do
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
    local output=$test_dir/$title.window
    DISPLAY="$display" "$test_dir/presentation-client" --title "$title" \
        >"$output" 2>"$test_dir/$title.log" &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 50); do
        launched_window=$(head -n 1 "$output" 2>/dev/null || true)
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

wait_for_current() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "current desktop was $observed, expected $expected" >&2
    return 1
}

wait_for_active() {
    local expected=$1
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)
        if grep -qi "window id # $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $expected" >&2
    return 1
}

wait_for_no_active() {
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>&1 || true)
        if [[ "$observed" == *'not found'* ]]; then return 0; fi
        sleep 0.05
    done
    echo "an active window remained: $observed" >&2
    return 1
}

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
    echo "$window map state was $observed, expected $expected" >&2
    return 1
}

wait_for_client_workspace() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_DESKTOP)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$window desktop was $observed, expected $expected" >&2
    return 1
}

wait_for_workspace_set() {
    local count=$1
    local current=$2
    local names=$3
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_NUMBER_OF_DESKTOPS \
            _NET_CURRENT_DESKTOP _NET_DESKTOP_NAMES)
        if grep -q "_NET_NUMBER_OF_DESKTOPS(CARDINAL) = $count" <<<"$observed" &&
            grep -q "_NET_CURRENT_DESKTOP(CARDINAL) = $current" <<<"$observed" &&
            grep -Fq "_NET_DESKTOP_NAMES(UTF8_STRING) = $names" <<<"$observed"; then
            return 0
        fi
        sleep 0.05
    done
    echo "workspace set was inconsistent: $observed" >&2
    return 1
}

wait_for_workspace_set 3 0 '"one", "two", "three"'
launch_client workspace-actions-first
first_window=$launched_window
wait_for_active "$first_window"

press F1
wait_for_current 1
launch_client workspace-actions-second
second_window=$launched_window
wait_for_active "$second_window"

press F2
wait_for_current 0
wait_for_active "$first_window"
press F2
wait_for_current 1
wait_for_active "$second_window"

press F3
wait_for_current 0
wait_for_client_workspace "$second_window" 0
wait_for_map_state "$second_window" IsViewable
wait_for_active "$second_window"

press F4
wait_for_workspace_set 4 0 '"4", "one", "two", "three"'
wait_for_client_workspace "$first_window" 1
wait_for_client_workspace "$second_window" 1
wait_for_map_state "$first_window" IsUnviewable
wait_for_map_state "$second_window" IsUnviewable
wait_for_no_active

press F5
wait_for_workspace_set 3 0 '"one", "two", "three"'
wait_for_client_workspace "$first_window" 0
wait_for_client_workspace "$second_window" 0
wait_for_map_state "$first_window" IsViewable
wait_for_map_state "$second_window" IsViewable
wait_for_active "$second_window"

press F6
wait_for_workspace_set 4 0 '"one", "two", "three", "4"'
press F7
wait_for_workspace_set 3 0 '"one", "two", "three"'
press F7
wait_for_workspace_set 2 0 '"one", "two"'
press F7
wait_for_workspace_set 1 0 '"one"'
press F7
wait_for_workspace_set 1 0 '"one"'

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during runtime workspace action checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 last-workspace and runtime workspace lifecycle actions passed on $display"
