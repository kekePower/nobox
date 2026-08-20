#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-font-fallback.sh /path/to/nobox}
for dependency in xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 font-fallback test"
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

display=
for number in $(seq 151 170); do
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
    sed -n '1,120p' "$test_dir/xserver.log" >&2
    exit 1
fi

cat >"$test_dir/config.toml" <<'EOF'
[theme]
font = "-nobox-nonexistent-medium-r-normal--12-*-*-*-p-*-iso10646-1"
EOF

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
started=false
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then
        echo "nobox exited instead of falling back to the fixed title font" >&2
        sed -n '1,160p' "$test_dir/nobox.log" >&2
        exit 1
    fi
    if grep -q 'loaded X11 key bindings' "$test_dir/nobox.log"; then
        started=true
        break
    fi
    sleep 0.1
done
if [[ "$started" != true ]]; then
    echo "nobox did not finish starting with an unavailable title font" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi
if ! grep -q 'configured title font unavailable' "$test_dir/nobox.log"; then
    echo "nobox did not report the title-font fallback" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi
if ! DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK |
    grep -q 'window id #'; then
    echo "nobox did not publish _NET_SUPPORTING_WM_CHECK after the fallback" >&2
    exit 1
fi

echo "X11 title-font fallback test passed on $display"
