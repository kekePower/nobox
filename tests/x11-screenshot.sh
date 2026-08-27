#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-screenshot.sh /path/to/nobox /path/to/nobox-screenshot}
screenshot_binary=${2:?usage: x11-screenshot.sh /path/to/nobox /path/to/nobox-screenshot}
for dependency in xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the screenshot test"
        exit 77
    fi
done

source "$(dirname "$0")/nested-x.sh"
# Xnest's root backing pixmap can omit child pixels from GetImage even though
# they are visible in its parent window. Prefer Xvfb for pixel/quality evidence;
# retain an explicit NOBOX_XSERVER override for backend diagnostics.
if [[ -z ${NOBOX_XSERVER:-} ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
xterm_pid=
area_pid=
cleanup() {
    if [[ -n "$area_pid" ]]; then kill "$area_pid" 2>/dev/null || true; fi
    if [[ -n "$xterm_pid" ]]; then kill "$xterm_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    find "$test_dir" -type f -delete 2>/dev/null || true
    find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 211 230); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
[[ -n "$display" ]]

"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
DISPLAY="$display" xdpyinfo >/dev/null

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then
        break
    fi
    sleep 0.1
done

DISPLAY="$display" xterm -title screenshot-quality-fixture -geometry 72x20+40+40 \
    -e sh -c 'i=0; while [ "$i" -lt 80 ]; do printf "Quality fixture row %02d: ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789\\n" "$i"; i=$((i + 1)); done; sleep 20' \
    >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
for _ in $(seq 1 30); do
    if DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null |
        grep -q 'window id'; then
        break
    fi
    sleep 0.1
done
sleep 0.2

DISPLAY="$display" "$screenshot_binary" --file "$test_dir/screen.png" >/dev/null
DISPLAY="$display" "$screenshot_binary" --quality 60 --file "$test_dir/q60.jpg" >/dev/null
DISPLAY="$display" "$screenshot_binary" --quality 80 --file "$test_dir/q80.jpg" >/dev/null
DISPLAY="$display" "$screenshot_binary" --window --include-pointer \
    --quality 75 --file "$test_dir/window.jpg" >/dev/null
DISPLAY="$display" "$screenshot_binary" --format jpeg --quality 75 --stdout \
    >"$test_dir/stdout.jpg"
if DISPLAY="$display" "$screenshot_binary" --quality 60 \
    --file "$test_dir/invalid.png" >"$test_dir/invalid.log" 2>&1; then
    echo "PNG unexpectedly accepted lossy JPEG quality" >&2
    exit 1
fi
grep -q 'quality.*JPEG' "$test_dir/invalid.log"
if DISPLAY="$display" "$screenshot_binary" --clipboard \
    >"$test_dir/clipboard.log" 2>&1; then
    echo "clipboard capture unexpectedly persisted without a clipboard manager" >&2
    exit 1
fi
grep -q 'no X11 clipboard manager' "$test_dir/clipboard.log"

first_default=$(DISPLAY="$display" XDG_PICTURES_DIR="$test_dir" \
    "$screenshot_binary")
second_default=$(DISPLAY="$display" XDG_PICTURES_DIR="$test_dir" \
    "$screenshot_binary")
[[ "$first_default" != "$second_default" ]]
[[ -s "$first_default" && -s "$second_default" ]]

png_magic=$(od -An -tx1 -N8 "$test_dir/screen.png" | tr -d ' \n')
[[ "$png_magic" == 89504e470d0a1a0a ]]
for image in q60.jpg q80.jpg window.jpg stdout.jpg; do
    jpeg_magic=$(od -An -tx1 -N2 "$test_dir/$image" | tr -d ' \n')
    [[ "$jpeg_magic" == ffd8 ]]
done
q60_size=$(wc -c <"$test_dir/q60.jpg")
q80_size=$(wc -c <"$test_dir/q80.jpg")
[[ "$q60_size" -lt "$q80_size" ]]
[[ -s "$test_dir/window.jpg" ]]

if command -v cc >/dev/null 2>&1 && command -v xwininfo >/dev/null 2>&1 &&
    DISPLAY="$display" xdpyinfo -queryExtensions | grep -q XTEST &&
    cc "$(dirname "$0")/button-input.c" -o "$test_dir/button-input" \
        -lX11 -lXtst >/dev/null 2>&1; then
    root_window=$(DISPLAY="$display" xwininfo -root |
        awk '/Window id:/ { print $4; exit }')
    DISPLAY="$display" "$screenshot_binary" --area --file "$test_dir/area.png" \
        >"$test_dir/area.log" 2>&1 &
    area_pid=$!
    sleep 0.2
    DISPLAY="$display" "$test_dir/button-input" "$root_window" move-at 100 120
    DISPLAY="$display" "$test_dir/button-input" "$root_window" press
    DISPLAY="$display" "$test_dir/button-input" "$root_window" move-at 420 360
    DISPLAY="$display" "$test_dir/button-input" "$root_window" release
    wait "$area_pid"
    area_pid=
    area_magic=$(od -An -tx1 -N8 "$test_dir/area.png" | tr -d ' \n')
    [[ "$area_magic" == 89504e470d0a1a0a ]]
fi

echo "screenshot PNG/JPEG, active-window, area, pointer, clipboard refusal, stdout, and quality checks passed"
