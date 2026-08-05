#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-stacking-actions.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 stacking-actions test"
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
cc "$source_dir/request-pager.c" -o "$test_dir/request-pager" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for stacking-actions tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<'EOF'
[focus]
raise_on_focus = false

[theme]
border_width = 0
titlebar_height = 24

[[keyboard.bindings]]
key = "W-F1"
action = { type = "raise_lower" }

[[keyboard.bindings]]
key = "W-F2"
action = { type = "shade_lower" }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "unshade_raise" }
EOF

display=
for number in $(seq 611 630); do
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
    DISPLAY="$display" "$test_dir/presentation-client" --title "$title" \
        >"$test_dir/$title.window" 2>"$test_dir/$title.log" &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 50); do
        launched_window=$(head -n 1 "$test_dir/$title.window" 2>/dev/null || true)
        if [[ -n "$launched_window" ]] && DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
            grep -qi "$launched_window"; then return 0; fi
        sleep 0.05
    done
    echo "$title did not map" >&2
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

wait_for_top_stacked() {
    local expected=${1,,}
    local observed=
    local top=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING)
        top=$(grep -o '0x[0-9a-fA-F]*' <<<"$observed" | tail -n 1)
        if [[ ${top,,} == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "top stacked window was $top, expected $expected: $observed" >&2
    return 1
}

wait_for_shade_state() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present && "$observed" == *'_NET_WM_STATE_SHADED'* ]]; then
            return 0
        fi
        if [[ "$expected" == absent && "$observed" != *'_NET_WM_STATE_SHADED'* ]]; then
            return 0
        fi
        sleep 0.05
    done
    echo "shade state was not $expected for $window: $observed" >&2
    return 1
}

launched_window=
launch_client stacking-actions-first
first_window=$launched_window
launch_client stacking-actions-second
second_window=$launched_window

DISPLAY="$display" "$test_dir/request-pager" geometry "$first_window" 1 xywh 100 100 300 200
DISPLAY="$display" "$test_dir/request-pager" geometry "$second_window" 1 xywh 150 150 300 200
DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_active "$first_window"
wait_for_top_stacked "$second_window"

DISPLAY="$display" "$test_dir/press-key" F1
wait_for_top_stacked "$first_window"
DISPLAY="$display" "$test_dir/press-key" F1
wait_for_top_stacked "$second_window"

DISPLAY="$display" "$test_dir/request-pager" geometry "$second_window" 1 xywh 500 350 200 100
sleep 0.1
DISPLAY="$display" "$test_dir/press-key" F1
wait_for_top_stacked "$second_window"

DISPLAY="$display" "$test_dir/press-key" F2
wait_for_shade_state "$first_window" present
wait_for_top_stacked "$second_window"
DISPLAY="$display" "$test_dir/press-key" F3
wait_for_shade_state "$first_window" absent
wait_for_top_stacked "$second_window"
DISPLAY="$display" "$test_dir/press-key" F3
wait_for_top_stacked "$first_window"
DISPLAY="$display" "$test_dir/press-key" F2
wait_for_shade_state "$first_window" present
wait_for_top_stacked "$first_window"
DISPLAY="$display" "$test_dir/press-key" F2
wait_for_top_stacked "$second_window"

echo "X11 adaptive and shade-composite stacking actions passed on $display"
