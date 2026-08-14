#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: runtime-control.sh /path/to/nobox}
for dependency in xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the runtime-control test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
xserver_pid=
backend_pids=()
cleanup() {
    for pid in "${backend_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 241 260); do
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
    sleep 0.05
done

export XDG_RUNTIME_DIR="$runtime_dir"
export DISPLAY="$display"
printf '[panel]\nenabled = false\n' >"$test_dir/config.toml"

"$nobox_binary" --config "$test_dir/config.toml" run --no-autostart \
    >"$test_dir/x11.log" 2>&1 &
x11_pid=$!
backend_pids+=("$x11_pid")
for _ in $(seq 1 100); do
    if xprop -root _NOBOX_RUNTIME_INSTANCE 2>/dev/null | grep -q 'nobox'; then break; fi
    kill -0 "$x11_pid"
    sleep 0.02
done
x11_records=("$runtime_dir"/nobox/x11-*.toml)
x11_sockets=("$runtime_dir"/nobox/x11-*.sock)
[[ ${#x11_records[@]} -eq 1 && ${#x11_sockets[@]} -eq 1 ]]
[[ $(stat -c '%a' "${x11_records[0]}") == 600 ]]
[[ $(stat -c '%a' "${x11_sockets[0]}") == 600 ]]
kill -HUP "$x11_pid"
sleep 0.05
kill -0 "$x11_pid"
"$nobox_binary" --exit
wait "$x11_pid"
backend_pids=()
compgen -G "$runtime_dir/nobox/x11-*" >/dev/null && {
    echo "X11 runtime files leaked after shutdown" >&2
    exit 1
}

start_wayland() {
    local log=$1
    "$nobox_binary" --backend wayland --config "$test_dir/config.toml" \
        run --nested-x11 --no-autostart >"$log" 2>&1 &
    wayland_pid=$!
    backend_pids+=("$wayland_pid")
}

start_wayland "$test_dir/wayland-one.log"
wayland_one=$wayland_pid
for _ in $(seq 1 100); do
    records=("$runtime_dir"/nobox/wayland-*.toml)
    if [[ ${#records[@]} -eq 1 && -e "${records[0]}" ]]; then break; fi
    kill -0 "$wayland_one"
    sleep 0.02
done
kill -HUP "$wayland_one"
for _ in $(seq 1 50); do
    if grep -q 'received a configuration reload request' "$test_dir/wayland-one.log"; then break; fi
    sleep 0.01
done
grep -q 'received a configuration reload request' "$test_dir/wayland-one.log"
started_ns=$(date +%s%N)
"$nobox_binary" --backend wayland --exit
wait "$wayland_one"
backend_pids=()
elapsed_ms=$((($(date +%s%N) - started_ns) / 1000000))
if (( elapsed_ms >= 500 )); then
    echo "Wayland control wake took ${elapsed_ms}ms; expected an immediate calloop wake" >&2
    exit 1
fi

start_wayland "$test_dir/wayland-a.log"
wayland_one=$wayland_pid
start_wayland "$test_dir/wayland-b.log"
wayland_two=$wayland_pid
for _ in $(seq 1 100); do
    records=("$runtime_dir"/nobox/wayland-*.toml)
    if [[ ${#records[@]} -eq 2 && -e "${records[1]}" ]]; then break; fi
    kill -0 "$wayland_one"
    kill -0 "$wayland_two"
    sleep 0.02
done
if "$nobox_binary" --backend wayland --exit >"$test_dir/ambiguous.out" 2>"$test_dir/ambiguous.err"; then
    echo "ambiguous Wayland exit unexpectedly succeeded" >&2
    exit 1
fi
grep -q 'select one explicitly' "$test_dir/ambiguous.err"
for record in "${records[@]}"; do
    name=$(basename "$record")
    instance=${name#wayland-}
    instance=${instance%.toml}
    "$nobox_binary" --backend wayland --instance "$instance" --exit
done
wait "$wayland_one"
wait "$wayland_two"
backend_pids=()

echo "typed X11 and Wayland runtime control, cleanup, prompt wake, and ambiguity checks passed"
