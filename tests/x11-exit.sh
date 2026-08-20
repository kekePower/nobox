#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-exit.sh /path/to/nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 exit test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
nobox_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! cc "$(dirname "$0")/press-key.c" -o "$test_dir/press-key" -lXtst -lX11; then
    echo "SKIP: XTest development libraries are required for the X11 exit test"
    exit 77
fi
cat >"$test_dir/config.toml" <<'EOF'
[[keyboard.bindings]]
key = "W-F10"
action = { type = "exit" }

[[keyboard.bindings]]
key = "W-F11"
action = { type = "exit", prompt = false }
EOF

display=
for number in $(seq 431 450); do
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
if DISPLAY="$display" "$nobox_binary" --exit \
    >"$test_dir/missing.out" 2>"$test_dir/missing.err"; then
    echo "--exit succeeded without a running nobox instance" >&2
    exit 1
fi
if ! grep -q 'no running nobox instance' "$test_dir/missing.err"; then
    echo "--exit did not diagnose the missing manager" >&2
    cat "$test_dir/missing.err" >&2
    exit 1
fi

start_manager() {
    local supporting_window=
    local runtime_instance=

    DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
        "$nobox_binary" run --no-autostart >>"$test_dir/nobox.log" 2>&1 &
    nobox_pid=$!
    for _ in $(seq 1 50); do
        supporting_window=$(DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK \
            2>/dev/null | sed -n 's/.*# //p')
        if [[ -n "$supporting_window" ]]; then
            runtime_instance=$(DISPLAY="$display" xprop -id "$supporting_window" \
                _NOBOX_RUNTIME_INSTANCE 2>/dev/null | \
                sed -n 's/.*= "\(.*\)"/\1/p')
        fi
        if [[ -n "$runtime_instance" ]] && DISPLAY="$display" \
            xprop -id "$supporting_window" _NET_SUPPORTING_WM_CHECK \
                >/dev/null 2>&1; then
            return 0
        fi
        kill -0 "$nobox_pid" 2>/dev/null || break
        sleep 0.05
    done
    echo "nobox did not claim the nested X11 server" >&2
    return 1
}

wait_for_exit() {
    for _ in $(seq 1 50); do
        if ! kill -0 "$nobox_pid" 2>/dev/null; then
            wait "$nobox_pid"
            nobox_pid=
            return 0
        fi
        sleep 0.05
    done
    echo "nobox did not exit after confirmed action" >&2
    return 1
}

start_manager
DISPLAY="$display" "$test_dir/press-key" F10
sleep 0.1
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "default Exit bypassed confirmation" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/press-key" --plain Escape
sleep 0.1
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "cancelled Exit stopped the manager" >&2
    exit 1
fi
DISPLAY="$display" "$test_dir/press-key" F10
DISPLAY="$display" "$test_dir/press-key" --plain Down
DISPLAY="$display" "$test_dir/press-key" --plain Return
wait_for_exit

start_manager
DISPLAY="$display" "$test_dir/press-key" F11
wait_for_exit

start_manager
DISPLAY="$display" "$nobox_binary" --exit
wait_for_exit

echo "X11 local and Openbox-compatible remote Exit paths passed"
