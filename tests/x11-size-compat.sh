#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-size-compat.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: x11-size-compat.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for X11 size compatibility tests"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

for fixture in big mingrow resize; do
    cc -include unistd.h "$openbox_source/tests/$fixture.c" -o "$test_dir/$fixture" -lX11
done
cc "$(dirname "$0")/configure-window.c" -o "$test_dir/configure-window" -lX11

display=
for number in $(seq 391 410); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 80); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

client_windows() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' || true
}

window_for_size() {
    local size=$1
    local window
    for window in $(client_windows); do
        if DISPLAY="$display" xwininfo -id "$window" 2>/dev/null |
            awk -v size="$size" '
                /^  Width:/ { width=$NF }
                /^  Height:/ { height=$NF }
                END { exit (width "x" height == size) ? 0 : 1 }
            '; then
            echo "$window"
            return 0
        fi
    done
    return 1
}

window_geometry() {
    DISPLAY="$display" xwininfo -id "$1" | awk '
        /Absolute upper-left X:/ { x=$NF }
        /Absolute upper-left Y:/ { y=$NF }
        /^  Width:/ { width=$NF }
        /^  Height:/ { height=$NF }
        END { print x "," y "-" width "x" height }
    '
}

wait_for_size() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 100); do
        observed=$(window_geometry "$window")
        if [[ "${observed#*-}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "geometry for $window was $observed, expected size $expected" >&2
    return 1
}

DISPLAY="$display" "$test_dir/big" >"$test_dir/big.log" 2>&1 &
client_pids+=("$!")
big_window=
for _ in $(seq 1 60); do
    big_window=$(window_for_size 2000x2000 || true)
    if [[ -n "$big_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$big_window" ]]; then
    echo "Openbox big fixture was implicitly shrunk or not managed" >&2
    DISPLAY="$display" xwininfo -root -tree >&2 || true
    exit 1
fi
big_before=$(window_geometry "$big_window")
DISPLAY="$display" "$test_dir/configure-window" move "$big_window" 120 90
for _ in $(seq 1 40); do
    big_after=$(window_geometry "$big_window")
    if [[ "$big_after" != "$big_before" ]]; then break; fi
    sleep 0.05
done
if [[ "$big_after" == "$big_before" || "${big_after#*-}" != 2000x2000 ]]; then
    echo "oversized client did not remain movable at its requested size: $big_before -> $big_after" >&2
    exit 1
fi
kill "${client_pids[0]}" 2>/dev/null || true
wait "${client_pids[0]}" 2>/dev/null || true

DISPLAY="$display" "$test_dir/mingrow" >"$test_dir/mingrow.log" 2>&1 &
client_pids+=("$!")
mingrow_window=
for _ in $(seq 1 60); do
    mingrow_window=$(window_for_size 100x100 || true)
    if [[ -n "$mingrow_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$mingrow_window" ]]; then
    echo "Openbox mingrow fixture did not map at its initial size" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/resize" >"$test_dir/resize.log" 2>&1 &
client_pids+=("$!")
resize_window=
for _ in $(seq 1 60); do
    resize_window=$(window_for_size 400x100 || true)
    if [[ -n "$resize_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$resize_window" ]]; then
    echo "Openbox resize fixture did not map at its initial size" >&2
    exit 1
fi

updated_hints=false
for _ in $(seq 1 120); do
    normal_hints=$(DISPLAY="$display" xprop -id "$mingrow_window" WM_NORMAL_HINTS)
    if grep -q 'minimum size: 200 by 200' <<<"$normal_hints"; then
        updated_hints=true
        break
    fi
    sleep 0.05
done
if [[ "$updated_hints" != true ]]; then
    echo "Openbox mingrow fixture did not publish its live minimum: $normal_hints" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/configure-window" resize "$mingrow_window" 50 50
wait_for_size "$mingrow_window" 200x200
wait_for_size "$resize_window" 600x150

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "Openbox size fixtures terminated nobox" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
if grep -q 'non-fatal X11 protocol error' "$test_dir/nobox.log"; then
    echo "Openbox size fixtures caused an X11 protocol error" >&2
    tail -n 100 "$test_dir/nobox.log" >&2
    exit 1
fi
kill -TERM "$nobox_pid"
wait "$nobox_pid"
nobox_pid=

echo "Openbox oversized, live minimum, and client resize regressions passed on $display"
