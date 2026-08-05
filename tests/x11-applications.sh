#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-applications.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 application-rule test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 application-rule test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
xterm_pid=
client_pids=()
cleanup() {
    for client_pid in "${client_pids[@]}"; do
        kill "$client_pid" 2>/dev/null || true
    done
    if [[ -n "$xterm_pid" ]]; then kill "$xterm_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/application-client.c" -o "$test_dir/application-client" -lX11

display=
for number in $(seq 171 190); do
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

cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["main", "web", "rules", "chat"]

[[applications]]
match = { name = "nobox-*", class = "ruleclient", group_name = "nobox-suite", group_class = "rulegroup", role = "editor", title = "nobox rule ?ialog", kind = "dialog" }
workspace = 3
layer = "above"
decorated = false
focus = false
skip_pager = true
skip_taskbar = true
position = { x = 140, y = 160 }
size = { width = "50%", height = 180 }

[[applications]]
match = { group_name = "nobox-suite", group_class = "rulegroup", role = "state" }
workspace = "all"
focus = false
minimized = true
shaded = true
skip_pager = true
skip_taskbar = true
maximized = "vertical"

[[applications]]
match = { group_name = "nobox-suite", group_class = "rulegroup", role = "fullscreen" }
focus = false
fullscreen = true

[[applications]]
match = { group_name = "nobox-suite", group_class = "rulegroup", role = "preserve" }
focus = false
decorated = false
position = { x = 500, y = 420 }
size = { width = 240, height = 100 }

[[applications]]
match = { group_name = "nobox-suite", group_class = "rulegroup", role = "force" }
focus = false
decorated = false
position = { x = 500, y = 420, force = true }
size = { width = 240, height = 100 }
EOF

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

DISPLAY="$display" xterm -title nobox-rule-baseline -geometry 30x8+30+40 \
    >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
baseline_window=
for _ in $(seq 1 40); do
    for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -o '0x[0-9a-fA-F]*'); do
        if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
            grep -q 'nobox-rule-baseline'; then
            baseline_window=$candidate
        fi
    done
    if [[ -n "$baseline_window" ]] && DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
        grep -qi "window id # $baseline_window"; then break; fi
    sleep 0.05
done
if [[ -z "$baseline_window" ]]; then
    echo "baseline client did not become active" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/application-client" >"$test_dir/client-window" 2>&1 &
client_pid=$!
client_pids+=("$client_pid")
rule_window=
for _ in $(seq 1 40); do
    if [[ -s "$test_dir/client-window" ]]; then
        rule_window=$(head -n 1 "$test_dir/client-window")
    fi
    if [[ -n "$rule_window" ]] && DISPLAY="$display" xprop -id "$rule_window" \
        _NET_WM_DESKTOP >/dev/null 2>&1; then break; fi
    sleep 0.05
done
if [[ -z "$rule_window" ]]; then
    echo "application-rule client did not map" >&2
    exit 1
fi

properties=$(DISPLAY="$display" xprop -id "$rule_window" \
    _NET_WM_DESKTOP _NET_WM_STATE _NET_FRAME_EXTENTS)
for expected in \
    '_NET_WM_DESKTOP(CARDINAL) = 2' \
    '_NET_FRAME_EXTENTS(CARDINAL) = 0, 0, 0, 0'; do
    if ! grep -Fq "$expected" <<<"$properties"; then
        echo "application rule did not apply: $expected" >&2
        echo "$properties" >&2
        exit 1
    fi
done
for expected in _NET_WM_STATE_ABOVE _NET_WM_STATE_SKIP_PAGER \
    _NET_WM_STATE_SKIP_TASKBAR; do
    if ! grep -Fq "$expected" <<<"$properties"; then
        echo "application rule did not apply state: $expected" >&2
        echo "$properties" >&2
        exit 1
    fi
done
if ! DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
    grep -qi "window id # $baseline_window"; then
    echo "focus=false application rule stole focus" >&2
    exit 1
fi
rule_window_info=$(DISPLAY="$display" xwininfo -id "$rule_window")
for expected in \
    'Absolute upper-left X:  140' \
    'Absolute upper-left Y:  160' \
    'Width: 400' \
    'Height: 180'; do
    if ! grep -Fq "$expected" <<<"$rule_window_info"; then
        echo "application placement/size rule did not apply: $expected" >&2
        echo "$rule_window_info" >&2
        exit 1
    fi
done
if ! grep -Eq 'Map State: IsUn(viewable|Mapped)' <<<"$rule_window_info"; then
    echo "workspace application rule did not hide the client on another workspace" >&2
    echo "$rule_window_info" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/application-client" state "nobox state dialog" \
    >"$test_dir/state-window" 2>&1 &
client_pids+=("$!")
state_window=
for _ in $(seq 1 40); do
    if [[ -s "$test_dir/state-window" ]]; then
        state_window=$(head -n 1 "$test_dir/state-window")
    fi
    if [[ -n "$state_window" ]] && DISPLAY="$display" xprop -id "$state_window" \
        _NET_WM_STATE >/dev/null 2>&1; then break; fi
    sleep 0.05
done
if [[ -z "$state_window" ]]; then
    echo "application state-rule client did not map" >&2
    exit 1
fi
state_properties=$(DISPLAY="$display" xprop -id "$state_window" \
    _NET_WM_DESKTOP _NET_WM_STATE WM_STATE)
if ! grep -Fq '_NET_WM_DESKTOP(CARDINAL) = 4294967295' <<<"$state_properties"; then
    echo "sticky application rule did not apply" >&2
    echo "$state_properties" >&2
    exit 1
fi
for expected in _NET_WM_STATE_HIDDEN _NET_WM_STATE_SHADED \
    _NET_WM_STATE_SKIP_PAGER _NET_WM_STATE_SKIP_TASKBAR \
    _NET_WM_STATE_MAXIMIZED_VERT; do
    if ! grep -Fq "$expected" <<<"$state_properties"; then
        echo "application state rule did not apply: $expected" >&2
        echo "$state_properties" >&2
        exit 1
    fi
done

DISPLAY="$display" "$test_dir/application-client" fullscreen "nobox fullscreen dialog" \
    >"$test_dir/fullscreen-window" 2>&1 &
client_pids+=("$!")
fullscreen_window=
for _ in $(seq 1 40); do
    if [[ -s "$test_dir/fullscreen-window" ]]; then
        fullscreen_window=$(head -n 1 "$test_dir/fullscreen-window")
    fi
    if [[ -n "$fullscreen_window" ]] && DISPLAY="$display" xprop -id "$fullscreen_window" \
        _NET_WM_STATE 2>/dev/null | grep -q '_NET_WM_STATE_FULLSCREEN'; then break; fi
    sleep 0.05
done
if [[ -z "$fullscreen_window" ]] || ! DISPLAY="$display" xprop -id "$fullscreen_window" \
    _NET_WM_STATE | grep -q '_NET_WM_STATE_FULLSCREEN'; then
    echo "fullscreen application rule did not apply" >&2
    exit 1
fi

for role in preserve force; do
    DISPLAY="$display" "$test_dir/application-client" "$role" "nobox $role dialog" positioned \
        >"$test_dir/$role-window" 2>&1 &
    client_pids+=("$!")
    client_window=
    for _ in $(seq 1 40); do
        if [[ -s "$test_dir/$role-window" ]]; then
            client_window=$(head -n 1 "$test_dir/$role-window")
        fi
        if [[ -n "$client_window" ]] && DISPLAY="$display" xprop -id "$client_window" \
            _NET_FRAME_EXTENTS >/dev/null 2>&1; then break; fi
        sleep 0.05
    done
    client_info=$(DISPLAY="$display" xwininfo -id "$client_window")
    expected_x=80
    expected_y=80
    if [[ "$role" == force ]]; then
        expected_x=500
        expected_y=420
    fi
    for expected in \
        "Absolute upper-left X:  $expected_x" \
        "Absolute upper-left Y:  $expected_y" \
        'Width: 240' \
        'Height: 100'; do
        if ! grep -Fq "$expected" <<<"$client_info"; then
            echo "application $role position-hint policy failed: $expected" >&2
            echo "$client_info" >&2
            exit 1
        fi
    done
done

echo "X11 application identity, group, geometry, and initial policy rules passed on $display"
