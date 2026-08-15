#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-xwayland-lifecycle.sh /path/to/nobox /path/to/probe}
probe_binary=${2:?missing Wayland probe binary}

for dependency in Xwayland cc pgrep xdpyinfo xprop xwininfo; do
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
mkdir -p "$test_dir/data/applications" "$test_dir/empty-data"
xserver_pid=
wayland_pid=
x11_client_pid=
x11_group_client_pid=
trusted_activation_pid=
gtk_client_pid=
qt_client_pid=
wayland_dnd_pid=
x11_dnd_pid=
wayland_selection_owner_pid=
x11_selection_owner_pid=
wayland_selection_observer_pid=
cleanup() {
    if [[ -n "$x11_dnd_pid" ]]; then kill "$x11_dnd_pid" 2>/dev/null || true; fi
    if [[ -n "$wayland_dnd_pid" ]]; then kill "$wayland_dnd_pid" 2>/dev/null || true; fi
    if [[ -n "$trusted_activation_pid" ]]; then kill "$trusted_activation_pid" 2>/dev/null || true; fi
    if [[ -n "$qt_client_pid" ]]; then kill "$qt_client_pid" 2>/dev/null || true; fi
    if [[ -n "$gtk_client_pid" ]]; then kill "$gtk_client_pid" 2>/dev/null || true; fi
    if [[ -n "$x11_group_client_pid" ]]; then kill "$x11_group_client_pid" 2>/dev/null || true; fi
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
   ! cc "$(dirname "$0")/xwayland-group-client.c" \
    -o "$test_dir/xwayland-group-client" -lX11 || \
   ! cc "$(dirname "$0")/selection-client.c" \
    -o "$test_dir/selection-client" -lX11 || \
   ! cc "$(dirname "$0")/x11-largest-window-pixel.c" \
    -o "$test_dir/x11-largest-window-pixel" -lX11; then
    echo "SKIP: X11 development libraries are required for the XWayland scene test"
    exit 77
fi
if ! cc "$(dirname "$0")/nested-pointer-drag.c" \
    -o "$test_dir/nested-pointer-drag" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for XWayland input tests"
    exit 77
fi
if ! cc "$(dirname "$0")/xwayland-activation-client.c" \
    -o "$test_dir/xwayland-activation-client" -lX11; then
    echo "SKIP: X11 development libraries are required for XWayland activation tests"
    exit 77
fi
activation_helper="$test_dir/xwayland-activation-launch"
cat >"$activation_helper" <<EOF
#!/usr/bin/env bash
exec '$test_dir/xwayland-activation-client' >'$test_dir/trusted-activation.log' 2>&1
EOF
chmod 700 "$activation_helper"
cat >"$test_dir/data/applications/nobox-xwayland-activation.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Nobox XWayland activation test
Exec=$activation_helper
StartupNotify=true
Categories=Utility;
EOF
export XDG_DATA_HOME="$test_dir/data"
export XDG_DATA_DIRS="$test_dir/empty-data"
gtk_toolkit=false
if pkg-config --exists gtk+-3.0 && \
   cc "$(dirname "$0")/xwayland-gtk-client.c" \
    -o "$test_dir/xwayland-gtk-client" \
    $(pkg-config --cflags --libs gtk+-3.0) && \
   cc "$(dirname "$0")/xwayland-dnd-gtk.c" \
    -o "$test_dir/xwayland-dnd-gtk" \
    $(pkg-config --cflags --libs gtk+-3.0); then
    gtk_toolkit=true
fi
qt_toolkit=false
if command -v c++ >/dev/null 2>&1 && pkg-config --exists Qt6Widgets && \
   c++ -std=c++17 -fPIC "$(dirname "$0")/xwayland-qt-client.cpp" \
    -o "$test_dir/xwayland-qt-client" \
    $(pkg-config --cflags --libs Qt6Widgets); then
    qt_toolkit=true
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
if ! grep -q XTEST <<<"$(DISPLAY="$display" xdpyinfo)"; then
    echo "SKIP: the nested X server does not provide the XTest extension"
    exit 77
fi

cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = false

[wayland]
xwayland = true

[mouse]
inherit_defaults = false
edge_resistance = 0
snap_to_windows = false

[[mouse.bindings]]
context = "client"
button = "Left"
trigger = "press"
action = { type = "focus" }

[keyboard]
inherit_defaults = false

[[keyboard.bindings]]
key = "W-a"
action = { type = "show_menu", menu = "application-test" }

[[menu.definitions]]
id = "application-test"
title = "Applications"
source = "applications"

[[applications]]
match = { class = "NoboxXWaylandActivation" }
focus = false

[[applications]]
match = { title = "nobox cross DND source" }
position = { x = 40, y = 40, force = true }

[[applications]]
match = { title = "nobox cross DND target" }
position = { x = 400, y = 20, force = true }

[[applications]]
match = { class = "NoboxCrossDndSource" }
position = { x = 40, y = 220, force = true }

[[applications]]
match = { class = "NoboxCrossDndTarget" }
position = { x = 400, y = 20, force = true }
size = { width = 260, height = 120, width_basis = "content", height_basis = "content" }

[[applications]]
match = { class = "org.nobox.shell-probe" }
position = { x = 40, y = 40, force = true }
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
managed_window=$(sed -n 's/^managed=\([^ ]*\).*/\1/p' \
    "$test_dir/x11-client.log" | head -n 1)
if [[ -z "$managed_window" ]]; then
    echo "could not discover the managed XWayland test window" >&2
    exit 1
fi
window_geometry() {
    DISPLAY="$xwayland_display" xwininfo -id "$managed_window" | awk '
        /Absolute upper-left X:/ { x=$4 }
        /Absolute upper-left Y:/ { y=$4 }
        /Width:/ { width=$2 }
        /Height:/ { height=$2 }
        END { print x, y, width, height }'
}
assert_geometry() {
    local expected=$1
    local operation=$2
    local observed=
    for _ in $(seq 1 100); do
        observed=$(window_geometry)
        if [[ "$observed" == "$expected" ]]; then return 0; fi
        sleep 0.05
    done
    echo "$operation produced '$observed', expected '$expected'" >&2
    cat "$log" >&2
    cat "$test_dir/x11-client.log" >&2
    return 1
}

read -r x y width height < <(window_geometry)
start_x=$((x + 100))
start_y=$((y + 100))
DISPLAY="$display" "$test_dir/nested-pointer-drag" motion \
    "$start_x" "$start_y" 0 0
kill -USR1 "$x11_client_pid"
for _ in $(seq 1 50); do
    if grep -Fq 'request=spoof' "$test_dir/x11-client.log"; then break; fi
    sleep 0.05
done
grep -Fq 'request=spoof' "$test_dir/x11-client.log"
DISPLAY="$display" "$test_dir/nested-pointer-drag" motion \
    "$start_x" "$start_y" 30 0
assert_geometry "$x $y $width $height" 'ungrabbed spoofed move request'

DISPLAY="$display" "$test_dir/nested-pointer-drag" resize \
    "$start_x" "$start_y" -40 0
width=$((width - 40))
assert_geometry "$x $y $width $height" 'authenticated pointer resize'
grep -Fq 'request=resize' "$test_dir/x11-client.log"

start_x=$((x + 50))
start_y=$((y + 100))
DISPLAY="$display" "$test_dir/nested-pointer-drag" move \
    "$start_x" "$start_y" -40 0
x=$((x - 40))
assert_geometry "$x $y $width $height" 'authenticated pointer move'
grep -Fq 'request=move' "$test_dir/x11-client.log"

DISPLAY="$xwayland_display" "$test_dir/xwayland-group-client" \
    >"$test_dir/x11-group-client.log" 2>&1 &
x11_group_client_pid=$!
group_main=
group_helper=
group_ordinary=
for _ in $(seq 1 100); do
    group_line=$(sed -n \
        's/^main=\([^ ]*\) helper=\([^ ]*\) ordinary=\([^ ]*\)$/\1 \2 \3/p' \
        "$test_dir/x11-group-client.log" 2>/dev/null | head -n 1)
    if [[ -n "$group_line" ]]; then
        read -r group_main group_helper group_ordinary <<<"$group_line"
        break
    fi
    if ! kill -0 "$x11_group_client_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if [[ -z "$group_ordinary" ]]; then
    echo "XWayland group fixture did not map" >&2
    cat "$test_dir/x11-group-client.log" >&2
    exit 1
fi
stacking_order() {
    DISPLAY="$xwayland_display" xprop -root _NET_CLIENT_LIST_STACKING |
        sed -n 's/.*# //p' | tr -d ' ' | tr '[:upper:]' '[:lower:]'
}
group_transient_is_above_peer() {
    local order main_index helper_index
    order=$(stacking_order | tr ',' '\n')
    main_index=$(awk -v wanted="${group_main,,}" '$0 == wanted { print NR }' <<<"$order")
    helper_index=$(awk -v wanted="${group_helper,,}" '$0 == wanted { print NR }' <<<"$order")
    [[ -n "$main_index" && -n "$helper_index" && "$helper_index" -gt "$main_index" ]]
}
for _ in $(seq 1 100); do
    if group_transient_is_above_peer; then break; fi
    sleep 0.05
done
if ! group_transient_is_above_peer; then
    echo "XWayland group transient was not initially stacked above its group peer" >&2
    echo "observed: $(stacking_order)" >&2
    exit 1
fi
is_xwayland_client() {
    local window=${1,,}
    DISPLAY="$xwayland_display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        tr '[:upper:]' '[:lower:]' | grep -Fq "$window"
}
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
    "$probe_binary" --application-menu >"$test_dir/xwayland-application-menu"
grep -Fq 'application-menu-ok' "$test_dir/xwayland-application-menu"
for _ in $(seq 1 100); do
    if grep -Fq 'focus=activation' "$test_dir/trusted-activation.log" 2>/dev/null; then break; fi
    sleep 0.05
done
if ! grep -Eq 'token=[[:alnum:]]{32}$' "$test_dir/trusted-activation.log" 2>/dev/null || \
   ! grep -Fq 'focus=activation' "$test_dir/trusted-activation.log" 2>/dev/null; then
    echo "compositor-issued XWayland startup token did not activate its client" >&2
    cat "$test_dir/trusted-activation.log" >&2 2>/dev/null || true
    exit 1
fi
trusted_activation_pid=$(sed -n 's/^pid=\([0-9]*\).*/\1/p' \
    "$test_dir/trusted-activation.log" | head -n 1)
trusted_activation_window=$(sed -n 's/^.*window=\([^ ]*\).*/\1/p' \
    "$test_dir/trusted-activation.log" | head -n 1)
kill "$trusted_activation_pid"
wait "$trusted_activation_pid" 2>/dev/null || true
trusted_activation_pid=
for _ in $(seq 1 100); do
    if ! is_xwayland_client "$trusted_activation_window"; then break; fi
    sleep 0.05
done
kill "$x11_group_client_pid"
wait "$x11_group_client_pid" 2>/dev/null || true
x11_group_client_pid=
for _ in $(seq 1 100); do
    if ! is_xwayland_client "$group_main" && \
       ! is_xwayland_client "$group_helper" && \
       ! is_xwayland_client "$group_ordinary"; then break; fi
    sleep 0.05
done

toolkit_window_geometry() {
    local window=$1
    DISPLAY="$xwayland_display" xwininfo -id "$window" | awk '
        /Absolute upper-left X:/ { x=$4 }
        /Absolute upper-left Y:/ { y=$4 }
        /Width:/ { width=$2 }
        /Height:/ { height=$2 }
        END { print x, y, width, height }'
}
gtk_window=
qt_window=
if [[ "$gtk_toolkit" == true ]]; then
    DISPLAY="$xwayland_display" GDK_BACKEND=x11 \
        "$test_dir/xwayland-gtk-client" >"$test_dir/gtk-client.log" 2>&1 &
    gtk_client_pid=$!
    for _ in $(seq 1 100); do
        gtk_window=$(sed -n 's/^window=//p' "$test_dir/gtk-client.log" | head -n 1)
        if [[ -n "$gtk_window" ]] && is_xwayland_client "$gtk_window"; then break; fi
        if ! kill -0 "$gtk_client_pid" 2>/dev/null; then break; fi
        sleep 0.05
    done
    if [[ -z "$gtk_window" ]] || ! DISPLAY="$xwayland_display" \
        xwininfo -id "$gtk_window" >/dev/null 2>&1; then
        echo "GTK 3 X11 client did not map through XWayland" >&2
        cat "$test_dir/gtk-client.log" >&2
        exit 1
    fi
fi
if [[ "$qt_toolkit" == true ]]; then
    DISPLAY="$xwayland_display" QT_QPA_PLATFORM=xcb \
        "$test_dir/xwayland-qt-client" >"$test_dir/qt-client.log" 2>&1 &
    qt_client_pid=$!
    for _ in $(seq 1 100); do
        qt_window=$(sed -n 's/^window=//p' "$test_dir/qt-client.log" | head -n 1)
        if [[ -n "$qt_window" ]] && is_xwayland_client "$qt_window"; then break; fi
        if ! kill -0 "$qt_client_pid" 2>/dev/null; then break; fi
        sleep 0.05
    done
    if [[ -z "$qt_window" ]] || ! DISPLAY="$xwayland_display" \
        xwininfo -id "$qt_window" >/dev/null 2>&1; then
        echo "Qt 6 X11 client did not map through XWayland" >&2
        cat "$test_dir/qt-client.log" >&2
        exit 1
    fi
fi
if [[ -n "$gtk_window" && -n "$qt_window" ]]; then
    gtk_focus_count=$(grep -Fc 'focus=gtk' "$test_dir/gtk-client.log" || true)
    read -r toolkit_x toolkit_y _ _ < <(toolkit_window_geometry "$gtk_window")
    DISPLAY="$display" "$test_dir/nested-pointer-drag" click \
        "$((toolkit_x + 40))" "$((toolkit_y + 40))" 0 0
    for _ in $(seq 1 100); do
        if (( $(grep -Fc 'focus=gtk' "$test_dir/gtk-client.log" || true) \
            > gtk_focus_count )) || DISPLAY="$xwayland_display" \
            xprop -id "$gtk_window" _NET_WM_STATE 2>/dev/null | \
            grep -Fq '_NET_WM_STATE_FOCUSED'; then break; fi
        sleep 0.05
    done
    if (( $(grep -Fc 'focus=gtk' "$test_dir/gtk-client.log" || true) \
        <= gtk_focus_count )) && ! DISPLAY="$xwayland_display" \
        xprop -id "$gtk_window" _NET_WM_STATE 2>/dev/null | \
        grep -Fq '_NET_WM_STATE_FOCUSED'; then
        echo "GTK X11 client did not regain focus through core policy" >&2
        echo "GTK geometry: $(toolkit_window_geometry "$gtk_window")" >&2
        echo "Qt geometry: $(toolkit_window_geometry "$qt_window")" >&2
        DISPLAY="$xwayland_display" xprop -id "$gtk_window" _NET_WM_STATE >&2 || true
        DISPLAY="$xwayland_display" xprop -id "$qt_window" _NET_WM_STATE >&2 || true
        exit 1
    fi

    qt_focus_count=$(grep -Fc 'focus=qt' "$test_dir/qt-client.log" || true)
    read -r toolkit_x toolkit_y _ _ < <(toolkit_window_geometry "$qt_window")
    DISPLAY="$display" "$test_dir/nested-pointer-drag" click \
        "$((toolkit_x + 40))" "$((toolkit_y + 40))" 0 0
    for _ in $(seq 1 100); do
        if (( $(grep -Fc 'focus=qt' "$test_dir/qt-client.log" || true) \
            > qt_focus_count )) || DISPLAY="$xwayland_display" \
            xprop -id "$qt_window" _NET_WM_STATE 2>/dev/null | \
            grep -Fq '_NET_WM_STATE_FOCUSED'; then break; fi
        sleep 0.05
    done
    if (( $(grep -Fc 'focus=qt' "$test_dir/qt-client.log" || true) \
        <= qt_focus_count )) && ! DISPLAY="$xwayland_display" \
        xprop -id "$qt_window" _NET_WM_STATE 2>/dev/null | \
        grep -Fq '_NET_WM_STATE_FOCUSED'; then
        echo "Qt X11 client did not regain focus through core policy" >&2
        exit 1
    fi
fi
if [[ -n "$qt_client_pid" ]]; then
    kill "$qt_client_pid"
    wait "$qt_client_pid" 2>/dev/null || true
    qt_client_pid=
fi
if [[ -n "$gtk_client_pid" ]]; then
    kill "$gtk_client_pid"
    wait "$gtk_client_pid" 2>/dev/null || true
    gtk_client_pid=
fi

wait_for_log() {
    local pattern=$1
    local file=$2
    local pid=$3
    for _ in $(seq 1 120); do
        if grep -Fq "$pattern" "$file" 2>/dev/null; then return 0; fi
        if ! kill -0 "$pid" 2>/dev/null; then break; fi
        sleep 0.05
    done
    return 1
}

wait_for_xwayland_position() {
    local window=$1
    local minimum_x=$2
    local minimum_y=$3
    local observed_x=0
    local observed_y=0
    for _ in $(seq 1 120); do
        if is_xwayland_client "$window"; then
            read -r observed_x observed_y _ _ < <(toolkit_window_geometry "$window")
            if (( observed_x >= minimum_x && observed_y >= minimum_y )); then
                return 0
            fi
        fi
        sleep 0.05
    done
    echo "XWayland DND window was not configured at its deterministic position" >&2
    return 1
}

if [[ "$gtk_toolkit" == true ]]; then
    NO_AT_BRIDGE=1 XDG_DATA_DIRS=/usr/local/share:/usr/share \
        GDK_BACKEND=x11 DISPLAY="$xwayland_display" "$test_dir/xwayland-dnd-gtk" target \
        >"$test_dir/x11-dnd-target.log" 2>&1 &
    x11_dnd_pid=$!
    wait_for_log 'window=0x' "$test_dir/x11-dnd-target.log" "$x11_dnd_pid"
    x11_dnd_window=$(sed -n 's/^window=//p' "$test_dir/x11-dnd-target.log" | head -n 1)
    wait_for_xwayland_position "$x11_dnd_window" 350 15
    # The X window can be managed before XWayland associates and commits its
    # corresponding Wayland surface. Wait for that asynchronous handoff before
    # testing compositor-side pointer focus.
    sleep 0.3
    read -r dnd_x dnd_y dnd_width dnd_height < <(
        DISPLAY="$xwayland_display" xwininfo -id "$x11_dnd_window" | awk '
            /Absolute upper-left X:/ { x=$4 }
            /Absolute upper-left Y:/ { y=$4 }
            /Width:/ { width=$2 }
            /Height:/ { height=$2 }
            END { print x, y, width, height }')
    dnd_target_x=$((dnd_x + dnd_width / 2))
    dnd_target_y=$((dnd_y + dnd_height / 2))
    if (( dnd_width != 260 || dnd_height != 120 )); then
        echo "XWayland application size rule was not retained" >&2
        exit 1
    fi
    if ! DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        NOBOX_DND_SOURCE_X=120 NOBOX_DND_SOURCE_Y=90 \
        NOBOX_DND_TARGET_X="$dnd_target_x" NOBOX_DND_TARGET_Y="$dnd_target_y" \
        "$probe_binary" --dnd-xwayland-source \
        >"$test_dir/wayland-dnd-source.log" 2>&1; then
        echo "Wayland-to-XWayland DND source failed" >&2
        cat "$test_dir/wayland-dnd-source.log" >&2
        exit 1
    fi
    if ! wait_for_log 'dnd-received=nobox-cross-dnd' \
        "$test_dir/x11-dnd-target.log" "$x11_dnd_pid"; then
        echo "Wayland-to-XWayland DND did not transfer its exact payload" >&2
        cat "$test_dir/wayland-dnd-source.log" >&2
        cat "$test_dir/x11-dnd-target.log" >&2
        exit 1
    fi
    kill "$x11_dnd_pid"
    wait "$x11_dnd_pid" 2>/dev/null || true
    x11_dnd_pid=
    sleep 0.2

    NO_AT_BRIDGE=1 XDG_DATA_DIRS=/usr/local/share:/usr/share \
        GDK_BACKEND=wayland DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
        WAYLAND_DISPLAY="$socket" "$test_dir/xwayland-dnd-gtk" target \
        >"$test_dir/wayland-dnd-target.log" 2>&1 &
    wayland_dnd_pid=$!
    wait_for_log 'window=wayland' "$test_dir/wayland-dnd-target.log" "$wayland_dnd_pid"
    NO_AT_BRIDGE=1 XDG_DATA_DIRS=/usr/local/share:/usr/share \
        GDK_BACKEND=x11 DISPLAY="$xwayland_display" "$test_dir/xwayland-dnd-gtk" source \
        >"$test_dir/x11-dnd-source.log" 2>&1 &
    x11_dnd_pid=$!
    wait_for_log 'window=0x' "$test_dir/x11-dnd-source.log" "$x11_dnd_pid"
    x11_dnd_window=$(sed -n 's/^window=//p' "$test_dir/x11-dnd-source.log" | head -n 1)
    wait_for_xwayland_position "$x11_dnd_window" 30 30
    sleep 0.3
    read -r dnd_x dnd_y dnd_width dnd_height < <(
        DISPLAY="$xwayland_display" xwininfo -id "$x11_dnd_window" | awk '
            /Absolute upper-left X:/ { x=$4 }
            /Absolute upper-left Y:/ { y=$4 }
            /Width:/ { width=$2 }
            /Height:/ { height=$2 }
            END { print x, y, width, height }')
    dnd_source_x=$((dnd_x + dnd_width / 2))
    dnd_source_y=$((dnd_y + dnd_height / 2))
    DISPLAY="$display" "$test_dir/nested-pointer-drag" drag \
        "$dnd_source_x" "$dnd_source_y" \
        "$((490 - dnd_source_x))" "$((70 - dnd_source_y))"
    if ! wait_for_log 'dnd-received=nobox-cross-dnd' \
        "$test_dir/wayland-dnd-target.log" "$wayland_dnd_pid"; then
        echo "XWayland-to-Wayland DND did not transfer its exact payload" >&2
        cat "$test_dir/x11-dnd-source.log" >&2
        cat "$test_dir/wayland-dnd-target.log" >&2
        exit 1
    fi
    kill "$x11_dnd_pid" "$wayland_dnd_pid"
    wait "$x11_dnd_pid" 2>/dev/null || true
    wait "$wayland_dnd_pid" 2>/dev/null || true
    x11_dnd_pid=
    wayland_dnd_pid=
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
