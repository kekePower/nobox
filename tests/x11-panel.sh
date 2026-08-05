#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-panel.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the optional panel test"
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

if ! cc "$(dirname "$0")/pointer-gesture.c" -o "$test_dir/pointer-gesture" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for panel interaction tests"
    exit 77
fi

write_config() {
    local position=$1
    local height=$2
    cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = true
position = "$position"
height = $height

[workspaces]
names = ["one", "two", "three"]
EOF
}
write_config bottom 34

display=
for number in $(seq 751 770); do
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
    RUST_LOG=nobox=debug "$nobox_binary" run --no-autostart \
    >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!

find_panel() {
    panel_window=
    for _ in $(seq 1 80); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_CLASS 2>/dev/null |
                grep -qi 'nobox-panel'; then
                panel_window=$candidate
                return 0
            fi
        done
        sleep 0.05
    done
    echo "optional panel did not become managed" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    return 1
}

assert_panel() {
    local y=$1
    local height=$2
    local strut=$3
    local workarea=$4
    find_panel
    local info properties observed=
    info=$(DISPLAY="$display" xwininfo -id "$panel_window")
    for expected in "Absolute upper-left X:  0" "Absolute upper-left Y:  $y" \
        "Width: 800" "Height: $height"; do
        if ! grep -q "$expected" <<<"$info"; then
            echo "panel geometry did not contain '$expected'" >&2
            echo "$info" >&2
            return 1
        fi
    done
    properties=$(DISPLAY="$display" xprop -id "$panel_window" \
        _NET_WM_WINDOW_TYPE _NET_WM_STRUT_PARTIAL)
    if ! grep -q '_NET_WM_WINDOW_TYPE_DOCK' <<<"$properties" ||
        ! grep -q "= $strut" <<<"$properties"; then
        echo "panel did not publish its dock type and strut: $properties" >&2
        return 1
    fi
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_WORKAREA 2>/dev/null || true)
        if grep -q "$workarea" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "panel strut was not reflected in work area: $observed" >&2
    return 1
}

assert_panel 566 34 '0, 0, 0, 34, 0, 0, 0, 0, 0, 0, 0, 799' \
    '= 0, 0, 800, 566'
write_config top 28
kill -HUP "$nobox_pid"
assert_panel 0 28 '0, 0, 28, 0, 0, 0, 0, 0, 0, 799, 0, 0' \
    '= 0, 28, 800, 572'

panel_state=
for _ in $(seq 1 50); do
    panel_state=$(DISPLAY="$display" xprop -id "$panel_window" \
        _NOBOX_PANEL_WORKSPACE_COUNT _NOBOX_PANEL_TASK_COUNT \
        _NOBOX_PANEL_CLOCK 2>/dev/null || true)
    if grep -q '_NOBOX_PANEL_WORKSPACE_COUNT(CARDINAL) = 3' <<<"$panel_state" &&
        grep -q '_NOBOX_PANEL_TASK_COUNT(CARDINAL) = 0' <<<"$panel_state" &&
        grep -Eq '_NOBOX_PANEL_CLOCK\(UTF8_STRING\) = "[0-9]{2}:[0-9]{2}"' \
            <<<"$panel_state"; then
        break
    fi
    sleep 0.1
done
if ! grep -q '_NOBOX_PANEL_WORKSPACE_COUNT(CARDINAL) = 3' <<<"$panel_state"; then
    echo "panel did not publish its workspace/task/clock state: $panel_state" >&2
    exit 1
fi

# With the fixed test font, the second named workspace occupies x=38..67.
DISPLAY="$display" "$test_dir/pointer-gesture" "$panel_window" 1 click 45 14 0 0
desktop=
for _ in $(seq 1 50); do
    desktop=$(DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP 2>/dev/null || true)
    if grep -q '= 1' <<<"$desktop"; then break; fi
    sleep 0.05
done
if ! grep -q '= 1' <<<"$desktop"; then
    echo "workspace button did not send a pager request: $desktop" >&2
    exit 1
fi

launch_client() {
    local title=$1
    DISPLAY="$display" xterm -title "$title" -geometry 30x8 \
        >"$test_dir/$title.log" 2>&1 &
    client_pids+=("$!")
}
launch_client panel-first
launch_client panel-second

tasks=
for _ in $(seq 1 50); do
    tasks=$(DISPLAY="$display" xprop -id "$panel_window" \
        _NOBOX_PANEL_TASK_COUNT 2>/dev/null || true)
    if grep -q '= 2' <<<"$tasks"; then break; fi
    sleep 0.1
done
if ! grep -q '= 2' <<<"$tasks"; then
    echo "panel did not show both current-workspace tasks: $tasks" >&2
    exit 1
fi

# Task order follows _NET_CLIENT_LIST; select its first non-panel client.
first_window=
for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
    grep -o '0x[0-9a-fA-F]*'); do
    if ! DISPLAY="$display" xprop -id "$candidate" WM_CLASS 2>/dev/null |
        grep -qi 'nobox-panel'; then
        first_window=$candidate
        break
    fi
done
if [[ -z "$first_window" ]]; then
    echo "panel task test client did not become managed" >&2
    exit 1
fi

# The first task begins after the three workspace buttons at x=106.
DISPLAY="$display" "$test_dir/pointer-gesture" "$panel_window" 1 click 120 14 0 0
active=
for _ in $(seq 1 50); do
    active=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null || true)
    if grep -qi "$first_window" <<<"$active"; then break; fi
    sleep 0.05
done
if ! grep -qi "$first_window" <<<"$active"; then
    echo "task button did not activate its client: $active" >&2
    exit 1
fi

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited while reconfiguring the optional panel" >&2
    exit 1
fi
echo "optional panel lifecycle, contents, and controls passed on $display"
