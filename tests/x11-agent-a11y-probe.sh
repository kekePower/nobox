#!/usr/bin/env bash
set -euo pipefail

usage="usage: x11-agent-a11y-probe.sh /path/to/nobox /path/to/probe /path/to/helper /path/to/seat-probe"
nobox_binary=${1:?$usage}
probe=${2:?$usage}
semantic_helper=${3:?$usage}
seat_probe=${4:?$usage}
qt_client=${5:-}

for dependency in dbus-run-session gdbus gtk4-demo python3 timeout xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the AT-SPI discovery probe"
        exit 77
    fi
done
if ! python3 -c 'import gi; gi.require_version("Atspi", "2.0")' 2>/dev/null; then
    echo "SKIP: Python AT-SPI introspection bindings are unavailable"
    exit 77
fi

# A private session bus avoids changing or depending on the user's desktop
# accessibility setting. Re-exec once so every process below shares it.
if [[ ${NOBOX_A11Y_PRIVATE_BUS:-0} != 1 ]]; then
    exec dbus-run-session -- env NOBOX_A11Y_PRIVATE_BUS=1 \
        bash "$0" "$nobox_binary" "$probe" "$semantic_helper" "$seat_probe" "$qt_client"
fi

source "$(dirname "$0")/nested-x.sh"

helper_root_matches() {
    python3 -c '
import json,sys
value=json.loads(sys.argv[1]); width=int(sys.argv[2]); height=int(sys.argv[3])
root=value.get("root", {})
assert value.get("v") == 1 and value.get("status") == "matched"
assert root.get("role") in ("window", "dialog")
assert isinstance(root.get("states"), list) and isinstance(root.get("child_count"), int)
assert root.get("bounds", {}).get("x") == 0
assert root.get("bounds", {}).get("y") == 0
assert root.get("bounds", {}).get("width") == width
assert root.get("bounds", {}).get("height") == height
' "$1" "$2" "$3"
}
# GTK 4's X11 backend can crash during startup on this system's Xnest, which
# normally lacks the visual/GLX setup modern toolkits expect. Prefer the fully
# isolated framebuffer for this toolkit probe while honoring explicit choices.
if [[ ${NOBOX_XSERVER:-auto} == auto ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 1280 800

test_dir=$(mktemp -d)
runtime_dir="$test_dir/run"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"
seat_probe_bound="$test_dir/nobox-agent-wire-probe"
cp -- "$seat_probe" "$seat_probe_bound"
cat >"$test_dir/config.toml" <<EOF
[agent]
enabled = true
policy = "deny"

[[agent.grants]]
label = "semantic projection probe"
executable = "$seat_probe_bound"
capabilities = ["observe", "accessibility", "capture"]
EOF
xserver_pid=
nobox_pid=
gtk_pid=
cleanup() {
    if [[ -n "$gtk_pid" ]]; then kill "$gtk_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    find "$test_dir" -type f -delete 2>/dev/null || true
    rmdir "$test_dir" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 90 110); do
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

env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then
        break
    fi
    sleep 0.1
done
if ! DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
    grep -q 'window id'; then
    echo "nobox did not claim the nested display" >&2
    exit 1
fi
socket="$runtime_dir/nobox/agent-seat-${display#:}.sock"
if [[ ! -S "$socket" ]]; then
    echo "nobox did not publish the agent seat at $socket" >&2
    exit 1
fi

# Starting the private AT-SPI bus before GTK makes discovery independent of a
# host desktop's accessibility setting.
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >/dev/null
env -u WAYLAND_DISPLAY DISPLAY="$display" GDK_BACKEND=x11 GTK_A11Y=atspi \
    NO_AT_BRIDGE=0 GDK_DEBUG=no-portals \
    gtk4-demo >"$test_dir/gtk.log" 2>&1 &
gtk_pid=$!

client=
for _ in $(seq 1 100); do
    while read -r candidate; do
        [[ -n "$candidate" ]] || continue
        pid=$(DISPLAY="$display" xprop -id "$candidate" _NET_WM_PID 2>/dev/null |
            sed -n 's/.* = //p')
        if [[ "$pid" == "$gtk_pid" ]]; then
            width=$(DISPLAY="$display" xwininfo -id "$candidate" 2>/dev/null |
                awk -F: '/^  Width:/{gsub(/ /,"",$2); print $2}')
            if [[ ${width:-0} -gt 100 ]]; then
                client=$candidate
                break
            fi
        fi
    done < <(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -oE '0x[0-9a-fA-F]+')
    [[ -n "$client" ]] && break
    sleep 0.1
done
if [[ -z "$client" ]]; then
    echo "GTK did not map a managed test window" >&2
    exit 1
fi

eval "$(DISPLAY="$display" xwininfo -id "$client" | awk -F: '
    /Absolute upper-left X:/{gsub(/ /,"",$2); print "client_x=" $2}
    /Absolute upper-left Y:/{gsub(/ /,"",$2); print "client_y=" $2}
    /^  Width:/{gsub(/ /,"",$2); print "client_width=" $2}
    /^  Height:/{gsub(/ /,"",$2); print "client_height=" $2}')"

request=$(python3 -c '
import json,sys
pid,x,y,w,h=map(int,sys.argv[1:])
print(json.dumps({"v":1,"pids":[pid],"rects":[{"x":x,"y":y,"width":w,"height":h}],"single_client":True},separators=(",",":")))
' "$gtk_pid" "$client_x" "$client_y" "$client_width" "$client_height")
result=$(printf '%s' "$request" | DISPLAY="$display" timeout 3s "$probe")
if [[ "$result" != '{"v":1,"status":"matched"}' ]]; then
    echo "the isolated GTK root did not correlate: $result" >&2
    exit 1
fi
result=$(printf '%s' "$request" | DISPLAY="$display" timeout 3s "$semantic_helper")
if ! helper_root_matches "$result" "$client_width" "$client_height"; then
    echo "the Rust helper did not correlate the isolated GTK root: $result" >&2
    exit 1
fi
client_title=$(DISPLAY="$display" xprop -id "$client" _NET_WM_NAME 2>/dev/null |
    sed -n 's/^[^=]*= "\(.*\)"$/\1/p')
if [[ -z "$client_title" ]]; then
    echo "the GTK window has no title for protocol correlation" >&2
    exit 1
fi
for _ in 1 2 3; do
    if ! DISPLAY="$display" timeout 8s "$seat_probe_bound" "$socket" semantic-root \
        nobox-a11y-integration-probe "$client_title" \
        >>"$test_dir/semantic-root.log" 2>&1; then
        echo "the manager did not return the scaled GTK semantic root" >&2
        sed -n '1,80p' "$test_dir/semantic-root.log" >&2
        sed -n '1,160p' "$test_dir/nobox.log" >&2
        exit 1
    fi
done

missing=$(python3 -c '
import json,sys
value=json.loads(sys.argv[1]); value["pids"]=[value["pids"][0]+1000000]
print(json.dumps(value,separators=(",",":")))
' "$request")
result=$(printf '%s' "$missing" | DISPLAY="$display" timeout 3s "$probe")
if [[ "$result" != '{"v":1,"status":"unavailable"}' ]]; then
    echo "an unrelated process did not fail closed: $result" >&2
    exit 1
fi
result=$(printf '%s' "$missing" | DISPLAY="$display" timeout 3s "$semantic_helper")
if [[ "$result" != '{"v":1,"status":"unavailable"}' ]]; then
    echo "the Rust helper did not fail closed for an unrelated process: $result" >&2
    exit 1
fi

if [[ -n "$qt_client" ]]; then
    kill "$gtk_pid" 2>/dev/null || true
    wait "$gtk_pid" 2>/dev/null || true
    gtk_pid=
    env -u WAYLAND_DISPLAY DISPLAY="$display" QT_QPA_PLATFORM=xcb \
        QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 NO_AT_BRIDGE=0 \
        "$qt_client" >"$test_dir/qt.log" 2>&1 &
    gtk_pid=$!

    client=
    for _ in $(seq 1 100); do
        while read -r candidate; do
            [[ -n "$candidate" ]] || continue
            pid=$(DISPLAY="$display" xprop -id "$candidate" _NET_WM_PID 2>/dev/null |
                sed -n 's/.* = //p')
            if [[ "$pid" == "$gtk_pid" ]]; then
                width=$(DISPLAY="$display" xwininfo -id "$candidate" 2>/dev/null |
                    awk -F: '/^  Width:/{gsub(/ /,"",$2); print $2}')
                if [[ ${width:-0} -gt 100 ]]; then
                    client=$candidate
                    break
                fi
            fi
        done < <(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
            grep -oE '0x[0-9a-fA-F]+')
        [[ -n "$client" ]] && break
        sleep 0.1
    done
    if [[ -z "$client" ]]; then
        echo "Qt did not map a managed test window" >&2
        exit 1
    fi

    eval "$(DISPLAY="$display" xwininfo -id "$client" | awk -F: '
        /Absolute upper-left X:/{gsub(/ /,"",$2); print "client_x=" $2}
        /Absolute upper-left Y:/{gsub(/ /,"",$2); print "client_y=" $2}
        /^  Width:/{gsub(/ /,"",$2); print "client_width=" $2}
        /^  Height:/{gsub(/ /,"",$2); print "client_height=" $2}')"
    request=$(python3 -c '
import json,sys
pid,x,y,w,h=map(int,sys.argv[1:])
print(json.dumps({"v":1,"pids":[pid],"rects":[{"x":x,"y":y,"width":w,"height":h}],"single_client":True},separators=(",",":")))
' "$gtk_pid" "$client_x" "$client_y" "$client_width" "$client_height")
    result=$(printf '%s' "$request" | DISPLAY="$display" timeout 3s "$probe")
    if [[ "$result" != '{"v":1,"status":"matched"}' ]]; then
        echo "the isolated Qt root did not correlate: $result" >&2
        exit 1
    fi
    result=$(printf '%s' "$request" | DISPLAY="$display" timeout 3s "$semantic_helper")
    if ! helper_root_matches "$result" "$client_width" "$client_height"; then
        echo "the Rust helper did not correlate the isolated Qt root: $result" >&2
        exit 1
    fi
    client_title=$(DISPLAY="$display" xprop -id "$client" _NET_WM_NAME 2>/dev/null |
        sed -n 's/^[^=]*= "\(.*\)"$/\1/p')
    if [[ -z "$client_title" ]]; then
        echo "the Qt window has no title for protocol correlation" >&2
        exit 1
    fi
    for _ in 1 2 3; do
        if ! DISPLAY="$display" timeout 8s "$seat_probe_bound" "$socket" semantic-root \
            nobox-a11y-integration-probe "$client_title" \
            >>"$test_dir/semantic-root-qt.log" 2>&1; then
            echo "the manager did not return the scaled Qt semantic root" >&2
            sed -n '1,80p' "$test_dir/semantic-root-qt.log" >&2
            sed -n '1,160p' "$test_dir/nobox.log" >&2
            exit 1
        fi
    done
fi

python3 - "$test_dir/semantic-root.log" "$test_dir/semantic-root-qt.log" <<'PY'
import json
import os
import sys

summary = {}
for path, family in zip(sys.argv[1:], ("gtk", "qt"), strict=True):
    if not os.path.exists(path):
        continue
    with open(path, encoding="utf-8") as source:
        rows = [json.loads(line) for line in source]
    assert len(rows) == 3, rows
    assert all(row["semantic"]["json_bytes"] < row["capture"]["json_bytes"]
               for row in rows), rows
    assert all(row["capture"]["png_bytes"] > 0 for row in rows), rows
    summary[family] = rows
print(json.dumps({"runs_per_family": 3, "measurements": summary},
                 separators=(",", ":")))
PY

echo "bounded AT-SPI discovery probe passed on $display"
