#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-agent-seat.sh /path/to/nobox /path/to/agent-seat-probe}
probe_binary=${2:?usage: x11-agent-seat.sh /path/to/nobox /path/to/agent-seat-probe}
for dependency in xdpyinfo xprop xterm; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the agent seat test"
        exit 77
    fi
done
if [[ ! -x "$probe_binary" ]]; then
    echo "SKIP: the agent seat probe was not built at $probe_binary"
    exit 77
fi

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
xterm_pid=
cleanup() {
    if [[ -n "$xterm_pid" ]]; then kill "$xterm_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

fail() {
    echo "$1" >&2
    sed -n '1,200p' "$test_dir/nobox.log" >&2
    exit 1
}

# A private runtime directory keeps the seat's socket inside the test.
runtime_dir="$test_dir/run"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"

# The grant binds to the probe's executable. The impostor is a byte-for-byte
# copy that declares the same harness name from a different path, so the test
# proves the binding is the executable and not anything the peer says.
probe="$test_dir/agent-seat-probe"
impostor="$test_dir/impostor-probe"
cp -- "$probe_binary" "$probe"
cp -- "$probe_binary" "$impostor"

cat >"$test_dir/config.toml" <<EOF
[agent]
enabled = true

[[agent.grants]]
label = "integration probe"
executable = "$probe"
capabilities = ["observe"]
EOF

display=
for number in $(seq 471 490); do
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
if ! DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then
    echo "$nested_x_server did not become ready" >&2
    exit 1
fi

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then
        fail "nobox exited during startup"
    fi
    if grep -q 'agent seat listening' "$test_dir/nobox.log"; then break; fi
    sleep 0.1
done
grep -q 'agent seat listening' "$test_dir/nobox.log" ||
    fail "nobox did not start its agent seat"

socket="$runtime_dir/nobox/agent-seat-${display#:}.sock"
[[ -S "$socket" ]] || fail "the agent seat socket is missing at $socket"

# The seat lives in a private directory behind a private socket.
directory_mode=$(stat -c %a "$runtime_dir/nobox")
socket_mode=$(stat -c %a "$socket")
[[ "$directory_mode" == "700" ]] ||
    fail "the agent seat directory is mode $directory_mode, not 700"
[[ "$socket_mode" == "600" ]] ||
    fail "the agent seat socket is mode $socket_mode, not 600"

# Discovery is the traditional X11 route: a root property naming the protocol
# version and the socket path.
advertisement=$(DISPLAY="$display" xprop -root _AGENT_SEAT)
grep -q 'agent-seat' <<<"$advertisement" ||
    fail "the root window does not advertise the protocol: $advertisement"
grep -qF "$socket" <<<"$advertisement" ||
    fail "the advertisement does not name the socket: $advertisement"

run_probe() {
    local binary=$1 scenario=$2 label=$3
    if ! "$binary" "$socket" "$scenario" nobox-integration-probe \
        >"$test_dir/probe-$scenario.log" 2>&1; then
        echo "the $label scenario failed" >&2
        sed -n '1,60p' "$test_dir/probe-$scenario.log" >&2
        exit 1
    fi
}

# A grant confers exactly its atoms: observe answers "not implemented yet",
# and manage is refused outright.
run_probe "$probe" granted "stored grant"
grep -q 'granted=observe.structure,observe.titles' "$test_dir/probe-granted.log" ||
    fail "the stored grant was not issued as configured"
grep -q 'desktop.snapshot -> unsupported' "$test_dir/probe-granted.log" ||
    fail "a granted capability was not distinguished from a denied one"
grep -q 'workspace.switch -> denied' "$test_dir/probe-granted.log" ||
    fail "an ungranted capability was not denied"

# The same declared harness name from an unnamed executable holds nothing.
run_probe "$impostor" unbound "deny by default"
grep -q 'granted=0' "$test_dir/probe-unbound.log" ||
    fail "an executable without a grant received capabilities"
grep -q 'desktop.snapshot -> denied' "$test_dir/probe-unbound.log" ||
    fail "a session without a grant was not denied"

# Protocol faults end their own session and nothing else.
run_probe "$probe" version "version mismatch"
run_probe "$probe" no-hello "request before handshake"
run_probe "$probe" second-hello "repeated handshake"
run_probe "$probe" oversize "oversized frame"
run_probe "$probe" garbage "malformed frame"
run_probe "$probe" truncate "abandoned mid-frame"
run_probe "$probe" flood "request flood"

kill -0 "$nobox_pid" 2>/dev/null || fail "nobox died while serving agent sessions"

# The real test of isolation: after all of that, window management still works.
DISPLAY="$display" xterm -title nobox-agent-seat -geometry 40x10+30+40 \
    >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
managed=
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -q '_NET_CLIENT_LIST(WINDOW): window id #'; then
        managed=yes
        break
    fi
    sleep 0.1
done
[[ -n "$managed" ]] || fail "nobox stopped managing windows after agent traffic"

DISPLAY="$display" xprop -root _AGENT_SEAT >/dev/null ||
    fail "the agent seat advertisement disappeared"

# A clean shutdown withdraws the seat and removes its socket.
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" "$nobox_binary" --exit
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.1
done
if kill -0 "$nobox_pid" 2>/dev/null; then
    fail "nobox did not exit"
fi
nobox_pid=
[[ ! -e "$socket" ]] || fail "the agent seat socket outlived the manager"

echo "agent seat test passed on $display"
