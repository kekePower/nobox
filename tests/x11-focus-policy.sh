#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-focus-policy.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 focus policy test"
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

cc "$(dirname "$0")/warp-pointer.c" -o "$test_dir/warp-pointer" -lX11
cc "$(dirname "$0")/request-activation.c" -o "$test_dir/request-activation" -lX11

write_config() {
    local follow_mouse=$1
    printf '%s\n' \
        '[focus]' \
        'focus_new = false' \
        "follow_mouse = $follow_mouse" \
        'prevent_focus_stealing = true' \
        'raise_on_focus = false' >"$test_dir/config.toml"
}
write_config true

display=
for number in $(seq 271 290); do
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

DISPLAY="$display" RUST_LOG=nobox_x11=debug NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.1
done

DISPLAY="$display" xterm -title nobox-hover-first -geometry 25x7+40+50 \
    >"$test_dir/first.log" 2>&1 &
client_pids+=("$!")
DISPLAY="$display" xterm -title nobox-hover-second -geometry 25x7+430+300 \
    >"$test_dir/second.log" 2>&1 &
client_pids+=("$!")

window_for_title() {
    local title=$1
    local clients
    clients=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null || true)
    for candidate in $(grep -o '0x[0-9a-fA-F]*' <<<"$clients"); do
        if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null | grep -q "$title"; then
            echo "$candidate"
            return 0
        fi
    done
    return 1
}

first_window=
second_window=
for _ in $(seq 1 50); do
    first_window=$(window_for_title nobox-hover-first || true)
    second_window=$(window_for_title nobox-hover-second || true)
    if [[ -n "$first_window" && -n "$second_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$first_window" || -z "$second_window" ]]; then
    echo "focus policy clients did not map" >&2
    exit 1
fi

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

stacking_top() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null |
        grep -o '0x[0-9a-fA-F]*' | tail -n 1 || true
}

DISPLAY="$display" "$test_dir/request-activation" "$first_window"
wait_for_active "$first_window"
top_before=$(stacking_top)
DISPLAY="$display" "$test_dir/warp-pointer" "$second_window"
wait_for_active "$second_window"
DISPLAY="$display" "$test_dir/warp-pointer" "$first_window"
wait_for_active "$first_window"
if [[ "${top_before,,}" != "$(stacking_top | tr '[:upper:]' '[:lower:]')" ]]; then
    echo "focus-follows-mouse raised a client despite raise_on_focus=false" >&2
    exit 1
fi

write_config false
reload_count=$(grep -c 'reloaded configuration in place' "$test_dir/nobox.log" || true)
kill -HUP "$nobox_pid"
for _ in $(seq 1 50); do
    current_count=$(grep -c 'reloaded configuration in place' "$test_dir/nobox.log" || true)
    if (( current_count > reload_count )); then break; fi
    sleep 0.05
done
if (( current_count <= reload_count )); then
    echo "focus policy configuration did not reload" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/request-activation" "$second_window"
wait_for_active "$second_window"
DISPLAY="$display" "$test_dir/warp-pointer" "$first_window"
sleep 0.2
wait_for_active "$second_window"

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited during focus policy checks" >&2
    cat "$test_dir/nobox.log" >&2
    exit 1
fi
echo "Reloadable non-raising X11 focus-follows-mouse policy passed on $display"
