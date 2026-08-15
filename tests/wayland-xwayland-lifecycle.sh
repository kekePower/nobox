#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-xwayland-lifecycle.sh /path/to/nobox /path/to/probe}
probe_binary=${2:?missing Wayland probe binary}

for dependency in Xwayland cc pgrep; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the XWayland lifecycle test"
        exit 77
    fi
done

source "$(dirname "$0")/nested-x.sh"
if [[ -z ${NOBOX_XSERVER:-} ]]; then
    if command -v Xvfb >/dev/null 2>&1; then
        export NOBOX_XSERVER=xvfb
    elif command -v Xephyr >/dev/null 2>&1; then
        export NOBOX_XSERVER=xephyr
    fi
fi
select_nested_x_server 800 600

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
xserver_pid=
wayland_pid=
x11_client_pid=
wayland_selection_owner_pid=
x11_selection_owner_pid=
wayland_selection_observer_pid=
cleanup() {
    if [[ -n "$wayland_selection_observer_pid" ]]; then kill "$wayland_selection_observer_pid" 2>/dev/null || true; fi
    if [[ -n "$x11_selection_owner_pid" ]]; then kill "$x11_selection_owner_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_selection_owner_pid" ]]; then kill "$wayland_selection_owner_pid" 2>/dev/null || true; fi
    if [[ -n "$x11_client_pid" ]]; then kill "$x11_client_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_pid" ]]; then kill "$wayland_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    if [[ "${KEEP_TEST_DIR:-0}" == 1 ]]; then
        echo "kept XWayland test directory: $test_dir" >&2
    else
        rm -rf -- "$test_dir"
    fi
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/xwayland-scene-client.c" \
    -o "$test_dir/xwayland-scene-client" -lX11 || \
   ! cc "$(dirname "$0")/selection-client.c" \
    -o "$test_dir/selection-client" -lX11 || \
   ! cc "$(dirname "$0")/x11-largest-window-pixel.c" \
    -o "$test_dir/x11-largest-window-pixel" -lX11; then
    echo "SKIP: X11 development libraries are required for the XWayland scene test"
    exit 77
fi

display=
for number in $(seq 261 280); do
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

cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = false

[wayland]
xwayland = true
EOF

log="$test_dir/wayland.log"
env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    RUST_LOG=nobox_wayland=info \
    "$nobox_binary" --backend wayland --config "$test_dir/config.toml" \
    run --nested-x11 --no-autostart >"$log" 2>&1 &
wayland_pid=$!

socket=
for _ in $(seq 1 200); do
    socket=$(sed -n 's/^ready: //p' "$log" 2>/dev/null | head -n 1)
    if [[ -n "$socket" ]] && grep -Fq 'XWayland and its XWM are ready' "$log"; then
        break
    fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ -z "$socket" ]] || ! grep -Fq 'XWayland and its XWM are ready' "$log"; then
    echo "XWayland did not become ready" >&2
    cat "$log" >&2
    exit 1
fi

xwayland_pid=$(pgrep -P "$wayland_pid" -x Xwayland | head -n 1 || true)
if [[ -z "$xwayland_pid" ]]; then
    echo "ready XWayland process was not owned by the compositor" >&2
    exit 1
fi

xwayland_display=$(tr '\0' '\n' <"/proc/$xwayland_pid/cmdline" | \
    awk '/^:[0-9]+$/ { print; exit }')
if [[ -z "$xwayland_display" ]]; then
    echo "could not discover the compositor-owned XWayland display" >&2
    exit 1
fi
DISPLAY="$xwayland_display" "$test_dir/xwayland-scene-client" \
    >"$test_dir/x11-client.log" 2>&1 &
x11_client_pid=$!
scene_ready=false
for _ in $(seq 1 200); do
    if grep -Fq 'managed XWayland window through core policy' "$log" 2>/dev/null && \
       grep -Fq 'mapped unmanaged XWayland surface' "$log" 2>/dev/null && \
       grep -Fq 'focus=managed' "$test_dir/x11-client.log" 2>/dev/null && \
       grep -Fq 'geometry=520x360' "$test_dir/x11-client.log" 2>/dev/null; then
        if DISPLAY="$display" "$test_dir/x11-largest-window-pixel" ff0000 \
               >"$test_dir/x11-scene-pixel" 2>/dev/null || \
           DISPLAY="$display" "$test_dir/x11-largest-window-pixel" 00ff00 \
               >"$test_dir/x11-scene-pixel" 2>/dev/null; then
            scene_ready=true
            break
        fi
    fi
    if ! kill -0 "$wayland_pid" 2>/dev/null || \
       ! kill -0 "$x11_client_pid" 2>/dev/null; then
        break
    fi
    sleep 0.05
done
if [[ "$scene_ready" != true ]]; then
    echo "managed XWayland surface did not enter the rendered scene" >&2
    cat "$log" >&2
    cat "$test_dir/x11-client.log" >&2
    cat "$test_dir/x11-scene-pixel" >&2 2>/dev/null || true
    exit 1
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --shell >"$test_dir/native-before-crash"
grep -Fq 'shell-ok configures=' "$test_dir/native-before-crash"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --selection-owner >"$test_dir/wayland-selection-owner" 2>&1 &
wayland_selection_owner_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'selection-owner-ready' "$test_dir/wayland-selection-owner" 2>/dev/null; then break; fi
    if ! kill -0 "$wayland_selection_owner_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
grep -Fq 'selection-owner-ready' "$test_dir/wayland-selection-owner"
[[ $(DISPLAY="$xwayland_display" "$test_dir/selection-client" request CLIPBOARD text) == nobox-clipboard ]]
[[ $(DISPLAY="$xwayland_display" "$test_dir/selection-client" request PRIMARY text) == nobox-primary ]]

sed -i 's/xwayland = true/xwayland = false/' "$test_dir/config.toml"
kill -HUP "$wayland_pid"
for _ in $(seq 1 100); do
    if ! kill -0 "$xwayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if kill -0 "$xwayland_pid" 2>/dev/null; then
    echo "runtime disable did not stop XWayland" >&2
    exit 1
fi
if pgrep -P "$wayland_pid" -x Xwayland >/dev/null; then
    echo "runtime-disabled compositor retained an XWayland process" >&2
    exit 1
fi
kill "$x11_client_pid" 2>/dev/null || true
wait "$x11_client_pid" 2>/dev/null || true
x11_client_pid=
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --shell >"$test_dir/native-while-disabled"
grep -Fq 'shell-ok configures=' "$test_dir/native-while-disabled"

sed -i 's/xwayland = false/xwayland = true/' "$test_dir/config.toml"
kill -HUP "$wayland_pid"
for _ in $(seq 1 200); do
    if [[ $(grep -Fc 'XWayland and its XWM are ready' "$log" 2>/dev/null || true) -ge 2 ]]; then
        break
    fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ $(grep -Fc 'XWayland and its XWM are ready' "$log" 2>/dev/null || true) -lt 2 ]]; then
    echo "runtime re-enable did not make XWayland ready" >&2
    cat "$log" >&2
    exit 1
fi

replacement_pid=$(pgrep -P "$wayland_pid" -x Xwayland | head -n 1 || true)
if [[ -z "$replacement_pid" || "$replacement_pid" == "$xwayland_pid" ]]; then
    echo "runtime re-enable did not create a replacement XWayland process" >&2
    exit 1
fi
replacement_display=$(tr '\0' '\n' <"/proc/$replacement_pid/cmdline" | \
    awk '/^:[0-9]+$/ { print; exit }')
if [[ -z "$replacement_display" ]]; then
    echo "could not discover the replacement XWayland display" >&2
    exit 1
fi
[[ $(DISPLAY="$replacement_display" "$test_dir/selection-client" request CLIPBOARD text) == nobox-clipboard ]]
[[ $(DISPLAY="$replacement_display" "$test_dir/selection-client" request PRIMARY text) == nobox-primary ]]

kill "$wayland_selection_owner_pid"
wait "$wayland_selection_owner_pid" 2>/dev/null || true
wayland_selection_owner_pid=
for _ in $(seq 1 100); do
    if ! DISPLAY="$replacement_display" "$test_dir/selection-client" request CLIPBOARD text \
        >/dev/null 2>&1; then break; fi
    sleep 0.05
done
for selection in CLIPBOARD PRIMARY; do
    if DISPLAY="$replacement_display" "$test_dir/selection-client" request "$selection" text \
        >/dev/null 2>&1; then
        echo "dead native $selection selection remained readable through XWayland" >&2
        exit 1
    fi
done

DISPLAY="$replacement_display" "$test_dir/selection-client" own xwayland-selection \
    >"$test_dir/xwayland-selection-owner" 2>&1 &
x11_selection_owner_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'owner ' "$test_dir/xwayland-selection-owner" 2>/dev/null; then break; fi
    if ! kill -0 "$x11_selection_owner_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
grep -Fq 'owner ' "$test_dir/xwayland-selection-owner"
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --xwayland-selection-observer \
    >"$test_dir/xwayland-selection-observer" 2>&1 &
wayland_selection_observer_pid=$!
for _ in $(seq 1 100); do
    if grep -Fq 'xwayland-selection-observer-ready' \
        "$test_dir/xwayland-selection-observer" 2>/dev/null; then break; fi
    if ! kill -0 "$wayland_selection_observer_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if ! grep -Fq 'xwayland-selection-observer-ready' \
    "$test_dir/xwayland-selection-observer"; then
    echo "XWayland selection did not reach the native Wayland seat" >&2
    cat "$test_dir/xwayland-selection-observer" >&2
    exit 1
fi
kill -KILL "$replacement_pid"
wait "$x11_selection_owner_pid" 2>/dev/null || true
x11_selection_owner_pid=
wait "$wayland_selection_observer_pid"
wayland_selection_observer_pid=
grep -Fq 'xwayland-selection-owner-death-ok' "$test_dir/xwayland-selection-observer"
for _ in $(seq 1 200); do
    if [[ $(grep -Fc 'XWayland and its XWM are ready' "$log" 2>/dev/null || true) -ge 3 ]]; then
        break
    fi
    if ! kill -0 "$wayland_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ $(grep -Fc 'XWayland and its XWM are ready' "$log" 2>/dev/null || true) -lt 3 ]]; then
    echo "XWayland crash did not produce an isolated restart" >&2
    cat "$log" >&2
    exit 1
fi

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --shell >"$test_dir/native-after-crash"
grep -Fq 'shell-ok configures=' "$test_dir/native-after-crash"

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    "$nobox_binary" --backend wayland --exit
wait "$wayland_pid"
wayland_pid=

echo "XWayland scene, bidirectional selections, lifecycle, and native-client survival passed"
