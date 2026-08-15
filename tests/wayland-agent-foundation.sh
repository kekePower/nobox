#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-agent-foundation.sh NOBOX WAYLAND_PROBE AGENT_PROBE}
wayland_probe=${2:?missing Wayland probe}
agent_probe=${3:?missing Agent Seat probe}

for dependency in xdpyinfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the nested Wayland Agent Seat test"
        exit 77
    fi
done

if [[ -z ${NOBOX_XSERVER:-} ]]; then
    if command -v Xvfb >/dev/null 2>&1; then
        export NOBOX_XSERVER=xvfb
    elif command -v Xephyr >/dev/null 2>&1; then
        export NOBOX_XSERVER=xephyr
    else
        echo "SKIP: Xvfb or Xephyr is required for the nested Wayland Agent Seat test"
        exit 77
    fi
fi

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
xserver_pid=
wayland_pid=
client_pid=
cleanup() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_pid" ]]; then kill "$wayland_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    if [[ "${KEEP_TEST_DIR:-0}" == 1 ]]; then
        echo "kept Wayland Agent Seat test directory: $test_dir" >&2
    else
        rm -rf -- "$test_dir"
    fi
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 281 290); do
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
DISPLAY="$display" xdpyinfo >/dev/null

agent_socket="$runtime_dir/nobox/wayland-agent-test.sock"
cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = false

[agent]
enabled = true
socket = "$agent_socket"
policy = "deny"

[[agent.grants]]
label = "nested Wayland observation probe"
executable = "$agent_probe"
capabilities = ["observe.structure", "observe.titles", "manage.close"]
EOF

log="$test_dir/nobox.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_STATE_FILE="$test_dir/session.toml" \
    "$nobox_binary" --backend wayland --config "$test_dir/config.toml" \
    run --nested-x11 --no-autostart >"$log" 2>&1 &
wayland_pid=$!

wayland_socket=
for _ in $(seq 1 100); do
    wayland_socket=$(sed -n 's/^ready: //p' "$log" 2>/dev/null | head -n 1)
    if [[ -n "$wayland_socket" && -S "$agent_socket" ]]; then break; fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ -z "$wayland_socket" || ! -S "$agent_socket" ]]; then
    echo "nested Wayland Agent Seat did not become ready" >&2
    cat "$log" >&2
    exit 1
fi
[[ $(stat -c '%a' "$(dirname "$agent_socket")") == 700 ]]
[[ $(stat -c '%a' "$agent_socket") == 600 ]]

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
    "$wayland_probe" --agent-hold >"$test_dir/client.log" 2>&1 &
client_pid=$!

"$agent_probe" "$agent_socket" granted wayland-foundation \
    >"$test_dir/granted.log"
grep -Fq 'welcome granted=observe.structure,observe.titles' "$test_dir/granted.log"
for _ in $(seq 1 50); do
    if "$agent_probe" "$agent_socket" snapshot wayland-foundation \
        >"$test_dir/snapshot.log" 2>&1; then
        break
    fi
    sleep 0.05
done
grep -Fq 'title=nobox Wayland agent visible' "$test_dir/snapshot.log"
grep -Fq 'workspaces 4' "$test_dir/snapshot.log"
grep -Fq 'outputs 1' "$test_dir/snapshot.log"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=
wait "$client_pid" 2>/dev/null || true
client_pid=
[[ ! -e "$agent_socket" ]]

echo "nested Wayland Agent Seat observation foundation passed on $nested_x_server $display"
