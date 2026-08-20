#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-focus-stealing.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 focus-stealing test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
baseline_pid=
client_pids=()
cleanup() {
    for pid in "${client_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$baseline_pid" ]]; then kill "$baseline_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc "$(dirname "$0")/focus-time-client.c" -o "$test_dir/focus-time-client" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11
if ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required"
    exit 77
fi

display=
for number in $(seq 211 230); do
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
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done

active_window() {
    DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' | tail -n 1 || true
}

wait_for_active() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 50); do
        observed=$(active_window)
        if [[ "${observed,,}" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "active window was '$observed', expected '$1'" >&2
    return 1
}

wait_for_attention() {
    local window=$1
    local expected=$2
    local state=
    for _ in $(seq 1 50); do
        state=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE 2>/dev/null || true)
        if [[ "$expected" == yes && "$state" == *'_NET_WM_STATE_DEMANDS_ATTENTION'* ]]; then
            return 0
        fi
        if [[ "$expected" == no && "$state" != *'_NET_WM_STATE_DEMANDS_ATTENTION'* ]]; then
            return 0
        fi
        sleep 0.05
    done
    echo "unexpected attention state for $window: $state" >&2
    return 1
}

launch_timed_client() {
    local mode=$1
    local title=$2
    local output=$test_dir/$title.window
    DISPLAY="$display" "$test_dir/focus-time-client" "$mode" 1 "$title" >"$output" 2>&1 &
    client_pids+=("$!")
    local window=
    for _ in $(seq 1 50); do
        if [[ -s "$output" ]]; then window=$(awk '{ print $1; exit }' "$output"); fi
        if [[ -n "$window" ]] && DISPLAY="$display" xprop -id "$window" \
            _NET_FRAME_EXTENTS >/dev/null 2>&1; then break; fi
        sleep 0.05
    done
    if [[ -z "$window" ]]; then
        echo "timed client $title did not map" >&2
        return 1
    fi
    launched_window=$window
}

DISPLAY="$display" xterm -title nobox-focus-baseline -geometry 30x8+30+40 \
    >"$test_dir/xterm.log" 2>&1 &
baseline_pid=$!
baseline_window=
for _ in $(seq 1 50); do
    for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*'); do
        if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
            grep -q 'nobox-focus-baseline'; then baseline_window=$candidate; fi
    done
    if [[ -n "$baseline_window" ]]; then break; fi
    sleep 0.05
done
wait_for_active "$baseline_window"

# A grabbed user key gives nobox a trustworthy timestamp newer than 1.
DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$baseline_window"

launch_timed_client direct nobox-stale-direct
direct_window=$launched_window
wait_for_active "$baseline_window"
wait_for_attention "$direct_window" yes

# Pager/taskbar requests are explicit user actions even with CurrentTime.
DISPLAY="$display" "$test_dir/request-activation" "$direct_window"
wait_for_active "$direct_window"
wait_for_attention "$direct_window" no
DISPLAY="$display" "$test_dir/request-activation" "$baseline_window"
wait_for_active "$baseline_window"

launch_timed_client indirect nobox-stale-indirect
indirect_window=$launched_window
wait_for_active "$baseline_window"
wait_for_attention "$indirect_window" yes

# Application requests with stale timestamps are denied; fresh ones are honored.
DISPLAY="$display" "$test_dir/request-activation" "$indirect_window" 1 1
wait_for_active "$baseline_window"
wait_for_attention "$indirect_window" yes
DISPLAY="$display" "$test_dir/request-activation" "$indirect_window" 1 current
wait_for_active "$indirect_window"
wait_for_attention "$indirect_window" no

supported=$(DISPLAY="$display" xprop -root _NET_SUPPORTED)
for atom in _NET_WM_USER_TIME _NET_WM_USER_TIME_WINDOW; do
    if ! grep -q "$atom" <<<"$supported"; then
        echo "$atom was not advertised" >&2
        exit 1
    fi
done
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during focus-stealing prevention" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "X11 user-time focus-stealing prevention passed on $display"
