#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-workspaces.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 workspace test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 workspace test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
first_pid=
second_pid=
cleanup() {
    if [[ -n "$first_pid" ]]; then kill "$first_pid" 2>/dev/null || true; fi
    if [[ -n "$second_pid" ]]; then kill "$second_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/request-workspace.c" -o "$test_dir/request-workspace" -lX11

display=
for number in $(seq 151 170); do
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
sleep 0.4

root_properties=$(DISPLAY="$display" xprop -root _NET_NUMBER_OF_DESKTOPS \
    _NET_CURRENT_DESKTOP _NET_DESKTOP_NAMES _NET_DESKTOP_GEOMETRY \
    _NET_DESKTOP_VIEWPORT _NET_WORKAREA)
for expected in \
    '_NET_NUMBER_OF_DESKTOPS(CARDINAL) = 4' \
    '_NET_CURRENT_DESKTOP(CARDINAL) = 0' \
    '_NET_DESKTOP_NAMES(UTF8_STRING) = "1", "2", "3", "4"' \
    '_NET_DESKTOP_GEOMETRY(CARDINAL) = 800, 600' \
    '_NET_DESKTOP_VIEWPORT(CARDINAL) = 0, 0, 0, 0, 0, 0, 0, 0' \
    '_NET_WORKAREA(CARDINAL) = 0, 0, 800, 600, 0, 0, 800, 600, 0, 0, 800, 600, 0, 0, 800, 600'; do
    if ! grep -Fq "$expected" <<<"$root_properties"; then
        echo "missing workspace root property: $expected" >&2
        echo "$root_properties" >&2
        exit 1
    fi
done

DISPLAY="$display" xterm -title nobox-workspace-one -geometry 30x8+30+40 \
    >"$test_dir/first.log" 2>&1 &
first_pid=$!
DISPLAY="$display" xterm -title nobox-workspace-two -geometry 30x8+350+40 \
    >"$test_dir/second.log" 2>&1 &
second_pid=$!
first_window=
second_window=
for _ in $(seq 1 40); do
    for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -o '0x[0-9a-fA-F]*'); do
        title=$(DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null || true)
        if grep -q 'nobox-workspace-one' <<<"$title"; then first_window=$candidate; fi
        if grep -q 'nobox-workspace-two' <<<"$title"; then second_window=$candidate; fi
    done
    if [[ -n "$first_window" && -n "$second_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$first_window" || -z "$second_window" ]]; then
    echo "workspace clients did not map" >&2
    exit 1
fi

map_state() {
    DISPLAY="$display" xwininfo -id "$1" |
        awk -F: '/Map State:/ { gsub(/ /, "", $2); print $2; exit }'
}

wait_for_state() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(map_state "$window")
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "window $window map state was $observed, expected $expected" >&2
    return 1
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
    return 1
}

wait_for_wm_state() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" WM_STATE)
        if grep -q "window state: $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "window $window WM_STATE was $observed, expected $expected" >&2
    return 1
}

DISPLAY="$display" "$test_dir/request-workspace" move "$second_window" 1
wait_for_state "$first_window" IsViewable
wait_for_state "$second_window" IsUnviewable
wait_for_wm_state "$first_window" Normal
wait_for_wm_state "$second_window" Iconic
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/request-workspace" current 1
wait_for_state "$first_window" IsUnviewable
wait_for_state "$second_window" IsViewable
wait_for_wm_state "$first_window" Iconic
wait_for_wm_state "$second_window" Normal
wait_for_active "$second_window"

DISPLAY="$display" "$test_dir/request-workspace" current 0
wait_for_state "$first_window" IsViewable
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/request-workspace" move "$first_window" 1
wait_for_state "$first_window" IsUnviewable
if ! DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW | grep -q 'not found'; then
    echo "moving the final visible client did not clear focus" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-workspace" current 1
wait_for_state "$first_window" IsViewable
wait_for_state "$second_window" IsViewable
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/request-workspace" move "$second_window" all
if ! DISPLAY="$display" xprop -id "$second_window" _NET_WM_DESKTOP | grep -q '= 4294967295'; then
    echo "sticky desktop assignment was not published" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-workspace" current 0
wait_for_state "$first_window" IsUnviewable
wait_for_state "$second_window" IsViewable
wait_for_active "$second_window"

cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["main", "web"]
EOF
kill -HUP "$nobox_pid"
for _ in $(seq 1 40); do
    reloaded=$(DISPLAY="$display" xprop -root _NET_NUMBER_OF_DESKTOPS _NET_DESKTOP_NAMES)
    if grep -q '= 2' <<<"$reloaded" && grep -q '"main", "web"' <<<"$reloaded"; then break; fi
    sleep 0.05
done
if ! grep -q '= 2' <<<"$reloaded" || ! grep -q '"main", "web"' <<<"$reloaded"; then
    echo "workspace configuration did not reload: $reloaded" >&2
    exit 1
fi

cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["only"]
EOF
kill -HUP "$nobox_pid"
for _ in $(seq 1 40); do
    reloaded=$(DISPLAY="$display" xprop -root _NET_NUMBER_OF_DESKTOPS \
        _NET_CURRENT_DESKTOP _NET_DESKTOP_NAMES _NET_WORKAREA)
    if grep -q '_NET_NUMBER_OF_DESKTOPS(CARDINAL) = 1' <<<"$reloaded"; then break; fi
    sleep 0.05
done
for expected in \
    '_NET_NUMBER_OF_DESKTOPS(CARDINAL) = 1' \
    '_NET_CURRENT_DESKTOP(CARDINAL) = 0' \
    '_NET_DESKTOP_NAMES(UTF8_STRING) = "only"' \
    '_NET_WORKAREA(CARDINAL) = 0, 0, 800, 600'; do
    if ! grep -Fq "$expected" <<<"$reloaded"; then
        echo "shrunk workspace set is inconsistent: $reloaded" >&2
        exit 1
    fi
done
wait_for_state "$first_window" IsViewable
wait_for_state "$second_window" IsViewable
if ! DISPLAY="$display" xprop -id "$first_window" _NET_WM_DESKTOP | grep -q '= 0'; then
    echo "client on removed workspace was not moved to the survivor" >&2
    exit 1
fi

echo "X11 workspace switching, moves, focus history, stickiness, and reload passed on $display"
