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
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
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
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$(dirname "$0")/fake-unmap-hold.c" -o "$test_dir/fake-unmap-hold" -lX11
cc "$(dirname "$0")/stacking-client.c" -o "$test_dir/stacking-client" -lX11
cc "$(dirname "$0")/request-restack.c" -o "$test_dir/request-restack" -lX11
cc "$(dirname "$0")/click-window.c" -o "$test_dir/click-window" -lX11
cc "$(dirname "$0")/decoration-client.c" -o "$test_dir/decoration-client" -lX11
cc "$(dirname "$0")/set-decoration-policy.c" -o "$test_dir/set-decoration-policy" -lX11

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
    DISPLAY="$display" xwininfo -root -tree |
        awk -v size="$size" -v position="$position" \
            'index($0, size) && $NF == position { print $1; exit }'
}

DISPLAY="$display" "$test_dir/aspect" >"$test_dir/client.log" 2>&1 &
client_pid=$!
sleep 0.7

aspect_window=$(window_for_geometry 400x400+10+10)
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
echo "Dynamic EWMH and Motif decoration regression passed on $display"
kill "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=

DISPLAY="$display" "$test_dir/title" 'nobox title regression' >"$test_dir/title.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    title_window=$(window_for_geometry 400x100+10+10)
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
    local parent_geometry=$2
    local child_geometry=$3
    local parent_window=
    local child_window=
    local active_window=

    DISPLAY="$display" "$test_dir/$program" >"$test_dir/$program.log" 2>&1 &
    client_pid=$!
    for _ in $(seq 1 30); do
        parent_window=$(window_for_geometry "$parent_geometry")
        child_window=$(window_for_geometry "$child_geometry")
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

run_modal_regression modal 400x400+10+10 200x200+10+10
run_modal_regression modal2 400x400+10+10 200x200+10+10
run_modal_regression groupmodal 300x300+0+0 100x100+0+0

DISPLAY="$display" "$test_dir/mapiconic" >"$test_dir/mapiconic.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 30); do
    iconic_window=$(window_for_geometry 400x100+50+50)
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
    fake_window=$(window_for_geometry 410x110+60+60)
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
    stack_one=$(window_for_geometry 311x111+100+100)
    stack_two=$(window_for_geometry 312x112+100+100)
    stack_three=$(window_for_geometry 313x113+100+100)
    if [[ -n "$stack_one" && -n "$stack_two" && -n "$stack_three" ]]; then break; fi
    sleep 0.1
done
wait_for_stacking "${stack_one,,},${stack_two,,},${stack_three,,}"

DISPLAY="$display" "$test_dir/request-restack" configure "$stack_three" 0 1
wait_for_stacking "${stack_three,,},${stack_one,,},${stack_two,,}"

DISPLAY="$display" "$test_dir/request-restack" ewmh "$stack_one" "$stack_two" 0
wait_for_stacking "${stack_three,,},${stack_two,,},${stack_one,,}"
echo "ConfigureRequest and EWMH stacking regressions passed on $display"
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
close_button=
for button in $(DISPLAY="$display" xwininfo -tree -id "$stacking_frame" |
    awk '/16x16/ { print $1 }'); do
    button_name=$(DISPLAY="$display" xprop -id "$button" _NET_WM_NAME)
    if grep -q 'nobox:minimize' <<<"$button_name"; then
        minimize_button=$button
    elif grep -q 'nobox:close' <<<"$button_name"; then
        close_button=$button
    fi
done
if [[ -z "$minimize_button" ]]; then
    echo "framed client has no mapped minimize button" >&2
    exit 1
fi
if [[ -z "$close_button" ]]; then
    echo "framed client has no mapped close button" >&2
    exit 1
fi
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

if grep -q 'non-fatal X11 protocol error' "$test_dir/nobox.log"; then
    echo "X11 protocol errors occurred during Openbox regressions" >&2
    tail -n 40 "$test_dir/nobox.log" >&2
    exit 1
fi
