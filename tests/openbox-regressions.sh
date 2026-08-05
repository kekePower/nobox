#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: openbox-regressions.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: openbox-regressions.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for Openbox regression tests"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for Openbox regression tests"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pid=
dock_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$dock_pid" ]]; then kill "$dock_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$openbox_source/tests/aspect.c" -o "$test_dir/aspect" -lX11
cc -include unistd.h "$openbox_source/tests/grav.c" -o "$test_dir/grav" -lX11
cc "$openbox_source/tests/fakeunmap.c" -o "$test_dir/fakeunmap" -lX11
cc "$openbox_source/tests/mapiconic.c" -o "$test_dir/mapiconic" -lX11
cc "$openbox_source/tests/modal.c" -o "$test_dir/modal" -lX11
cc "$openbox_source/tests/modal2.c" -o "$test_dir/modal2" -lX11
cc -include unistd.h "$openbox_source/tests/groupmodal.c" -o "$test_dir/groupmodal" -lX11
cc -include unistd.h "$openbox_source/tests/stacking.c" -o "$test_dir/stacking" -lX11
cc "$openbox_source/tests/extentsrequest.c" -o "$test_dir/extentsrequest" -lX11
cc "$openbox_source/tests/title.c" -o "$test_dir/title" -lX11
cc "$openbox_source/tests/confignotifymax.c" -o "$test_dir/confignotifymax" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$(dirname "$0")/request-workspace.c" -o "$test_dir/request-workspace" -lX11
cc "$(dirname "$0")/fake-unmap-hold.c" -o "$test_dir/fake-unmap-hold" -lX11
cc "$(dirname "$0")/interactive-drag.c" -o "$test_dir/interactive-drag" -lX11 -lXtst
cc "$(dirname "$0")/stacking-client.c" -o "$test_dir/stacking-client" -lX11
cc "$(dirname "$0")/request-restack.c" -o "$test_dir/request-restack" -lX11
cc "$(dirname "$0")/click-window.c" -o "$test_dir/click-window" -lX11
cc "$(dirname "$0")/decoration-client.c" -o "$test_dir/decoration-client" -lX11
cc "$(dirname "$0")/set-decoration-policy.c" -o "$test_dir/set-decoration-policy" -lX11
cc "$(dirname "$0")/request-maximize.c" -o "$test_dir/request-maximize" -lX11
cc "$(dirname "$0")/request-geometry.c" -o "$test_dir/request-geometry" -lX11
cc "$(dirname "$0")/request-state.c" -o "$test_dir/request-state" -lX11
cc "$(dirname "$0")/strut-dock.c" -o "$test_dir/strut-dock" -lX11
cc "$(dirname "$0")/set-strut.c" -o "$test_dir/set-strut" -lX11

display=
for number in $(seq 111 130); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
sleep 0.4

window_for_geometry() {
    local geometry=$1
    local size=${geometry%%+*}
    local position=${geometry#"$size"}
    DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk -v size="$size" -v position="$position" \
            'index($0, size) && $NF == position { print $1; exit }' || true
}

window_for_size() {
    local size=$1
    DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk -v size="$size" 'index($0, size) { print $1; exit }' || true
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$NF }
        /Absolute upper-left Y:/ { y=$NF }
        /^  Width:/ { w=$NF }
        /^  Height:/ { h=$NF }
        END { print x "," y "-" w "x" h }'
}

DISPLAY="$display" "$test_dir/aspect" >"$test_dir/client.log" 2>&1 &
client_pid=$!
aspect_window=
for _ in $(seq 1 40); do
    aspect_window=$(window_for_size 400x400)
    if [[ -n "$aspect_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$aspect_window" ]]; then
    echo "Openbox aspect regression did not produce a constrained square" >&2
    DISPLAY="$display" xwininfo -root -tree >&2
    exit 1
fi
frame_extents=$(DISPLAY="$display" xprop -id "$aspect_window" _NET_FRAME_EXTENTS)
if ! grep -q '= 2, 2, 26, 2' <<<"$frame_extents"; then
    echo "framed client published unexpected extents: $frame_extents" >&2
    exit 1
fi
echo "Openbox aspect regression passed on $display"

kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/extentsrequest" >"$test_dir/extentsrequest.log" 2>&1 || true
if ! grep -q 'got new extents 2, 2, 26, 2' "$test_dir/extentsrequest.log"; then
    echo "normal pre-map frame-extents request returned an unexpected estimate" >&2
    cat "$test_dir/extentsrequest.log" >&2
    exit 1
fi
if ! tail -n 1 "$test_dir/extentsrequest.log" | grep -q 'got new extents 0, 0, 0, 0'; then
    echo "desktop pre-map frame-extents request was not undecorated" >&2
    cat "$test_dir/extentsrequest.log" >&2
    exit 1
fi
echo "Openbox pre-map frame-extents regression passed on $display"

wait_for_extents() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 30); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_FRAME_EXTENTS)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "frame extents for $window were $observed, expected $expected" >&2
    return 1
}

wait_for_unmanaged() {
    local window=$1
    for _ in $(seq 1 30); do
        if ! DISPLAY="$display" xwininfo -id "$window" >/dev/null 2>&1 \
            && ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$window"; then
            return 0
        fi
        sleep 0.05
    done
    echo "window $window remained managed after its client exited" >&2
    return 1
}

DISPLAY="$display" "$test_dir/decoration-client" >"$test_dir/decoration-client.log" 2>&1 &
client_pid=$!
decoration_window=
for _ in $(seq 1 30); do
    decoration_window=$(DISPLAY="$display" xwininfo -root -tree |
        awk '/"nobox decoration regression"/ && /360x120/ { print $1; exit }')
    if [[ -n "$decoration_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$decoration_window" ]]; then
    echo "decoration regression window did not map" >&2
    exit 1
fi
wait_for_extents "$decoration_window" '2, 2, 26, 2'
DISPLAY="$display" "$test_dir/set-decoration-policy" "$decoration_window" motif-none
wait_for_extents "$decoration_window" '0, 0, 0, 0'
DISPLAY="$display" "$test_dir/set-decoration-policy" "$decoration_window" motif-border
wait_for_extents "$decoration_window" '2, 2, 2, 2'
DISPLAY="$display" "$test_dir/set-decoration-policy" "$decoration_window" motif-all
wait_for_extents "$decoration_window" '2, 2, 26, 2'
DISPLAY="$display" "$test_dir/set-decoration-policy" "$decoration_window" desktop
wait_for_extents "$decoration_window" '0, 0, 0, 0'
DISPLAY="$display" "$test_dir/set-decoration-policy" "$decoration_window" normal
wait_for_extents "$decoration_window" '2, 2, 26, 2'
drag_initial=$(window_geometry "$decoration_window")
if [[ "$drag_initial" != '70,70-360x120' ]]; then
    echo "pointer regression client started at unexpected geometry: $drag_initial" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/interactive-drag" "$decoration_window" move cancel 100 80
if [[ "$(window_geometry "$decoration_window")" != "$drag_initial" ]]; then
    echo "Escape did not restore the initial move geometry" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/interactive-drag" "$decoration_window" move commit -63 -39
if [[ "$(window_geometry "$decoration_window")" != '2,26-360x120' ]]; then
    echo "move did not snap to the work-area edges: $(window_geometry "$decoration_window")" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/interactive-drag" "$decoration_window" resize cancel 100 80
if [[ "$(window_geometry "$decoration_window")" != '2,26-360x120' ]]; then
    echo "Escape did not restore the initial resize geometry" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/interactive-drag" "$decoration_window" resize commit 431 447
if [[ "$(window_geometry "$decoration_window")" != '2,26-796x572' ]]; then
    echo "resize did not snap to the work-area edges: $(window_geometry "$decoration_window")" >&2
    exit 1
fi
echo "Dynamic decoration and cancellable edge-resistance regressions passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=
wait_for_unmanaged "$decoration_window"

DISPLAY="$display" "$test_dir/title" 'nobox title regression' >"$test_dir/title.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    title_window=$(window_for_size 400x100)
    if [[ -n "$title_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$title_window" ]]; then
    echo "Openbox title regression window did not map" >&2
    exit 1
fi
title_frame=$(DISPLAY="$display" xwininfo -tree -id "$title_window" |
    awk '/Parent window id:/ { print $4; exit }')
if ! DISPLAY="$display" xprop -id "$title_frame" _NET_WM_NAME |
    grep -q 'nobox title regression'; then
    echo "legacy WM_NAME was not mirrored to the rendered frame title" >&2
    exit 1
fi
DISPLAY="$display" xprop -id "$title_window" -set WM_NAME 'updated nobox title'
for _ in $(seq 1 30); do
    if DISPLAY="$display" xprop -id "$title_frame" _NET_WM_NAME |
        grep -q 'updated nobox title'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -id "$title_frame" _NET_WM_NAME |
    grep -q 'updated nobox title'; then
    echo "live WM_NAME update did not refresh the frame title" >&2
    exit 1
fi
echo "Openbox title regression passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=
wait_for_unmanaged "$title_window"

DISPLAY="$display" "$test_dir/confignotifymax" >"$test_dir/confignotifymax.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    initial_max_window=$(window_for_geometry 796x572+2+26)
    if [[ -n "$initial_max_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$initial_max_window" ]]; then
    echo "Openbox initial-maximize regression did not fill the available screen" >&2
    DISPLAY="$display" xwininfo -root -tree >&2
    exit 1
fi
initial_max_state=$(DISPLAY="$display" xprop -id "$initial_max_window" _NET_WM_STATE)
if ! grep -q '_NET_WM_STATE_MAXIMIZED_HORZ' <<<"$initial_max_state" \
    || ! grep -q '_NET_WM_STATE_MAXIMIZED_VERT' <<<"$initial_max_state"; then
    echo "initial maximize state was not retained: $initial_max_state" >&2
    exit 1
fi
echo "Openbox initial-maximize regression passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/decoration-client" >"$test_dir/workarea-client.log" 2>&1 &
client_pid=$!
workarea_client=
for _ in $(seq 1 30); do
    workarea_client=$(DISPLAY="$display" xwininfo -root -tree |
        awk '/"nobox decoration regression"/ && /360x120/ { print $1; exit }')
    if [[ -n "$workarea_client" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$workarea_client" ]]; then
    echo "work-area regression application did not map" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-maximize" "$workarea_client" add
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$workarea_client")" == '2,26-796x572' ]]; then break; fi
    sleep 0.05
done
active_before_dock=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)

DISPLAY="$display" "$test_dir/strut-dock" >"$test_dir/strut-dock.log" 2>&1 &
dock_pid=$!
dock_window=
for _ in $(seq 1 30); do
    dock_window=$(head -n 1 "$test_dir/strut-dock.log")
    if [[ -n "$dock_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$dock_window" ]]; then
    echo "strut dock did not report its window" >&2
    exit 1
fi
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 30, 800, 570' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 30, 800, 570' <<<"$workarea"; then
    echo "partial top strut produced unexpected work area: $workarea" >&2
    exit 1
fi
if [[ "$(window_geometry "$workarea_client")" != '2,56-796x542' ]]; then
    echo "maximized client did not reflow around dock: $(window_geometry "$workarea_client")" >&2
    exit 1
fi
if [[ "$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)" != "$active_before_dock" ]]; then
    echo "dock incorrectly stole focus from the active application" >&2
    exit 1
fi
sticky_workareas='= 0, 30, 800, 570, 0, 30, 800, 570, 0, 30, 800, 570, 0, 30, 800, 570'
if ! grep -Fq "$sticky_workareas" <<<"$workarea"; then
    echo "sticky dock did not reserve every workspace: $workarea" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-workspace" move "$dock_window" 1
local_workareas='= 0, 0, 800, 600, 0, 30, 800, 570, 0, 0, 800, 600, 0, 0, 800, 600'
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -Fq "$local_workareas" <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -Fq "$local_workareas" <<<"$workarea"; then
    echo "workspace-local dock affected unrelated work areas: $workarea" >&2
    exit 1
fi
if [[ "$(window_geometry "$workarea_client")" != '2,26-796x572' ]]; then
    echo "local dock did not restore unrelated maximized geometry" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-workspace" current 1
DISPLAY="$display" "$test_dir/request-workspace" move "$workarea_client" 1
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$workarea_client")" == '2,56-796x542' ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$workarea_client")" != '2,56-796x542' ]]; then
    echo "maximized client did not adopt its destination work area" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-workspace" move "$workarea_client" 0
DISPLAY="$display" "$test_dir/request-workspace" current 0
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$workarea_client")" == '2,26-796x572' ]]; then break; fi
    sleep 0.05
done
DISPLAY="$display" "$test_dir/request-workspace" move "$dock_window" all
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -Fq "$sticky_workareas" <<<"$workarea" \
        && [[ "$(window_geometry "$workarea_client")" == '2,56-796x542' ]]; then break; fi
    sleep 0.05
done
if ! grep -Fq "$sticky_workareas" <<<"$workarea" \
    || [[ "$(window_geometry "$workarea_client")" != '2,56-796x542' ]]; then
    echo "restoring sticky dock membership did not reflow all workspaces" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -id "$dock_window" _NET_FRAME_EXTENTS | grep -q '= 0, 0, 0, 0'; then
    echo "dock received application decorations" >&2
    exit 1
fi
dock_stacking=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
    sed -n 's/.*# //p' | tr -d ' ')
if [[ "${dock_stacking##*,}" != "${dock_window,,}" ]]; then
    echo "dock was not kept in the top EWMH layer: $dock_stacking" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-state" "$workarea_client" fullscreen add
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$workarea_client")" == '0,0-800x600' ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$workarea_client")" != '0,0-800x600' ]]; then
    echo "fullscreen client did not cover the output: $(window_geometry "$workarea_client")" >&2
    exit 1
fi
fullscreen_state=$(DISPLAY="$display" xprop -id "$workarea_client" _NET_WM_STATE)
if ! grep -q '_NET_WM_STATE_FULLSCREEN' <<<"$fullscreen_state"; then
    echo "fullscreen request was not published: $fullscreen_state" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -id "$workarea_client" _NET_FRAME_EXTENTS |
    grep -q '= 0, 0, 0, 0'; then
    echo "fullscreen client retained decorations" >&2
    exit 1
fi
fullscreen_stacking=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
    sed -n 's/.*# //p' | tr -d ' ')
if [[ "${fullscreen_stacking##*,}" != "${workarea_client,,}" ]]; then
    echo "fullscreen client was not stacked above the dock: $fullscreen_stacking" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-geometry" "$workarea_client"
if [[ "$(window_geometry "$workarea_client")" != '0,0-800x600' ]]; then
    echo "fullscreen client accepted an application geometry request" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/set-strut" "$dock_window" both 50 80
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 50, 800, 550' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 50, 800, 550' <<<"$workarea"; then
    echo "partial strut did not override legacy strut: $workarea" >&2
    exit 1
fi
if [[ "$(window_geometry "$workarea_client")" != '0,0-800x600' ]]; then
    echo "dock work-area change resized a fullscreen client" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-state" "$workarea_client" fullscreen remove
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$workarea_client")" == '2,76-796x522' ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$workarea_client")" != '2,76-796x522' ]]; then
    echo "leaving fullscreen did not restore maximized geometry in the new work area" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -id "$workarea_client" _NET_FRAME_EXTENTS |
    grep -q '= 2, 2, 26, 2'; then
    echo "leaving fullscreen did not restore application decorations" >&2
    exit 1
fi
if DISPLAY="$display" xprop -id "$workarea_client" _NET_WM_STATE |
    grep -q '_NET_WM_STATE_FULLSCREEN'; then
    echo "leaving fullscreen retained the EWMH fullscreen state" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/request-state" "$workarea_client" above add
for _ in $(seq 1 30); do
    above_stacking=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
        sed -n 's/.*# //p' | tr -d ' ')
    if [[ "${above_stacking##*,}" == "${workarea_client,,}" ]]; then break; fi
    sleep 0.05
done
if [[ "${above_stacking##*,}" != "${workarea_client,,}" ]]; then
    echo "above client was not stacked over the dock: $above_stacking" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-state" "$workarea_client" above remove
for _ in $(seq 1 30); do
    dock_stacking=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
        sed -n 's/.*# //p' | tr -d ' ')
    if [[ "${dock_stacking##*,}" == "${dock_window,,}" ]]; then break; fi
    sleep 0.05
done
if [[ "${dock_stacking##*,}" != "${dock_window,,}" ]]; then
    echo "dock layer was not restored after removing above state: $dock_stacking" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/set-strut" "$dock_window" legacy 20
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 20, 800, 580' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 20, 800, 580' <<<"$workarea"; then
    echo "legacy strut produced unexpected work area: $workarea" >&2
    exit 1
fi
if [[ "$(window_geometry "$workarea_client")" != '2,46-796x552' ]]; then
    echo "legacy strut fallback did not reflow maximized client" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/set-strut" "$dock_window" clear
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 0, 800, 600' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 0, 800, 600' <<<"$workarea"; then
    echo "clearing struts did not restore the full work area: $workarea" >&2
    exit 1
fi
if [[ "$(window_geometry "$workarea_client")" != '2,26-796x572' ]]; then
    echo "clearing struts did not restore the full maximize area" >&2
    exit 1
fi
echo "Dynamic dock, strut, work-area, focus, and layer regressions passed on $display"
DISPLAY="$display" "$test_dir/set-strut" "$dock_window" partial 35
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 35, 800, 565' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 35, 800, 565' <<<"$workarea"; then
    echo "dock reservation was not active before destroy test: $workarea" >&2
    exit 1
fi
kill "$dock_pid" 2>/dev/null || true
wait "$dock_pid" 2>/dev/null || true
dock_pid=
for _ in $(seq 1 30); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if grep -q '= 0, 0, 800, 600' <<<"$workarea"; then break; fi
    sleep 0.05
done
if ! grep -q '= 0, 0, 800, 600' <<<"$workarea"; then
    echo "destroyed dock left a stale work-area reservation: $workarea" >&2
    exit 1
fi
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/grav" >"$test_dir/client.log" 2>&1 &
client_pid=$!
sleep 1.4

if [[ -z "$(window_for_geometry 900x275+252+373)" ]]; then
    echo "Openbox gravity regression did not preserve the south-east anchor" >&2
    DISPLAY="$display" xwininfo -root -tree >&2
    exit 1
fi
echo "Openbox gravity regression passed on $display"

kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

run_modal_regression() {
    local program=$1
    local parent_size=$2
    local child_size=$3
    local parent_window=
    local child_window=
    local active_window=
    local parent_desktop=
    local child_desktop=

    DISPLAY="$display" "$test_dir/$program" >"$test_dir/$program.log" 2>&1 &
    client_pid=$!
    for _ in $(seq 1 30); do
        parent_window=$(window_for_size "$parent_size")
        child_window=$(window_for_size "$child_size")
        if [[ -n "$parent_window" && -n "$child_window" ]]; then break; fi
        sleep 0.1
    done
    if [[ -z "$parent_window" || -z "$child_window" ]]; then
        echo "Openbox $program regression windows did not map" >&2
        DISPLAY="$display" xwininfo -root -tree >&2
        exit 1
    fi
    local ready=false
    for _ in $(seq 1 30); do
        client_list=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST)
        modal_state=$(DISPLAY="$display" xprop -id "$child_window" _NET_WM_STATE)
        if grep -qi "$parent_window" <<<"$client_list" \
            && grep -qi "$child_window" <<<"$client_list" \
            && grep -q '_NET_WM_STATE_MODAL' <<<"$modal_state"; then
            ready=true
            break
        fi
        sleep 0.1
    done
    if [[ "$ready" != true ]]; then
        echo "Openbox $program regression was not fully managed as modal" >&2
        echo "$client_list" >&2
        echo "$modal_state" >&2
        DISPLAY="$display" xwininfo -root -tree >&2
        tail -n 30 "$test_dir/nobox.log" >&2
        exit 1
    fi

    local stacking
    stacking=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
        tr '[:upper:]' '[:lower:]')
    if [[ "$stacking" != *"${parent_window,,}"*"${child_window,,}"* ]]; then
        echo "Openbox $program transient was not stacked above its parent: $stacking" >&2
        exit 1
    fi

    if [[ "$program" != groupmodal ]]; then
        DISPLAY="$display" "$test_dir/request-workspace" move "$parent_window" 1
        local family_moved=false
        for _ in $(seq 1 30); do
            parent_desktop=$(DISPLAY="$display" xprop -id "$parent_window" _NET_WM_DESKTOP)
            child_desktop=$(DISPLAY="$display" xprop -id "$child_window" _NET_WM_DESKTOP)
            if grep -q '= 1' <<<"$parent_desktop" && grep -q '= 1' <<<"$child_desktop"; then
                family_moved=true
                break
            fi
            sleep 0.05
        done
        if [[ "$family_moved" != true ]]; then
            echo "Openbox $program transient family did not move together" >&2
            exit 1
        fi
        DISPLAY="$display" "$test_dir/request-workspace" current 1
        sleep 0.1
        DISPLAY="$display" "$test_dir/request-workspace" move "$child_window" 0
        DISPLAY="$display" "$test_dir/request-workspace" current 0
        sleep 0.1
    fi

    DISPLAY="$display" "$test_dir/request-activation" "$parent_window"
    sleep 0.2
    active_window=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW |
        sed -n 's/.*# \(0x[0-9a-fA-F]*\)$/\1/p')
    if [[ -z "$active_window" || "${active_window,,}" != "${child_window,,}" ]]; then
        echo "Openbox $program regression activated $active_window instead of modal $child_window" >&2
        exit 1
    fi
    echo "Openbox $program regression passed on $display"

    kill "$client_pid" 2>/dev/null || true
    wait "$client_pid" 2>/dev/null || true
    client_pid=
    for _ in $(seq 1 20); do
        client_list=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST)
        if ! DISPLAY="$display" xwininfo -id "$child_window" >/dev/null 2>&1 \
            && ! grep -qi "$parent_window" <<<"$client_list" \
            && ! grep -qi "$child_window" <<<"$client_list"; then
            break
        fi
        sleep 0.05
    done
}

run_modal_regression modal 400x400 200x200
run_modal_regression modal2 400x400 200x200
run_modal_regression groupmodal 300x300 100x100

DISPLAY="$display" "$test_dir/mapiconic" >"$test_dir/mapiconic.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    iconic_window=$(window_for_size 400x100)
    if [[ -n "$iconic_window" ]]; then break; fi
    sleep 0.1
done
if [[ -z "$iconic_window" ]]; then
    echo "Openbox mapiconic regression window did not appear" >&2
    exit 1
fi
if ! DISPLAY="$display" xwininfo -id "$iconic_window" | grep -q 'Map State: IsUnMapped'; then
    echo "Openbox mapiconic regression was mapped despite its initial state" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -id "$iconic_window" WM_STATE | grep -q 'Iconic'; then
    echo "Openbox mapiconic regression did not receive Iconic WM_STATE" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-activation" "$iconic_window"
sleep 0.2
if ! DISPLAY="$display" xwininfo -id "$iconic_window" | grep -q 'Map State: IsViewable'; then
    echo "Openbox mapiconic regression did not restore on activation" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -id "$iconic_window" WM_STATE | grep -q 'Normal'; then
    echo "Openbox mapiconic regression did not receive Normal WM_STATE on restore" >&2
    exit 1
fi
echo "Openbox mapiconic regression passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/fake-unmap-hold" >"$test_dir/fake-unmap-hold.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    fake_window=$(window_for_size 410x110)
    if [[ -n "$fake_window" ]]; then break; fi
    sleep 0.1
done
sleep 1.2
if ! DISPLAY="$display" xwininfo -id "$fake_window" | grep -q 'Map State: IsViewable'; then
    echo "Synthetic UnmapNotify incorrectly withdrew a mapped client" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_CLIENT_LIST | grep -qi "$fake_window"; then
    echo "Synthetic UnmapNotify incorrectly removed a managed client" >&2
    exit 1
fi
echo "Synthetic unmap regression passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/fakeunmap" >"$test_dir/fakeunmap.log" 2>&1 || true
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "Openbox fakeunmap regression terminated nobox" >&2
    exit 1
fi
echo "Openbox fakeunmap regression passed on $display"

stacking_order() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING |
        sed -n 's/.*# //p' | tr -d ' '
}

wait_for_stacking() {
    local expected=${1,,}
    for _ in $(seq 1 30); do
        if [[ "$(stacking_order)" == "$expected" ]]; then return 0; fi
        sleep 0.1
    done
    echo "stacking order $(stacking_order) did not become $expected" >&2
    return 1
}

DISPLAY="$display" "$test_dir/stacking-client" >"$test_dir/stacking-client.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    stack_one=$(window_for_size 311x111)
    stack_two=$(window_for_size 312x112)
    stack_three=$(window_for_size 313x113)
    if [[ -n "$stack_one" && -n "$stack_two" && -n "$stack_three" ]]; then break; fi
    sleep 0.1
done
wait_for_stacking "${stack_one,,},${stack_two,,},${stack_three,,}"

DISPLAY="$display" "$test_dir/request-restack" configure "$stack_three" 0 1
wait_for_stacking "${stack_three,,},${stack_one,,},${stack_two,,}"

DISPLAY="$display" "$test_dir/request-restack" ewmh "$stack_one" "$stack_two" 0
wait_for_stacking "${stack_three,,},${stack_two,,},${stack_one,,}"

DISPLAY="$display" "$test_dir/request-state" "$stack_three" above add
wait_for_stacking "${stack_two,,},${stack_one,,},${stack_three,,}"
DISPLAY="$display" "$test_dir/request-state" "$stack_one" below add
wait_for_stacking "${stack_one,,},${stack_two,,},${stack_three,,}"
DISPLAY="$display" "$test_dir/request-state" "$stack_one" above add
wait_for_stacking "${stack_two,,},${stack_one,,},${stack_three,,}"
layer_state=$(DISPLAY="$display" xprop -id "$stack_one" _NET_WM_STATE)
if ! grep -q '_NET_WM_STATE_ABOVE' <<<"$layer_state" \
    || grep -q '_NET_WM_STATE_BELOW' <<<"$layer_state"; then
    echo "above/below state was not kept mutually exclusive: $layer_state" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-state" "$stack_one" above remove
wait_for_stacking "${stack_two,,},${stack_one,,},${stack_three,,}"
DISPLAY="$display" "$test_dir/request-state" "$stack_three" above remove
wait_for_stacking "${stack_two,,},${stack_one,,},${stack_three,,}"
echo "ConfigureRequest and EWMH stacking/layer regressions passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/stacking" >"$test_dir/stacking.log" 2>&1 &
client_pid=$!
sleep 6.5
openbox_stacking=$(stacking_order)
openbox_stacking_count=$(grep -o '0x[0-9a-fA-F]*' <<<"$openbox_stacking" | wc -l)
if [[ "$openbox_stacking_count" -ne 3 ]]; then
    echo "Openbox stacking regression managed $openbox_stacking_count clients instead of 3" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "Openbox stacking regression terminated nobox" >&2
    exit 1
fi
echo "Openbox stacking regression passed on $display"

stacking_client=${openbox_stacking%%,*}
stacking_frame=$(DISPLAY="$display" xwininfo -tree -id "$stacking_client" |
    awk '/Parent window id:/ { print $4; exit }')
minimize_button=
maximize_button=
close_button=
for button in $(DISPLAY="$display" xwininfo -tree -id "$stacking_frame" |
    awk '/16x16/ { print $1 }'); do
    button_name=$(DISPLAY="$display" xprop -id "$button" _NET_WM_NAME)
    if grep -q 'nobox:minimize' <<<"$button_name"; then
        minimize_button=$button
    elif grep -q 'nobox:maximize' <<<"$button_name"; then
        maximize_button=$button
    elif grep -q 'nobox:close' <<<"$button_name"; then
        close_button=$button
    fi
done
if [[ -z "$minimize_button" ]]; then
    echo "framed client has no mapped minimize button" >&2
    exit 1
fi
if [[ -z "$maximize_button" ]]; then
    echo "framed client has no mapped maximize button" >&2
    exit 1
fi
if [[ -z "$close_button" ]]; then
    echo "framed client has no mapped close button" >&2
    exit 1
fi

restore_geometry=$(window_geometry "$stacking_client")
DISPLAY="$display" "$test_dir/request-maximize" "$stacking_client" add
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$stacking_client")" == '2,26-796x572' ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$stacking_client")" != '2,26-796x572' ]]; then
    echo "EWMH maximize request produced $(window_geometry "$stacking_client")" >&2
    exit 1
fi
maximized_state=$(DISPLAY="$display" xprop -id "$stacking_client" _NET_WM_STATE)
if ! grep -q '_NET_WM_STATE_MAXIMIZED_HORZ' <<<"$maximized_state" \
    || ! grep -q '_NET_WM_STATE_MAXIMIZED_VERT' <<<"$maximized_state"; then
    echo "EWMH maximize request did not publish both axes: $maximized_state" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-maximize" "$stacking_client" remove
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$stacking_client")" == "$restore_geometry" ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$stacking_client")" != "$restore_geometry" ]]; then
    echo "EWMH unmaximize restored $(window_geometry "$stacking_client"), expected $restore_geometry" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/click-window" "$maximize_button"
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$stacking_client")" == '2,26-796x572' ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$stacking_client")" != '2,26-796x572' ]]; then
    echo "titlebar maximize button did not maximize the client" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/click-window" "$maximize_button"
for _ in $(seq 1 30); do
    if [[ "$(window_geometry "$stacking_client")" == "$restore_geometry" ]]; then break; fi
    sleep 0.05
done
if [[ "$(window_geometry "$stacking_client")" != "$restore_geometry" ]]; then
    echo "titlebar maximize button did not restore the client" >&2
    exit 1
fi
echo "EWMH and titlebar maximize regressions passed on $display"

DISPLAY="$display" "$test_dir/click-window" "$minimize_button"
for _ in $(seq 1 30); do
    if DISPLAY="$display" xprop -id "$stacking_client" WM_STATE | grep -q 'Iconic'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -id "$stacking_client" WM_STATE | grep -q 'Iconic'; then
    echo "titlebar minimize button did not iconify the client" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-activation" "$stacking_client"
for _ in $(seq 1 30); do
    if DISPLAY="$display" xprop -id "$stacking_client" WM_STATE | grep -q 'Normal'; then break; fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -id "$stacking_client" WM_STATE | grep -q 'Normal'; then
    echo "minimized titlebar client did not restore on activation" >&2
    exit 1
fi
echo "Titlebar minimize-button regression passed on $display"

printf '[theme]\ntitlebar_height = 30\nfont = "fixed"\ntitle_alignment = "right"\ntitle_padding = 12\n' >"$test_dir/config.toml"
kill -HUP "$nobox_pid"
wait_for_extents "$stacking_client" '2, 2, 32, 2'
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "valid SIGHUP configuration reload terminated nobox" >&2
    exit 1
fi
font_error_count=$(grep -c 'could not apply reloaded configuration' "$test_dir/nobox.log" || true)
printf '[theme]\ntitlebar_height = 40\nfont = "nobox-font-that-does-not-exist"\n' >"$test_dir/config.toml"
kill -HUP "$nobox_pid"
for _ in $(seq 1 20); do
    current_font_error_count=$(
        grep -c 'could not apply reloaded configuration' "$test_dir/nobox.log" || true
    )
    if (( current_font_error_count > font_error_count )); then break; fi
    sleep 0.05
done
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "unavailable theme font terminated nobox" >&2
    exit 1
fi
wait_for_extents "$stacking_client" '2, 2, 32, 2'
if (( current_font_error_count <= font_error_count )); then
    echo "unavailable runtime theme font was not diagnosed" >&2
    exit 1
fi
printf 'unknown_runtime_key = true\n' >"$test_dir/config.toml"
kill -HUP "$nobox_pid"
sleep 0.2
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "invalid SIGHUP configuration reload terminated nobox" >&2
    exit 1
fi
wait_for_extents "$stacking_client" '2, 2, 32, 2'
: >"$test_dir/config.toml"
kill -HUP "$nobox_pid"
wait_for_extents "$stacking_client" '2, 2, 26, 2'
if ! grep -q 'could not reload configuration' "$test_dir/nobox.log"; then
    echo "invalid runtime configuration was not diagnosed" >&2
    exit 1
fi
echo "Validated in-place SIGHUP configuration reload regression passed on $display"

DISPLAY="$display" "$test_dir/click-window" "$close_button"
for _ in $(seq 1 30); do
    if [[ -z "$(stacking_order)" ]]; then break; fi
    sleep 0.1
done
if [[ -n "$(stacking_order)" ]]; then
    echo "titlebar close button did not close the client connection" >&2
    exit 1
fi
echo "Titlebar close-button regression passed on $display"

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited before the clean shutdown request" >&2
    tail -n 80 "$test_dir/nobox.log" >&2
    exit 1
fi
if grep -q 'non-fatal X11 protocol error' "$test_dir/nobox.log"; then
    echo "X11 protocol errors occurred during Openbox regressions" >&2
    tail -n 40 "$test_dir/nobox.log" >&2
    exit 1
fi

kill -TERM "$nobox_pid"
if ! wait "$nobox_pid"; then
    echo "SIGTERM did not produce a successful nobox exit" >&2
    exit 1
fi
nobox_pid=
if ! grep -q 'X11 event loop stopped cleanly' "$test_dir/nobox.log"; then
    echo "SIGTERM shutdown was not logged as clean" >&2
    exit 1
fi
echo "Clean SIGTERM shutdown regression passed on $display"
