#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-icons.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 icon test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

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

cc "$(dirname "$0")/icon-client.c" -o "$test_dir/icon-client" -lX11
cc "$(dirname "$0")/set-icon.c" -o "$test_dir/set-icon" -lX11

display=
for number in $(seq 251 270); do
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

DISPLAY="$display" RUST_LOG=nobox_x11=debug NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done

DISPLAY="$display" "$test_dir/icon-client" >"$test_dir/window" 2>&1 &
client_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/window" ]]; then break; fi
    sleep 0.05
done
read -r window <"$test_dir/window"

wait_for_log() {
    local pattern=$1
    for _ in $(seq 1 50); do
        if grep -q "$pattern" "$test_dir/nobox.log"; then return 0; fi
        sleep 0.05
    done
    echo "missing icon log pattern: $pattern" >&2
    cat "$test_dir/nobox.log" >&2
    return 1
}

for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null | grep -qi "$window"; then
        break
    fi
    sleep 0.05
done
wait_for_log 'width=32 height=32 pixels=1024'

DISPLAY="$display" "$test_dir/set-icon" "$window" 16
wait_for_log 'width=16 height=16 pixels=256'
DISPLAY="$display" "$test_dir/set-icon" "$window" malformed
wait_for_log 'cleared client icon metadata'
DISPLAY="$display" "$test_dir/set-icon" "$window" 24
wait_for_log 'width=24 height=24 pixels=576'
clears_before=$(grep -c 'cleared client icon metadata' "$test_dir/nobox.log")
DISPLAY="$display" "$test_dir/set-icon" "$window" delete
for _ in $(seq 1 50); do
    clears_after=$(grep -c 'cleared client icon metadata' "$test_dir/nobox.log")
    if (( clears_after > clears_before )); then break; fi
    sleep 0.05
done
if (( clears_after <= clears_before )); then
    echo "deleting _NET_WM_ICON did not clear cached metadata" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_SUPPORTED | grep -q '_NET_WM_ICON'; then
    echo "_NET_WM_ICON was not advertised" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited while processing icon replacements" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "Bounded initial and live X11 icon metadata passed on $display"
