#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-restart.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 restart test"
        exit 77
    fi
done
if command -v Xnest >/dev/null 2>&1; then
    x_server=(Xnest)
    x_server_args=(-geometry 800x600 -depth 24 -ac)
elif command -v Xephyr >/dev/null 2>&1; then
    x_server=(Xephyr)
    x_server_args=(-screen 800x600x24 -ac)
elif command -v Xvfb >/dev/null 2>&1; then
    x_server=(Xvfb)
    x_server_args=(-screen 0 800x600x24 -ac)
else
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 restart test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

source_dir=$(dirname "$0")
cc "$source_dir/session-client.c" -o "$test_dir/session-client" -lX11
cc "$source_dir/selection-client.c" -o "$test_dir/selection-client" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for restart tests"
    exit 77
fi

cat >"$test_dir/config.toml" <<EOF
[focus]
focus_new = true
raise_on_focus = false

[workspaces]
names = ["one", "two"]

[[keyboard.bindings]]
key = "W-F2"
action = { type = "move_to_next_workspace", follow = true }

[[keyboard.bindings]]
key = "W-F3"
action = { type = "toggle_always_on_top" }

[[keyboard.bindings]]
key = "W-F8"
action = { type = "restart" }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "restart", command = "printf handoff > '$test_dir/handoff'" }
EOF
cat >"$test_dir/autostart" <<EOF
printf 'started\n' >>'$test_dir/autostart.log'
EOF

display=
for number in $(seq 671 690); do
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
    "$nobox_binary" run >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!

support_window() {
    DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        awk '/window id/ { print $NF; exit }'
}

initial_support=
for _ in $(seq 1 50); do
    initial_support=$(support_window)
    if [[ -n "$initial_support" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$initial_support" ]]; then
    echo "nobox did not claim the nested X11 server" >&2
    exit 1
fi
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/autostart.log" ]]; then break; fi
    sleep 0.05
done
if [[ $(wc -l <"$test_dir/autostart.log") -ne 1 ]]; then
    echo "autostart did not run exactly once on initial startup" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/session-client" restart-client persistent 80 90 \
    >"$test_dir/client.log" 2>&1 &
client_pid=$!
client_window=
for _ in $(seq 1 50); do
    client_window=$(awk 'NR == 1 { print; exit }' "$test_dir/client.log" 2>/dev/null || true)
    if [[ -n "$client_window" ]] &&
        DISPLAY="$display" xprop -id "$client_window" _NET_FRAME_EXTENTS >/dev/null 2>&1; then
        break
    fi
    sleep 0.05
done
if [[ -z "$client_window" ]]; then
    echo "restart test client did not map" >&2
    exit 1
fi
press() {
    DISPLAY="$display" "$test_dir/press-key" "$1"
}

wait_for_property() {
    local window=$1
    local property=$2
    local pattern=$3
    local observed=
    for _ in $(seq 1 100); do
        observed=$(DISPLAY="$display" xprop -id "$window" "$property" 2>&1 || true)
        if grep -q "$pattern" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$property was $observed, expected $pattern" >&2
    return 1
}

press F2
wait_for_property "$client_window" _NET_WM_DESKTOP '= 1$'
press F3
wait_for_property "$client_window" _NET_WM_STATE '_NET_WM_STATE_ABOVE'

press F8
for _ in $(seq 1 100); do
    restart_count=$(grep -c 'nobox owns the X11 root window' "$test_dir/nobox.log" || true)
    if [[ "$restart_count" -ge 2 ]]; then break; fi
    sleep 0.05
done
if [[ "$restart_count" -lt 2 ]]; then
    echo "self-restart did not start a fresh X11 backend" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
if [[ -z $(support_window) ]]; then
    echo "self-restart did not reclaim X11 ownership" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "self-restart exited the nobox process" >&2
    exit 1
fi

wait_for_property "$client_window" _NET_WM_DESKTOP '= 1$'
wait_for_property "$client_window" _NET_WM_STATE '_NET_WM_STATE_ABOVE'
if ! DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP | grep -q '= 1$'; then
    echo "self-restart did not restore the active workspace" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW | grep -qi "${client_window#0x}"; then
    echo "self-restart did not restore focus" >&2
    exit 1
fi
if [[ $(wc -l <"$test_dir/autostart.log") -ne 1 ]]; then
    echo "self-restart reran autostart" >&2
    exit 1
fi
if [[ ! -s "$test_dir/session.toml" ]]; then
    echo "self-restart did not persist session state" >&2
    exit 1
fi

press F9
if ! wait "$nobox_pid"; then
    echo "replacement command exited unsuccessfully" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
nobox_pid=
if [[ $(<"$test_dir/handoff") != handoff ]]; then
    echo "restart replacement command did not run" >&2
    exit 1
fi
if DISPLAY="$display" "$test_dir/selection-client" request WM_S0 owner >/dev/null 2>&1; then
    echo "replacement handoff retained the ICCCM manager selection" >&2
    exit 1
fi
for _ in $(seq 1 50); do
    if [[ -z $(support_window) ]]; then break; fi
    sleep 0.05
done
if [[ -n $(support_window) ]]; then
    echo "replacement handoff did not release X11 ownership" >&2
    DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK >&2 || true
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

echo "X11 self-restart and clean replacement handoff passed on $display"
