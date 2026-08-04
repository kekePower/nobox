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
client_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
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
match = { name = "nobox-*", class = "ruleclient", role = "editor", title = "nobox rule ?ialog", kind = "dialog" }
workspace = 3
layer = "above"
decorated = false
focus = false
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
    '_NET_WM_STATE(ATOM) = _NET_WM_STATE_ABOVE' \
    '_NET_FRAME_EXTENTS(CARDINAL) = 0, 0, 0, 0'; do
    if ! grep -Fq "$expected" <<<"$properties"; then
        echo "application rule did not apply: $expected" >&2
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
if ! grep -Eq 'Map State: IsUn(viewable|Mapped)' <<<"$rule_window_info"; then
    echo "workspace application rule did not hide the client on another workspace" >&2
    echo "$rule_window_info" >&2
    exit 1
fi

echo "X11 application identity and initial policy rules passed on $display"
