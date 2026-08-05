#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-doctor.sh /path/to/nobox}
for dependency in xdpyinfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for X11 doctor tests"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for X11 doctor tests"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 221 240); do
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

: >"$test_dir/config.toml"
XDG_STATE_HOME="$test_dir/state" "$nobox_binary" --config "$test_dir/config.toml" \
    doctor --display "$display" >"$test_dir/ready.log"
for expected in \
    '[ok] config:' \
    '[ok] X11:' \
    '[ok] output 1:' \
    '[ok] X11 font: fixed' \
    '[ok] window-manager selection: available' \
    'ready: yes'; do
    if ! grep -Fq "$expected" "$test_dir/ready.log"; then
        echo "doctor omitted readiness fact: $expected" >&2
        exit 1
    fi
done

DISPLAY="$display" XDG_STATE_HOME="$test_dir/state" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
sleep 0.4
XDG_STATE_HOME="$test_dir/state" "$nobox_binary" --config "$test_dir/config.toml" \
    doctor --display "$display" >"$test_dir/owned.log"
if ! grep -q 'another window manager owns this screen' "$test_dir/owned.log" \
    || ! grep -q 'ready: yes' "$test_dir/owned.log"; then
    echo "doctor did not report a non-blocking existing WM owner" >&2
    exit 1
fi

printf '[theme]\nfont = "nobox-font-that-does-not-exist"\n' >"$test_dir/bad-font.toml"
if XDG_STATE_HOME="$test_dir/state" "$nobox_binary" --config "$test_dir/bad-font.toml" \
    doctor --display "$display" >"$test_dir/bad-font.log" 2>&1; then
    echo "doctor accepted an unavailable configured font" >&2
    exit 1
fi
if ! grep -q 'X11 font is unavailable' "$test_dir/bad-font.log" \
    || ! grep -q 'ready: no' "$test_dir/bad-font.log"; then
    echo "doctor did not explain the unavailable font" >&2
    exit 1
fi

printf 'unknown_key = true\n' >"$test_dir/invalid.toml"
if XDG_STATE_HOME="$test_dir/state" "$nobox_binary" --config "$test_dir/invalid.toml" \
    doctor --display "$display" >"$test_dir/invalid.log" 2>&1; then
    echo "doctor accepted invalid configuration" >&2
    exit 1
fi
if ! grep -q '\[error\] config:' "$test_dir/invalid.log"; then
    echo "doctor did not explain invalid configuration" >&2
    exit 1
fi

echo "Read-only X11 doctor readiness and failure diagnostics passed on $display"
