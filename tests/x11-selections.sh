#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-selections.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 selection test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 selection test"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
owner_pid=
client_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$owner_pid" ]]; then kill "$owner_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/selection-client.c" -o "$test_dir/selection-client" -lX11
cc "$(dirname "$0")/replace-wm-selection.c" \
    -o "$test_dir/replace-wm-selection" -lX11
cc "$(dirname "$0")/placement-client.c" -o "$test_dir/placement-client" -lX11
printf '%s\n' \
    '[focus]' \
    'focus_new = false' \
    'follow_mouse = false' \
    'raise_on_focus = false' >"$test_dir/config.toml"

display=
for number in $(seq 351 370); do
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

DISPLAY="$display" "$test_dir/selection-client" own clipboard-survives \
    >"$test_dir/owner.log" 2>&1 &
owner_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^owner ' "$test_dir/owner.log" 2>/dev/null; then break; fi
    sleep 0.05
done
selection_owner=$(awk '/^owner / { print $2; exit }' "$test_dir/owner.log")
if [[ -z "$selection_owner" ]]; then
    echo "selection owner did not start" >&2
    exit 1
fi

request() {
    DISPLAY="$display" "$test_dir/selection-client" request "$1" "$2"
}

[[ $(request CLIPBOARD owner) == "$selection_owner" ]]
[[ $(request PRIMARY owner) == "$selection_owner" ]]
[[ $(request CLIPBOARD text) == clipboard-survives ]]
[[ $(request PRIMARY text) == clipboard-survives ]]

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox did not start" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi

[[ $(request CLIPBOARD owner) == "$selection_owner" ]]
[[ $(request PRIMARY owner) == "$selection_owner" ]]
[[ $(request CLIPBOARD text) == clipboard-survives ]]
[[ $(request WM_S0 targets) == 'targets ok' ]]
[[ $(request WM_S0 timestamp) == timestamp\ * ]]
[[ $(request WM_S0 multiple) == 'multiple ok' ]]

DISPLAY="$display" "$test_dir/placement-client" handover positioned \
    >"$test_dir/client.log" 2>&1 &
client_pid=$!
handover_client=
for _ in $(seq 1 50); do
    handover_client=$(awk 'NR == 1 { print; exit }' "$test_dir/client.log" 2>/dev/null || true)
    if [[ -n "$handover_client" ]] &&
        DISPLAY="$display" xprop -id "$handover_client" _NET_FRAME_EXTENTS >/dev/null 2>&1; then
        break
    fi
    sleep 0.05
done
if [[ -z "$handover_client" ]]; then
    echo "handover client did not map" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/replace-wm-selection" "$handover_client" \
    >"$test_dir/replacement.log"
if ! grep -q '^handover ok$' "$test_dir/replacement.log"; then
    echo "nobox did not complete the WM selection handover" >&2
    exit 1
fi
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox did not exit after losing WM_S0" >&2
    exit 1
fi
wait "$nobox_pid"
nobox_pid=

[[ $(request CLIPBOARD owner) == "$selection_owner" ]]
[[ $(request PRIMARY owner) == "$selection_owner" ]]
[[ $(request CLIPBOARD text) == clipboard-survives ]]
[[ $(request PRIMARY text) == clipboard-survives ]]

echo "X11 manager-selection and clipboard-coexistence checks passed"
