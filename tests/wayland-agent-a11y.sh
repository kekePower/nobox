#!/usr/bin/env bash
set -euo pipefail

usage="usage: wayland-agent-a11y.sh NOBOX AGENT_PROBE"
nobox_binary=${1:?$usage}
agent_probe=${2:?$usage}

for dependency in dbus-run-session gdbus gtk4-demo python3 xdpyinfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the native Wayland accessibility test"
        exit 77
    fi
done
if ! python3 -c 'import gi; gi.require_version("Atspi", "2.0")' 2>/dev/null; then
    echo "SKIP: Python AT-SPI introspection bindings are unavailable"
    exit 77
fi
if [[ ${NOBOX_A11Y_PRIVATE_BUS:-0} != 1 ]]; then
    private_test_dir=$(mktemp -d)
    mkdir -m 700 "$private_test_dir/nested-runtime"
    exec env XDG_RUNTIME_DIR="$private_test_dir/nested-runtime" \
        dbus-run-session -- env NOBOX_A11Y_PRIVATE_BUS=1 \
        NOBOX_A11Y_TEST_DIR="$private_test_dir" NOBOX_XSERVER="${NOBOX_XSERVER:-}" \
        bash "$0" "$nobox_binary" "$agent_probe"
fi

source "$(dirname "$0")/nested-x.sh"
if [[ -z ${NOBOX_XSERVER:-} ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 1000 700

test_dir=${NOBOX_A11Y_TEST_DIR:-$(mktemp -d)}
isolate_nested_session "$test_dir" private-bus
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
probe_bound="$test_dir/nobox-agent-wire-probe"
cp -- "$agent_probe" "$probe_bound"
agent_socket="$runtime_dir/nobox/wayland-agent-a11y.sock"
cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = false

[agent]
enabled = true
socket = "$agent_socket"
policy = "deny"

[[agent.grants]]
label = "native Wayland accessibility probe"
executable = "$probe_bound"
capabilities = ["observe", "accessibility", "capture"]
EOF

xserver_pid=
wayland_pid=
gtk_pid=
cleanup() {
    if [[ -n "$gtk_pid" ]]; then kill "$gtk_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_pid" ]]; then kill "$wayland_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    if [[ "${KEEP_TEST_DIR:-0}" == 1 ]]; then
        echo "kept native Wayland accessibility test directory: $test_dir" >&2
    else
        find "$test_dir" -type f -delete 2>/dev/null || true
        find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 291 300); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
[[ -n "$display" ]]
"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done
DISPLAY="$display" xdpyinfo >/dev/null

env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_STATE_FILE="$test_dir/session.toml" \
    "$nobox_binary" --backend wayland --config "$test_dir/config.toml" \
    run --nested-x11 --no-autostart >"$test_dir/nobox.log" 2>&1 &
wayland_pid=$!
wayland_socket=
for _ in $(seq 1 100); do
    wayland_socket=$(sed -n 's/^ready: //p' "$test_dir/nobox.log" | head -n 1)
    if [[ -n "$wayland_socket" && -S "$agent_socket" ]]; then break; fi
    sleep 0.05
done
[[ -n "$wayland_socket" && -S "$agent_socket" ]]

gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >/dev/null
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$wayland_socket" \
    GDK_BACKEND=wayland GTK_A11Y=atspi NO_AT_BRIDGE=0 GDK_DEBUG=no-portals \
    gtk4-demo >"$test_dir/gtk.log" 2>&1 &
gtk_pid=$!

for _ in $(seq 1 100); do
    if "$probe_bound" "$agent_socket" snapshot wayland-a11y \
        >"$test_dir/snapshot.log" 2>&1 && grep -Fq 'GTK Demo' "$test_dir/snapshot.log"; then
        break
    fi
    sleep 0.1
done
grep -Fq 'GTK Demo' "$test_dir/snapshot.log"
timeout 15s "$probe_bound" "$agent_socket" semantic-root wayland-a11y \
    "GTK Demo" >"$test_dir/semantic.log" 2>&1
grep -Fq '"semantic":{"calls":2' "$test_dir/semantic.log"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=
echo "native Wayland accessibility correlation passed on $nested_x_server $display"
