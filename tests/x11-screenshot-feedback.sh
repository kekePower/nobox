#!/usr/bin/env bash
set -euo pipefail
wm_binary=${1:?window manager required}
screenshot_binary=${2:?screenshot binary required}
wm_mode=${3:-nobox}
for dependency in cc xdpyinfo xprop Xvfb; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for screenshot feedback"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
# Never send test shutter sounds to the login session's audio server.
mkdir "$test_dir/bin"
printf '#!/bin/sh\nexit 0\n' >"$test_dir/bin/canberra-gtk-play"
chmod +x "$test_dir/bin/canberra-gtk-play"
export PATH="$test_dir/bin:$PATH"
wm_pid= xserver_pid=
cleanup() {
    if [[ -n "$wm_pid" ]]; then kill "$wm_pid" 2>/dev/null || true; wait "$wm_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; wait "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM
cc -std=c11 -Wall -Wextra -Werror "$(dirname "$0")/screenshot-feedback.c" \
    -o "$test_dir/probe" -lX11 -lXext -lXtst
display=
for number in $(seq 971 990); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then display=":$number"; break; fi
done
[[ -n "$display" ]]
Xvfb "$display" -screen 0 800x600x24 -ac >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
if [[ "$wm_mode" == --openbox ]]; then
    cat >"$test_dir/rc.xml" <<EOF_CONFIG
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <keyboard><keybind key="A-Print"><action name="Execute"><command>"$screenshot_binary" -w --file "$test_dir/shot.png"</command></action></keybind></keyboard>
  <theme><name>Clearlooks</name><animateIconify>no</animateIconify></theme>
</openbox_config>
EOF_CONFIG
    DISPLAY="$display" "$wm_binary" --config-file "$test_dir/rc.xml" --sm-disable >"$test_dir/wm.log" 2>&1 &
else
    cat >"$test_dir/config.toml" <<EOF_CONFIG
[commands]
window_screenshot = '''"$screenshot_binary" -w --file "$test_dir/shot.png"'''
[panel]
enabled = false
EOF_CONFIG
    DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" "$wm_binary" run --no-autostart >"$test_dir/wm.log" 2>&1 &
fi
wm_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; then break; fi
    sleep 0.1
done
for mode in decorated fullscreen; do
    # Wait for withdrawal before the next connection can reuse client IDs.
    for _ in $(seq 1 100); do
        clients=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST)
        if [[ "$clients" == '_NET_CLIENT_LIST(WINDOW)'* && "$clients" != *0x* ]]; then break; fi
        sleep 0.02
    done
    [[ "$clients" == '_NET_CLIENT_LIST(WINDOW)'* && "$clients" != *0x* ]]
    if ! DISPLAY="$display" "$test_dir/probe" "$screenshot_binary" "$test_dir/shot.png" "$mode"; then
        tail -n 40 "$test_dir/wm.log" >&2
        exit 1
    fi
done
