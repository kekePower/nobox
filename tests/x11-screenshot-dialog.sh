#!/usr/bin/env bash
set -euo pipefail
wm_binary=${1:?window manager required}
screenshot_binary=${2:?screenshot binary required}
wm_mode=${3:-nobox}
for dependency in cc xdpyinfo xprop xwininfo xterm Xvfb python3; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for screenshot dialog tests"
        exit 77
    fi
done
if ! python3 -c 'from PIL import Image' 2>/dev/null; then
    echo 'SKIP: Pillow is required for screenshot pixel checks'
    exit 77
fi
source "$(dirname "$0")/nested-x.sh"
test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
export GTK_A11Y=none GSK_RENDERER=cairo
mkdir "$test_dir/bin"
export NOBOX_TEST_SOUND_LOG="$test_dir/sound.log"
printf '#!/bin/sh\necho played >>"$NOBOX_TEST_SOUND_LOG"\n' >"$test_dir/bin/canberra-gtk-play"
chmod +x "$test_dir/bin/canberra-gtk-play"
export PATH="$test_dir/bin:$PATH"
wm_pid= xserver_pid= client_pid= screenshot_pid=
cleanup() {
    for pid in "$screenshot_pid" "$client_pid" "$wm_pid" "$xserver_pid"; do
        if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
    done
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM
cc -std=c11 -Wall -Wextra -Werror "$(dirname "$0")/press-key.c" -o "$test_dir/key" -lX11 -lXtst
cc -std=c11 -Wall -Wextra -Werror "$(dirname "$0")/button-input.c" -o "$test_dir/pointer" -lX11 -lXtst
display=
for number in $(seq 991 1010); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then display=":$number"; break; fi
done
[[ -n "$display" ]]
Xvfb "$display" -screen 0 800x600x24 -ac >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
export DISPLAY="$display"
for _ in $(seq 1 50); do
    if xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
if [[ "$wm_mode" == --openbox ]]; then
    cat >"$test_dir/rc.xml" <<'EOF_CONFIG'
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <theme><name>Clearlooks</name><animateIconify>no</animateIconify></theme>
</openbox_config>
EOF_CONFIG
    "$wm_binary" --config-file "$test_dir/rc.xml" --sm-disable >"$test_dir/wm.log" 2>&1 &
else
    printf '[panel]\nenabled = false\n' >"$test_dir/config.toml"
    NOBOX_CONFIG_FILE="$test_dir/config.toml" "$wm_binary" run --no-autostart >"$test_dir/wm.log" 2>&1 &
fi
wm_pid=$!
for _ in $(seq 1 50); do
    if xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; then break; fi
    sleep 0.1
done
xterm -title screenshot-dialog-fixture -geometry 40x10+30+30 -e sleep 90 >"$test_dir/client.log" 2>&1 &
client_pid=$!
active_title() {
    local active
    active=$(xprop -root _NET_ACTIVE_WINDOW | sed -n 's/.*# //p')
    [[ -n "$active" && "$active" != 0x0 ]] || return 1
    xprop -id "$active" _NET_WM_NAME WM_NAME 2>/dev/null
}
wait_title() {
    for _ in $(seq 1 100); do
        if active_title | grep -Fq "\"$1\""; then return 0; fi
        sleep 0.05
    done
    cat "$test_dir/chooser.log" >&2 || true
    echo "did not focus $1" >&2
    return 1
}
wait_title screenshot-dialog-fixture
sleep 0.2
"$screenshot_binary" --window --no-flash --no-sound --file "$test_dir/window-baseline.png" >/dev/null
"$screenshot_binary" --no-sound --file "$test_dir/screen-baseline.png" >/dev/null
open_chooser() {
    # The explicit display must work even when DISPLAY and GDK_BACKEND are wrong.
    DISPLAY=:65530 GDK_BACKEND=wayland "$screenshot_binary" --display "$display" -i \
        --no-sound --no-flash "$@" >"$test_dir/chooser.log" 2>&1 &
    screenshot_pid=$!
    wait_title Screenshot
}
finish_capture() {
    "$test_dir/key" --alt t
    wait "$screenshot_pid"
    screenshot_pid=
    wait_title screenshot-dialog-fixture
}
open_chooser --file "$test_dir/cancel.png"
if [[ -n ${NOBOX_TEST_ARTIFACTS:-} ]]; then
    mkdir -p "$NOBOX_TEST_ARTIFACTS"
    "$screenshot_binary" --no-sound --file "$NOBOX_TEST_ARTIFACTS/screenshot-chooser.png" >/dev/null
fi
"$test_dir/key" --plain Escape
wait "$screenshot_pid"
screenshot_pid=
[[ ! -e "$test_dir/cancel.png" ]]
wait_title screenshot-dialog-fixture
open_chooser --file "$test_dir/window.png"
"$test_dir/key" --alt w
finish_capture
cmp "$test_dir/window-baseline.png" "$test_dir/window.png"
[[ ! -e "$NOBOX_TEST_SOUND_LOG" ]]
open_chooser --window --file "$test_dir/feedback.png"
"$test_dir/key" --alt n
"$test_dir/key" --alt o
finish_capture
[[ $(wc -l <"$NOBOX_TEST_SOUND_LOG") == 1 ]]
cmp "$test_dir/window-baseline.png" "$test_dir/feedback.png"
open_chooser --window --file "$test_dir/screen.png"
"$test_dir/key" --alt s
finish_capture
cmp "$test_dir/screen-baseline.png" "$test_dir/screen.png"
open_chooser --window --file "$test_dir/pointer.png"
"$test_dir/key" --alt p
# Put the pointer inside the target, which the chooser overlaps.
root=$(xwininfo -root | awk '/Window id:/ {print $4; exit}')
"$test_dir/pointer" "$root" move-at 100 100
finish_capture
if cmp -s "$test_dir/window-baseline.png" "$test_dir/pointer.png"; then
    echo 'pointer switch did not affect the capture' >&2
    exit 1
fi
open_chooser --file "$test_dir/delay.png"
"$test_dir/key" --alt d
"$test_dir/key" --plain --control a
"$test_dir/key" --plain 1
"$test_dir/key" --alt t
sleep 0.35
[[ ! -e "$test_dir/delay.png" ]]
wait "$screenshot_pid"
screenshot_pid=
wait_title screenshot-dialog-fixture
open_chooser --file "$test_dir/area.png"
"$test_dir/key" --alt l
"$test_dir/key" --alt t
# Activation may still be queued when key injection returns. Wait for actual
# withdrawal, then allow the selector's redraw delay before sending a drag.
wait_title screenshot-dialog-fixture
sleep 0.4
"$test_dir/pointer" "$root" move-at 100 120
"$test_dir/pointer" "$root" press
sleep 0.05
"$test_dir/pointer" "$root" move-at 420 360
sleep 0.05
"$test_dir/pointer" "$root" release
wait "$screenshot_pid"
screenshot_pid=
wait_title screenshot-dialog-fixture
open_chooser --area --file "$test_dir/cancel-selection.png"
"$test_dir/key" --alt t
wait_title screenshot-dialog-fixture
sleep 0.4
"$test_dir/pointer" "$root" move-at 100 120
"$test_dir/pointer" "$root" press
sleep 0.05
"$test_dir/pointer" "$root" move-at 420 360
sleep 0.05
"$test_dir/key" --plain Escape
"$test_dir/pointer" "$root" release
wait "$screenshot_pid"
screenshot_pid=
[[ ! -e "$test_dir/cancel-selection.png" ]]
wait_title screenshot-dialog-fixture
"$screenshot_binary" --no-sound --file "$test_dir/after-cancel.png" >/dev/null
cmp "$test_dir/screen-baseline.png" "$test_dir/after-cancel.png"
open_chooser --file "$test_dir/missing/failed.png"
"$test_dir/key" --alt t
wait_title 'Screenshot failed'
"$test_dir/key" --plain Escape
if wait "$screenshot_pid"; then echo 'save failure returned success' >&2; exit 1; fi
screenshot_pid=
[[ ! -e "$test_dir/missing/failed.png" ]]
python3 - "$test_dir" <<'EOF_PY'
from pathlib import Path
from PIL import Image
import sys
root = Path(sys.argv[1])
assert Image.open(root / 'screen.png').size == (800, 600)
assert Image.open(root / 'area.png').size == (320, 240)
assert Image.open(root / 'delay.png').size == (800, 600)
assert Image.open(root / 'window.png').size == Image.open(root / 'pointer.png').size
EOF_PY
echo 'chooser cancellation, modes, focus restoration, pixels, pointer, delay, explicit display, and save error passed'
