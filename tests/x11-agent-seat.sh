#!/usr/bin/env bash
set -euo pipefail

usage="usage: x11-agent-seat.sh /path/to/nobox /path/to/agent-seat-probe /path/to/nobox-agent"
nobox_binary=${1:?$usage}
probe_binary=${2:?$usage}
companion_binary=${3:?$usage}
for dependency in xdpyinfo xprop xterm python3; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the agent seat test"
        exit 77
    fi
done
if [[ ! -x "$probe_binary" ]]; then
    echo "SKIP: the agent seat probe was not built at $probe_binary"
    exit 77
fi
if [[ ! -x "$companion_binary" ]]; then
    echo "SKIP: the MCP companion was not built at $companion_binary"
    exit 77
fi

# Grants bind to absolute executable paths, so resolve what we were handed.
nobox_binary=$(readlink -f "$nobox_binary")
probe_binary=$(readlink -f "$probe_binary")
companion_binary=$(readlink -f "$companion_binary")

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
visible_pid=
secret_pid=
late_pid=
cleanup() {
    for pid in "$late_pid" "$secret_pid" "$visible_pid" "$nobox_pid" "$xserver_pid"; do
        if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi
    done
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

[[agent.grants]]
label = "MCP companion"
executable = "$companion_binary"
capabilities = ["observe"]

# A window the user marks hidden must be absent from every agent answer, and
# indistinguishable from one that never existed.
[[applications]]
match = { title = "nobox-agent-secret" }
agent_visibility = "hidden"
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
    shift 3
    if ! "$binary" "$socket" "$scenario" nobox-integration-probe "$@" \
        >"$test_dir/probe-$scenario.log" 2>&1; then
        echo "the $label scenario failed" >&2
        sed -n '1,60p' "$test_dir/probe-$scenario.log" >&2
        exit 1
    fi
}

count_managed_windows() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST | tr ',' '\n' | grep -c '0x' || true
}

wait_for_managed_windows() {
    local wanted=$1
    for _ in $(seq 1 80); do
        if [[ "$(count_managed_windows)" -ge "$wanted" ]]; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

# A grant confers exactly its atoms: an unimplemented but granted tool answers
# "unsupported", and an ungranted one is refused outright.
run_probe "$probe" granted "stored grant"
grep -q 'granted=observe.structure,observe.titles' "$test_dir/probe-granted.log" ||
    fail "the stored grant was not issued as configured"
grep -q 'subscribe_and_snapshot -> unsupported' "$test_dir/probe-granted.log" ||
    fail "a granted capability was not distinguished from a denied one"
grep -q 'workspace.switch -> denied' "$test_dir/probe-granted.log" ||
    fail "an ungranted capability was not denied"

# The same declared harness name from an unnamed executable holds nothing.
run_probe "$impostor" unbound "deny by default"
grep -q 'granted=0' "$test_dir/probe-unbound.log" ||
    fail "an executable without a grant received capabilities"
grep -q 'desktop.snapshot -> denied' "$test_dir/probe-unbound.log" ||
    fail "a session without a grant was not denied"

# Two windows: one ordinary, one the configuration hides from agents.
DISPLAY="$display" xterm -title nobox-agent-visible -geometry 40x10+30+40 \
    >"$test_dir/xterm-visible.log" 2>&1 &
visible_pid=$!
DISPLAY="$display" xterm -title nobox-agent-secret -geometry 40x10+300+40 \
    >"$test_dir/xterm-secret.log" 2>&1 &
secret_pid=$!
wait_for_managed_windows 2 || fail "nobox did not manage both test windows"
mapfile -t managed_windows < <(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
    sed 's/.*# //' | tr ',' '\n' | tr -d ' ' | grep '^0x')

# Structured observation: the world model an agent works from.
run_probe "$probe" snapshot "desktop snapshot"
grep -q 'class=XTerm' "$test_dir/probe-snapshot.log" ||
    fail "the snapshot did not describe the managed windows"
grep -q 'title=nobox-agent-visible' "$test_dir/probe-snapshot.log" ||
    fail "the snapshot did not carry window titles"
if grep -q 'nobox-agent-secret' "$test_dir/probe-snapshot.log"; then
    echo "a hidden window appeared in an agent snapshot" >&2
    cat "$test_dir/probe-snapshot.log" >&2
    exit 1
fi

# The hidden window must answer exactly as a window that never existed.
run_probe "$probe" hidden-oracle "hidden client oracle" "${managed_windows[@]}"
grep -q 'withheld 1 of' "$test_dir/probe-hidden-oracle.log" ||
    fail "the hidden window was not withheld exactly once"

# Protocol faults end their own session and nothing else.
run_probe "$probe" version "version mismatch"
run_probe "$probe" no-hello "request before handshake"
run_probe "$probe" second-hello "repeated handshake"
run_probe "$probe" oversize "oversized frame"
run_probe "$probe" garbage "malformed frame"
run_probe "$probe" truncate "abandoned mid-frame"
run_probe "$probe" flood "request flood"

kill -0 "$nobox_pid" 2>/dev/null || fail "nobox died while serving agent sessions"

# The MCP companion, driven exactly as a stock harness would drive it.
cat >"$test_dir/mcp-input.jsonl" <<'REQUESTS'
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_snapshot","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-11-25","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
REQUESTS
if ! DISPLAY="$display" AGENT_SEAT_SOCKET="$socket" "$companion_binary" \
    <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-output.jsonl" \
    2>"$test_dir/mcp-stderr.log"; then
    echo "the MCP companion exited with a failure" >&2
    cat "$test_dir/mcp-stderr.log" >&2
    exit 1
fi
if ! python3 "$(dirname "$0")/agent-mcp-check.py" "$test_dir/mcp-output.jsonl"; then
    echo "the MCP companion did not behave as revision 2026-07-28 requires" >&2
    cat "$test_dir/mcp-output.jsonl" >&2
    exit 1
fi

# The real test of isolation: after all of that, window management still works.
DISPLAY="$display" xterm -title nobox-agent-late -geometry 40x10+60+200 \
    >"$test_dir/xterm-late.log" 2>&1 &
late_pid=$!
wait_for_managed_windows 3 ||
    fail "nobox stopped managing windows after agent traffic"

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
