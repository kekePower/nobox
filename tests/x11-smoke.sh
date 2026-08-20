#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-smoke.sh /path/to/nobox}
for dependency in xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the nested X11 smoke test"
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

display=
for number in $(seq 90 110); do
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
if ! DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then
    echo "$nested_x_server did not become ready" >&2
    sed -n '1,120p' "$test_dir/xserver.log" >&2
    exit 1
fi

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then
        echo "nobox exited during startup" >&2
        sed -n '1,160p' "$test_dir/nobox.log" >&2
        exit 1
    fi
    if grep -q 'loaded X11 key bindings' "$test_dir/nobox.log" &&
        grep -q 'using X11 output topology' "$test_dir/nobox.log"; then
        break
    fi
    sleep 0.1
done
if ! grep -q 'loaded X11 key bindings' "$test_dir/nobox.log"; then
    echo "nobox did not load its default keyboard bindings" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi
if ! grep -q 'using X11 output topology' "$test_dir/nobox.log"; then
    echo "nobox did not initialize its output topology" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi

if DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/second-wm.log" 2>&1; then
    echo "a second window manager unexpectedly claimed the display" >&2
    exit 1
fi

DISPLAY="$display" xterm -title nobox-smoke -geometry 40x10+30+40 \
    >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
sleep 0.7

properties=$(DISPLAY="$display" xprop -root \
    _NET_SUPPORTING_WM_CHECK _NET_CLIENT_LIST _NET_ACTIVE_WINDOW)
if grep -q 'not found' <<<"$properties"; then
    echo "$properties" >&2
    exit 1
fi
if ! grep -q '_NET_CLIENT_LIST(WINDOW): window id #' <<<"$properties"; then
    echo "nobox did not publish the managed xterm" >&2
    echo "$properties" >&2
    exit 1
fi

echo "nested X11 smoke test passed on $display"
