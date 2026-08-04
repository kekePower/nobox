#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-presentation.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 presentation test"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for the X11 presentation test"
    exit 77
fi

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

source_dir=$(dirname "$0")
cc "$source_dir/presentation-client.c" -o "$test_dir/presentation-client" -lX11
cc "$source_dir/request-state.c" -o "$test_dir/request-state" -lX11
cc "$source_dir/request-activation.c" -o "$test_dir/request-activation" -lX11
cc "$source_dir/set-urgency.c" -o "$test_dir/set-urgency" -lX11
if ! cc "$source_dir/press-key.c" -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for presentation tests"
    exit 77
fi

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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" RUST_LOG=nobox_x11=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

supported=$(DISPLAY="$display" xprop -root _NET_SUPPORTED)
for atom in _NET_WM_STATE_SKIP_TASKBAR _NET_WM_STATE_SKIP_PAGER \
    _NET_WM_STATE_DEMANDS_ATTENTION; do
    if ! grep -q "$atom" <<<"$supported"; then
        echo "_NET_SUPPORTED omitted $atom" >&2
        exit 1
    fi
done

launch_client() {
    local name=$1
    shift
    DISPLAY="$display" "$test_dir/presentation-client" --title "$name" "$@" \
        >"$test_dir/$name.window" 2>"$test_dir/$name.log" &
    client_pids+=("$!")
    launched_window=
    for _ in $(seq 1 40); do
        if [[ -s "$test_dir/$name.window" ]]; then
            launched_window=$(head -n 1 "$test_dir/$name.window")
            if DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
                grep -qi "$launched_window"; then
                return 0
            fi
        fi
        sleep 0.05
    done
    echo "client $name was not managed" >&2
    return 1
}

wait_for_active() {
    local expected=$1
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -root _NET_ACTIVE_WINDOW)
        if grep -qi "window id # $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "active window was $observed, expected $expected" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    return 1
}

wait_for_state() {
    local window=$1
    local atom=$2
    local expected=$3
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_STATE)
        if [[ "$expected" == present ]] && grep -q "$atom" <<<"$observed"; then return 0; fi
        if [[ "$expected" == absent ]] && ! grep -q "$atom" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "$atom was unexpectedly $expected for $window: $observed" >&2
    return 1
}

launched_window=
launch_client presentation-one
first_window=$launched_window
launch_client presentation-skipped --skip-taskbar --skip-pager
skipped_window=$launched_window
launch_client presentation-three
third_window=$launched_window
wait_for_active "$third_window"

initial_state=$(DISPLAY="$display" xprop -id "$skipped_window" _NET_WM_STATE)
if ! grep -q _NET_WM_STATE_SKIP_TASKBAR <<<"$initial_state" \
    || ! grep -q _NET_WM_STATE_SKIP_PAGER <<<"$initial_state"; then
    echo "initial skip-taskbar/skip-pager hints were not retained: $initial_state" >&2
    exit 1
fi

DISPLAY="$display" "$test_dir/press-key" --alt Tab
wait_for_active "$first_window"

DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-taskbar remove
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_TASKBAR absent
DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-taskbar toggle
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_TASKBAR present
DISPLAY="$display" "$test_dir/request-state" "$skipped_window" skip-pager remove
wait_for_state "$skipped_window" _NET_WM_STATE_SKIP_PAGER absent

DISPLAY="$display" "$test_dir/request-state" "$third_window" attention add
wait_for_state "$third_window" _NET_WM_STATE_DEMANDS_ATTENTION present
DISPLAY="$display" "$test_dir/request-activation" "$third_window"
wait_for_active "$third_window"
wait_for_state "$third_window" _NET_WM_STATE_DEMANDS_ATTENTION absent

DISPLAY="$display" "$test_dir/set-urgency" "$skipped_window" on
for _ in $(seq 1 40); do
    if grep -q "skip_taskbar: true, skip_pager: false, urgent: true" \
        "$test_dir/nobox.log"; then break; fi
    sleep 0.05
done
if ! grep -q "skip_taskbar: true, skip_pager: false, urgent: true" \
    "$test_dir/nobox.log"; then
    echo "nobox did not observe the live ICCCM urgency hint" >&2
    tail -n 80 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
DISPLAY="$display" "$test_dir/request-activation" "$skipped_window"
wait_for_active "$skipped_window"
if ! DISPLAY="$display" xprop -id "$skipped_window" WM_HINTS | grep -qi urgency; then
    echo "nobox rewrote the client-owned ICCCM urgency hint" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/set-urgency" "$skipped_window" off
for _ in $(seq 1 40); do
    if grep -q "skip_taskbar: true, skip_pager: false, urgent: false" \
        "$test_dir/nobox.log"; then break; fi
    sleep 0.05
done
if ! grep -q "skip_taskbar: true, skip_pager: false, urgent: false" \
    "$test_dir/nobox.log"; then
    echo "nobox did not observe urgency being cleared" >&2
    exit 1
fi

echo "X11 task-list, pager, and urgency semantics passed on $display"
