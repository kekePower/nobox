#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-group-transients.sh /path/to/nobox /path/to/openbox}
openbox_source=${2:?usage: x11-group-transients.sh /path/to/nobox /path/to/openbox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for group-transient tests"
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
    echo "SKIP: Xnest, Xephyr, or Xvfb is required for group-transient tests"
    exit 77
fi

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
fixture_pid=
cleanup() {
    if [[ -n "$fixture_pid" ]]; then kill "$fixture_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

for fixture in grouptran grouptran2 grouptrancircular grouptrancircular2; do
    cc "$openbox_source/tests/$fixture.c" -o "$test_dir/$fixture" -lX11
done
cc "$(dirname "$0")/request-restack.c" -o "$test_dir/request-restack" -lX11
cc "$(dirname "$0")/request-workspace.c" -o "$test_dir/request-workspace" -lX11

display=
for number in $(seq 371 390); do
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

stacking_order() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null |
        sed -n 's/.*# //p' | tr -d ' ' | tr '[:upper:]' '[:lower:]'
}

wait_for_client_count() {
    local expected=$1
    local observed=
    for _ in $(seq 1 60); do
        mapfile -t observed < <(client_windows)
        if (( ${#observed[@]} == expected )); then return 0; fi
        sleep 0.05
    done
    echo "managed ${#observed[@]} clients, expected $expected" >&2
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST >&2 || true
    return 1
}

wait_for_stacking() {
    local expected=${1,,}
    local observed=
    for _ in $(seq 1 40); do
        observed=$(stacking_order)
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "stacking order was $observed, expected $expected" >&2
    return 1
}

wait_for_desktop() {
    local window=$1
    local expected=$2
    local observed=
    for _ in $(seq 1 40); do
        observed=$(DISPLAY="$display" xprop -id "$window" _NET_WM_DESKTOP 2>/dev/null || true)
        if grep -q "= $expected" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "desktop for $window was $observed, expected $expected" >&2
    return 1
}

stop_fixture() {
    kill "$fixture_pid" 2>/dev/null || true
    wait "$fixture_pid" 2>/dev/null || true
    fixture_pid=
    wait_for_client_count 0
}

run_fixture() {
    local fixture=$1
    local count=$2
    DISPLAY="$display" "$test_dir/$fixture" >"$test_dir/$fixture.log" 2>&1 &
    fixture_pid=$!
    wait_for_client_count "$count"
    mapfile -t windows < <(client_windows)
    for window in "${windows[@]}"; do
        if ! DISPLAY="$display" xprop -id "$window" WM_HINTS |
            grep -q 'window id #'; then
            echo "$fixture client $window did not retain its ICCCM window group" >&2
            return 1
        fi
    done
}

run_fixture grouptran 2
group_main=${windows[0]}
group_helper=${windows[1]}
DISPLAY="$display" "$test_dir/request-restack" configure "$group_main" 0 0
wait_for_stacking "${group_main,,},${group_helper,,}"
DISPLAY="$display" "$test_dir/request-workspace" move "$group_main" 1
wait_for_desktop "$group_main" 1
wait_for_desktop "$group_helper" 1
stop_fixture

run_fixture grouptran2 3
group_main=${windows[0]}
group_helper=${windows[1]}
group_child=${windows[2]}
DISPLAY="$display" "$test_dir/request-restack" configure "$group_main" 0 0
wait_for_stacking "${group_main,,},${group_helper,,},${group_child,,}"
DISPLAY="$display" "$test_dir/request-workspace" move "$group_main" 1
for window in "$group_main" "$group_helper" "$group_child"; do
    wait_for_desktop "$window" 1
done
stop_fixture

run_fixture grouptrancircular 2
first_helper=${windows[0]}
second_helper=${windows[1]}
DISPLAY="$display" "$test_dir/request-restack" configure "$first_helper" 0 0
wait_for_stacking "${second_helper,,},${first_helper,,}"
DISPLAY="$display" "$test_dir/request-workspace" move "$first_helper" 1
wait_for_desktop "$first_helper" 1
wait_for_desktop "$second_helper" 0
stop_fixture

run_fixture grouptrancircular2 3
group_helper=${windows[0]}
group_child=${windows[1]}
group_grandchild=${windows[2]}
DISPLAY="$display" "$test_dir/request-restack" configure "$group_helper" 0 0
wait_for_stacking "${group_helper,,},${group_child,,},${group_grandchild,,}"
DISPLAY="$display" "$test_dir/request-workspace" move "$group_helper" 1
for window in "$group_helper" "$group_child" "$group_grandchild"; do
    wait_for_desktop "$window" 1
done
stop_fixture

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "group-transient fixtures terminated nobox" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
if grep -q 'non-fatal X11 protocol error' "$test_dir/nobox.log"; then
    echo "group-transient fixtures caused an X11 protocol error" >&2
    tail -n 100 "$test_dir/nobox.log" >&2
    exit 1
fi
kill -TERM "$nobox_pid"
wait "$nobox_pid"
nobox_pid=

echo "Openbox group-transient stacking, workspace, and cycle regressions passed on $display"
