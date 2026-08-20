#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-execute.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 execute test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
xterm_pid=
cleanup() {
    if [[ -n "$xterm_pid" ]]; then kill "$xterm_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/startup-client.c" -o "$test_dir/startup-client" -lX11
cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lXtst -lX11

display=
for number in $(seq 411 430); do
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

cat >"$test_dir/config.toml" <<EOF
[workspaces]
names = ["one", "two"]

[commands]
terminal = "touch $test_dir/terminal-command"
screenshot = "touch $test_dir/screen-command"
window_screenshot = "touch $test_dir/window-command"
session = "touch $test_dir/session-command"

[shortcuts]
terminal = "W-F5"
screenshot = "W-F6"
window_screenshot = "W-F7"

[[keyboard.bindings]]
key = "W-Right"
action = { type = "next_workspace" }

[[keyboard.bindings]]
key = "W-F9"
action = { type = "execute", command = "$test_dir/startup-client $test_dir/result \$pid \$wid \$pointer", prompt = "Launch the startup client?", startup_notify = { name = "Startup test", icon = "utilities-terminal", wm_class = "NoboxStartupTest" } }

[[keyboard.bindings]]
key = "W-F8"
action = { type = "session_logout", prompt = false }
EOF

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

DISPLAY="$display" "$test_dir/press-key" F5
DISPLAY="$display" "$test_dir/press-key" F6
DISPLAY="$display" "$test_dir/press-key" F7
DISPLAY="$display" "$test_dir/press-key" F8
for _ in $(seq 1 40); do
    if [[ -e "$test_dir/terminal-command" && -e "$test_dir/screen-command" &&
          -e "$test_dir/window-command" && -e "$test_dir/session-command" ]]; then
        break
    fi
    sleep 0.05
done
for result in terminal-command screen-command window-command session-command; do
    if [[ ! -e "$test_dir/$result" ]]; then
        echo "configured semantic command did not run: $result" >&2
        cat "$test_dir/nobox.log" >&2
        exit 1
    fi
done

DISPLAY="$display" "$test_dir/press-key" Right
DISPLAY="$display" xterm -title nobox-execute-target -geometry 30x8+30+40 \
    >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
xterm_window=
for _ in $(seq 1 60); do
    for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' || true); do
        if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
            grep -q 'nobox-execute-target'; then
            xterm_window=$candidate
        fi
    done
    if [[ -n "$xterm_window" ]] && DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
        grep -qi "window id # $xterm_window"; then break; fi
    sleep 0.05
done
if [[ -z "$xterm_window" ]]; then
    echo "execute target did not become active" >&2
    exit 1
fi
xterm_net_pid=$(DISPLAY="$display" xprop -id "$xterm_window" _NET_WM_PID |
    sed -n 's/.*= *\([0-9][0-9]*\).*/\1/p')
if [[ -z "$xterm_net_pid" ]]; then
    echo "execute target did not publish _NET_WM_PID" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" F9
sleep 0.1
if [[ -e "$test_dir/result" ]]; then
    echo "prompted Execute launched before confirmation" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/press-key" --plain Down
DISPLAY="$display" "$test_dir/press-key" --plain Return
for _ in $(seq 1 80); do
    if [[ -s "$test_dir/result" ]]; then break; fi
    sleep 0.05
done
if [[ ! -s "$test_dir/result" ]]; then
    echo "confirmed Execute did not launch" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi

grep -Eq '^startup_id=.+_TIME[0-9]+$' "$test_dir/result"
grep -q '^desktop=1$' "$test_dir/result"
grep -q "^pid=$xterm_net_pid$" "$test_dir/result"
grep -q "^wid=$((xterm_window))$" "$test_dir/result"
grep -Eq '^pointer=-?[0-9]+ -?[0-9]+$' "$test_dir/result"
startup_window=$(sed -n 's/^window=//p' "$test_dir/result")
startup_id=$(sed -n 's/^startup_id=//p' "$test_dir/result")
DISPLAY="$display" xprop -id "$startup_window" _NET_STARTUP_ID |
    grep -Fq "$startup_id"

kill "$nobox_pid"
wait "$nobox_pid"
nobox_pid=
echo "X11 configured commands, Execute confirmation, context expansion, startup environment, and workspace placement passed"
