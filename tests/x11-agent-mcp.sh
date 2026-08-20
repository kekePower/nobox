#!/usr/bin/env bash
set -euo pipefail

# The end-to-end flow from docs/agent-roadmap.md, driven through the real MCP
# companion by a host that knows nothing nobox-specific.

usage="usage: x11-agent-mcp.sh /path/to/nobox /path/to/nobox-agent"
nobox_binary=$(readlink -f "${1:?$usage}")
companion_binary=$(readlink -f "${2:?$usage}")
for dependency in cc xdpyinfo xprop xterm python3; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the agent MCP flow test"
        exit 77
    fi
done
if [[ ! -x "$companion_binary" ]]; then
    echo "SKIP: the MCP companion was not built at $companion_binary"
    exit 77
fi

helpers=$(mktemp -d)
if ! cc "$(dirname "$0")/press-key.c" -o "$helpers/press-key" -lXtst -lX11 2>/dev/null; then
    echo "SKIP: XTest development files are required for the agent MCP flow test"
    rm -rf -- "$helpers"
    exit 77
fi

source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
cleanup() {
    for pid in "$nobox_pid" "$xserver_pid"; do
        if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi
    done
    rm -rf -- "$test_dir" "$helpers"
}
trap cleanup EXIT INT TERM

fail() {
    echo "$1" >&2
    sed -n '1,200p' "$test_dir/nobox.log" >&2
    exit 1
}

runtime_dir="$test_dir/run"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"

data_home="$test_dir/data"
mkdir -p "$data_home/applications"
cat >"$data_home/applications/nobox-flow-app.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=nobox agent flow fixture
Exec=xterm -title nobox-flow-app -e sleep 600
StartupNotify=true
StartupWMClass=XTerm
Categories=Utility;
ENTRY

# One grant, holding everything the flow needs, bound to the companion's own
# executable.
cat >"$test_dir/config.toml" <<EOF
[agent]
enabled = true
suppression_ms = 1500
kill_chord = "C-A-Escape"

[[agent.grants]]
label = "flow companion"
executable = "$companion_binary"
capabilities = ["observe", "manage", "input", "capture", "launch"]

[agent.launch]
policy = "allow_listed"
allow = ["nobox-flow-app.desktop"]
user_entries = true
EOF

display=
for number in $(seq 451 470); do
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
DISPLAY="$display" xdpyinfo >/dev/null 2>&1 ||
    { echo "$nested_x_server did not become ready" >&2; exit 1; }

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" XDG_DATA_HOME="$data_home" \
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

if ! DISPLAY="$display" python3 "$(dirname "$0")/agent-mcp-flow.py" \
    "$companion_binary" "$socket" "$helpers/press-key" nobox-flow-app.desktop; then
    fail "the end-to-end agent flow failed"
fi

kill -0 "$nobox_pid" 2>/dev/null || fail "nobox died during the flow"

echo "agent MCP flow passed on $display"
