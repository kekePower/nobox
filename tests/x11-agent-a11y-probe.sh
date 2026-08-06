#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-agent-a11y-probe.sh /path/to/nobox /path/to/probe /path/to/helper}
probe=${2:?usage: x11-agent-a11y-probe.sh /path/to/nobox /path/to/probe /path/to/helper}
semantic_helper=${3:?usage: x11-agent-a11y-probe.sh /path/to/nobox /path/to/probe /path/to/helper}
qt_client=${4:-}

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
        bash "$0" "$nobox_binary" "$probe" "$semantic_helper" "$qt_client"
fi

source "$(dirname "$0")/nested-x.sh"
# GTK 4's X11 backend can crash during startup on this system's Xnest, which
# normally lacks the visual/GLX setup modern toolkits expect. Prefer the fully
# isolated framebuffer for this toolkit probe while honoring explicit choices.
if [[ ${NOBOX_XSERVER:-auto} == auto ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 1280 800

test_dir=$(mktemp -d)
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
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

# Starting the private AT-SPI bus before GTK makes discovery independent of a
# host desktop's accessibility setting.
gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >/dev/null
DISPLAY="$display" GTK_A11Y=atspi NO_AT_BRIDGE=0 GDK_DEBUG=no-portals \
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
if [[ "$result" != '{"v":1,"status":"matched"}' ]]; then
    echo "the Rust helper did not correlate the isolated GTK root: $result" >&2
    exit 1
fi

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
    DISPLAY="$display" QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 NO_AT_BRIDGE=0 \
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
    if [[ "$result" != '{"v":1,"status":"matched"}' ]]; then
        echo "the Rust helper did not correlate the isolated Qt root: $result" >&2
        exit 1
    fi
fi

echo "bounded AT-SPI discovery probe passed on $display"
