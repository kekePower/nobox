#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-agent-foundation.sh NOBOX WAYLAND_PROBE AGENT_PROBE}
wayland_probe=${2:?missing Wayland probe}
agent_probe=${3:?missing Agent Seat probe}
companion=${4:-}

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
watch_pid=
cleanup() {
    if [[ -n "$watch_pid" ]]; then kill "$watch_pid" 2>/dev/null || true; fi
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
applications_dir="$test_dir/data/applications"
mkdir -p "$applications_dir"
cat >"$applications_dir/org.nobox.AgentLaunch.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Nobox Agent Launch Probe
Exec=$wayland_probe --agent-environment $test_dir/agent-environment.log $agent_socket
StartupNotify=true
EOF
cat >"$applications_dir/org.nobox.AgentDenied.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Nobox Denied Agent Launch Probe
Exec=$wayland_probe --agent-hold
StartupNotify=true
EOF
cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = false

[agent]
enabled = true
socket = "$agent_socket"
policy = "ask"

[agent.launch]
policy = "allow_listed"
allow = ["org.nobox.AgentLaunch.desktop"]
user_entries = true

[[agent.grants]]
label = "nested Wayland Agent Seat probe"
executable = "$agent_probe"
capabilities = [
    "observe.structure",
    "observe.titles",
    "manage.activate",
    "manage.geometry",
    "manage.close",
    "manage.state",
    "manage.workspace",
    "launch.desktop",
    "capture.client_visible",
    "capture.client_obscured",
    "capture.output",
    "input.pointer",
    "input.keyboard",
]
EOF
if [[ -n "$companion" && -x "$companion" ]]; then
cat >>"$test_dir/config.toml" <<EOF

[[agent.grants]]
label = "nested Wayland MCP companion"
executable = "$companion"
capabilities = [
    "observe.structure",
    "observe.titles",
    "manage.activate",
    "manage.geometry",
    "manage.close",
    "manage.state",
    "manage.workspace",
    "launch.desktop",
    "capture.client_visible",
    "capture.client_obscured",
    "capture.output",
    "input.pointer",
    "input.keyboard",
]
EOF
fi

log="$test_dir/nobox.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    XDG_DATA_HOME="$test_dir/data" \
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

if [[ -n "$companion" && -x "$companion" ]]; then
cat >"$test_dir/mcp-input.jsonl" <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_snapshot","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-11-25","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":6,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
EOF
AGENT_SEAT_SOCKET="$agent_socket" XDG_RUNTIME_DIR="$runtime_dir" \
    "$companion" <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-output.jsonl" \
    2>"$test_dir/mcp-stderr.log"
python3 - "$test_dir/mcp-output.jsonl" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    responses = {message.get("id"): message for line in stream
                 if line.strip() for message in [json.loads(line)]}
assert len(responses) == 6, responses
assert responses[1]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"] == "nobox-agent"
assert responses[2]["result"]["tools"] == responses[6]["result"]["tools"]
call = responses[3]["result"]
assert call["isError"] is False, call
snapshot = call["structuredContent"]["snapshot"]
assert snapshot["workspaces"] and snapshot["outputs"], snapshot
assert responses[4]["error"]["code"] == -32602
assert responses[5]["error"]["code"] == -32022
PY
fi

consent_probe="$test_dir/nobox-agent-wire-consent-probe"
cp -- "$agent_probe" "$consent_probe"
"$consent_probe" "$agent_socket" consent wayland-consent granted \
    >"$test_dir/consent-once.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'asked' "$test_dir/consent-once.log"; then break; fi
    sleep 0.02
done
grep -Fq 'asked' "$test_dir/consent-once.log"
DISPLAY="$display" "$wayland_probe" --agent-consent-once \
    >"$test_dir/consent-once-key.log" 2>&1
wait "$watch_pid"
watch_pid=
grep -Fq 'answered granted=observe.structure,observe.titles' \
    "$test_dir/consent-once.log"

"$consent_probe" "$agent_socket" consent wayland-consent denied \
    >"$test_dir/consent-deny.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'asked' "$test_dir/consent-deny.log"; then break; fi
    sleep 0.02
done
grep -Fq 'asked' "$test_dir/consent-deny.log"
DISPLAY="$display" "$wayland_probe" --agent-consent-deny \
    >"$test_dir/consent-deny-key.log" 2>&1
wait "$watch_pid"
watch_pid=
grep -Fq 'answered granted=' "$test_dir/consent-deny.log"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
    "$wayland_probe" --agent-hold >"$test_dir/client.log" 2>&1 &
client_pid=$!

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

wait "$client_pid"
client_pid=
"$agent_probe" "$agent_socket" watch wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/watch.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'tool="subscribe_and_snapshot"' "$log"; then break; fi
    if ! kill -0 "$watch_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
grep -Fq 'tool="subscribe_and_snapshot"' "$log"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
    "$wayland_probe" --agent-hold >"$test_dir/watched-client.log" 2>&1 &
client_pid=$!
wait "$client_pid"
client_pid=
wait "$watch_pid"
watch_pid=
grep -Fq 'mapped ' "$test_dir/watch.log"
grep -Fq 'watched window appeared and went away' "$test_dir/watch.log"

"$agent_probe" "$agent_socket" launch wayland-foundation \
    org.nobox.AgentLaunch.desktop org.nobox.AgentDenied.desktop \
    >"$test_dir/launch.log" 2>&1
grep -Fq 'launch refused:' "$test_dir/launch.log"
grep -Fq 'launched org.nobox.AgentLaunch.desktop as ' "$test_dir/launch.log"
grep -Fq 'correlated ' "$test_dir/launch.log"
grep -Fq 'agent-environment-ok' "$test_dir/agent-environment.log"
sleep 1.1

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
    "$wayland_probe" --agent-hold-long >"$test_dir/managed-client.log" 2>&1 &
client_pid=$!
for _ in $(seq 1 50); do
    if "$agent_probe" "$agent_socket" snapshot wayland-foundation \
        >"$test_dir/managed-snapshot.log" 2>&1 && \
        grep -Fq 'title=nobox Wayland agent visible' "$test_dir/managed-snapshot.log"; then
        break
    fi
    sleep 0.05
done
grep -Fq 'title=nobox Wayland agent visible' "$test_dir/managed-snapshot.log"
"$agent_probe" "$agent_socket" input wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/input.log" 2>&1
grep -Fq 'clicked, committed' "$test_dir/input.log"
grep -Fq 'typed exact Unicode text' "$test_dir/input.log"
grep -Fq 'a point outside the window was refused' "$test_dir/input.log"

DISPLAY="$display" "$wayland_probe" --agent-human-key \
    >"$test_dir/human-key.log" 2>&1
sleep 0.05
"$agent_probe" "$agent_socket" interrupted wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/interrupted.log" 2>&1
grep -Fq 'interrupted, committed' "$test_dir/interrupted.log"

sleep 1
"$agent_probe" "$agent_socket" text-interrupted wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/text-interrupted.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'ready' "$test_dir/text-interrupted.log"; then break; fi
    sleep 0.02
done
grep -Fq 'ready' "$test_dir/text-interrupted.log"
sleep 0.05
DISPLAY="$display" "$wayland_probe" --agent-human-key \
    >"$test_dir/text-human-key.log" 2>&1
wait "$watch_pid"
watch_pid=
grep -Fq 'text interrupted after a committed prefix' "$test_dir/text-interrupted.log"

"$agent_probe" "$agent_socket" freeze wayland-foundation \
    >"$test_dir/freeze.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'ready' "$test_dir/freeze.log"; then break; fi
    sleep 0.02
done
grep -Fq 'ready' "$test_dir/freeze.log"
DISPLAY="$display" "$wayland_probe" --agent-freeze \
    >"$test_dir/freeze-on.log" 2>&1
for _ in $(seq 1 50); do
    if grep -Fq 'refused while frozen' "$test_dir/freeze.log"; then break; fi
    sleep 0.02
done
grep -Fq 'refused while frozen' "$test_dir/freeze.log"
DISPLAY="$display" "$wayland_probe" --agent-freeze \
    >"$test_dir/freeze-off.log" 2>&1
wait "$watch_pid"
watch_pid=
grep -Fq 'resumed' "$test_dir/freeze.log"

"$agent_probe" "$agent_socket" capture-covered wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/client-capture.log" 2>&1
grep -Fq 'captured a covered window as' "$test_dir/client-capture.log"
"$agent_probe" "$agent_socket" output-capture wayland-foundation \
    >"$test_dir/output-capture.log" 2>&1
grep -Fq 'captured the output as' "$test_dir/output-capture.log"
grep -Fq 'captured an output crop at' "$test_dir/output-capture.log"
"$agent_probe" "$agent_socket" minimize wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/minimize.log" 2>&1
grep -Fq 'minimized, committed [State]' "$test_dir/minimize.log"
"$agent_probe" "$agent_socket" restore wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/restore.log" 2>&1
grep -Fq 'restored, committed [State]' "$test_dir/restore.log"
"$agent_probe" "$agent_socket" manage wayland-foundation \
    "nobox Wayland agent visible" >"$test_dir/manage.log" 2>&1
grep -Fq 'activated across a workspace boundary' "$test_dir/manage.log"
grep -Fq 'stale_state -> re-observe' "$test_dir/manage.log"
grep -Fq 'moved to' "$test_dir/manage.log"
grep -Fq 'the window closed through its own protocol' "$test_dir/manage.log"
wait "$client_pid"
client_pid=
grep -Eq 'pointer_presses=[1-9][0-9]*' "$test_dir/managed-client.log"
grep -Eq 'key_events=[1-9][0-9]*' "$test_dir/managed-client.log"

"$agent_probe" "$agent_socket" revoke wayland-foundation \
    >"$test_dir/revoke.log" 2>&1 &
watch_pid=$!
for _ in $(seq 1 50); do
    if grep -Fq 'ready' "$test_dir/revoke.log"; then break; fi
    sleep 0.02
done
grep -Fq 'ready' "$test_dir/revoke.log"
sed -i "s|executable = \"$agent_probe\"|executable = \"$consent_probe\"|" \
    "$test_dir/config.toml"
kill -HUP "$wayland_pid"
wait "$watch_pid"
watch_pid=
grep -Fq 'revoked' "$test_dir/revoke.log"
grep -Fq 'refused after revocation' "$test_dir/revoke.log"
[[ -S "$agent_socket" ]]

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=
wait "$client_pid" 2>/dev/null || true
client_pid=
[[ ! -e "$agent_socket" ]]

echo "nested Wayland Agent Seat observation foundation passed on $nested_x_server $display"
