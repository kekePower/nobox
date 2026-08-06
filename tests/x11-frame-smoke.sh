#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-frame-smoke.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 frame smoke test"
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
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/selection-client.c" -o "$test_dir/selection-client" -lX11
cc "$(dirname "$0")/pseudo-transparent-client.c" \
    -o "$test_dir/pseudo-transparent-client" -lX11

display=
for number in $(seq 131 150); do
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

DISPLAY="$display" xterm -title nobox-adoption-probe -geometry 40x10+50+50 \
    >"$test_dir/client.log" 2>&1 &
client_pid=$!
client_window=
for _ in $(seq 1 30); do
    client_window=$(DISPLAY="$display" xwininfo -root -tree |
        awk '/"nobox-adoption-probe"/ { print $1; exit }')
    if [[ -n "$client_window" ]]; then break; fi
    sleep 0.1
done
root_window=$(DISPLAY="$display" xwininfo -root | awk '/Window id:/ { print $4; exit }')

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
initial_support=
framed=false
for _ in $(seq 1 40); do
    initial_support=$(DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        awk '/window id/ { print $NF; exit }')
    parent_window=$(DISPLAY="$display" xwininfo -tree -id "$client_window" |
        awk '/Parent window id:/ { print $4; exit }')
    frame_extents=$(DISPLAY="$display" xprop -id "$client_window" _NET_FRAME_EXTENTS)
    if [[ -n "$initial_support" && "$parent_window" != "$root_window" ]] &&
        grep -q '= 2, 2, 26, 2' <<<"$frame_extents"; then
        framed=true
        break
    fi
    sleep 0.1
done
if [[ "$framed" != true ]]; then
    echo "nobox did not adopt the existing client into a frame" >&2
    exit 1
fi

kill -KILL "$nobox_pid"
wait "$nobox_pid" 2>/dev/null || true
nobox_pid=
restored=false
for _ in $(seq 1 40); do
    parent_window=$(DISPLAY="$display" xwininfo -tree -id "$client_window" |
        awk '/Parent window id:/ { print $4; exit }')
    map_state=$(DISPLAY="$display" xwininfo -id "$client_window" |
        awk -F: '/Map State:/ { gsub(/ /, "", $2); print $2; exit }')
    if [[ "$parent_window" == "$root_window" && "$map_state" == IsViewable ]]; then
        restored=true
        break
    fi
    sleep 0.1
done
if [[ "$restored" != true ]]; then
    echo "X save set did not restore the client after nobox terminated" >&2
    DISPLAY="$display" xwininfo -root -tree >&2 || true
    DISPLAY="$display" xwininfo -id "$client_window" >&2 || true
    echo "expected root=$root_window observed parent=${parent_window:-missing} state=${map_state:-missing}" >&2
    tail -n 40 "$test_dir/nobox.log" >&2
    exit 1
fi
if DISPLAY="$display" "$test_dir/selection-client" request WM_S0 owner >/dev/null 2>&1; then
    echo "the crashed nobox connection retained the ICCCM manager selection" >&2
    exit 1
fi

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/recovery.log" 2>&1 &
nobox_pid=$!
readopted=false
for _ in $(seq 1 50); do
    recovery_support=$(DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        awk '/window id/ { print $NF; exit }')
    selection_owner=$(DISPLAY="$display" "$test_dir/selection-client" \
        request WM_S0 owner 2>/dev/null || true)
    parent_window=$(DISPLAY="$display" xwininfo -tree -id "$client_window" 2>/dev/null |
        awk '/Parent window id:/ { print $4; exit }')
    frame_extents=$(DISPLAY="$display" xprop -id "$client_window" _NET_FRAME_EXTENTS 2>/dev/null || true)
    client_list=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null || true)
    active=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null || true)
    if [[ -n "$recovery_support" && "${selection_owner,,}" == "${recovery_support,,}" &&
        "$parent_window" != "$root_window" ]] &&
        grep -q '= 2, 2, 26, 2' <<<"$frame_extents" &&
        grep -qi "${client_window#0x}" <<<"$client_list" &&
        grep -qi "${client_window#0x}" <<<"$active"; then
        readopted=true
        break
    fi
    sleep 0.1
done
if [[ "$readopted" != true ]]; then
    echo "fresh nobox did not fully re-adopt the save-set client" >&2
    echo "initial support=$initial_support recovery=${recovery_support:-missing} selection=${selection_owner:-missing}" >&2
    echo "parent=${parent_window:-missing} root=$root_window" >&2
    echo "$frame_extents" >&2
    echo "$client_list" >&2
    echo "$active" >&2
    tail -n 100 "$test_dir/recovery.log" >&2 || true
    exit 1
fi

if ! DISPLAY="$display" "$test_dir/pseudo-transparent-client" \
    >"$test_dir/pseudo-transparent.log" 2>&1; then
    echo "undecorated frame replaced a ParentRelative client background" >&2
    cat "$test_dir/pseudo-transparent.log" >&2
    tail -n 100 "$test_dir/recovery.log" >&2 || true
    exit 1
fi
if ! grep -q '^pixel=0x123456 expected=0x123456$' \
    "$test_dir/pseudo-transparent.log"; then
    echo "pseudo-transparent client reported an unexpected background" >&2
    cat "$test_dir/pseudo-transparent.log" >&2
    exit 1
fi

kill -TERM "$nobox_pid"
if ! wait "$nobox_pid"; then
    echo "recovery nobox did not exit cleanly" >&2
    exit 1
fi
nobox_pid=
echo "X11 frame adoption, recovery, and ParentRelative transparency passed on $display"
