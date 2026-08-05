#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-margins.sh /path/to/nobox}
for dependency in xdpyinfo xprop xterm xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the X11 margin test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
xterm_pid=
cleanup() {
    if [[ -n "$xterm_pid" ]]; then kill "$xterm_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 171 190); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
if [[ -z "$display" ]]; then
    echo "no unused nested X11 display found" >&2
    exit 1
fi

write_config() {
    local top=$1 right=$2 bottom=$3 left=$4
    cat >"$test_dir/config.toml" <<EOF
[margins]
top = $top
right = $right
bottom = $bottom
left = $left

[workspaces]
names = ["one", "two", "three"]
initial = 3

[[applications]]
match = { class = "XTerm" }
maximized = true
focus = false
EOF
}

write_config 10 20 30 40
"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 40); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then break; fi
    sleep 0.05
done

if ! DISPLAY="$display" xprop -root _NET_CURRENT_DESKTOP | grep -q '= 2'; then
    echo "configured initial workspace was not selected" >&2
    exit 1
fi
workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
if [[ $(grep -o '40, 10, 740, 560' <<<"$workarea" | wc -l) -ne 3 ]]; then
    echo "configured margins were not published for every workspace" >&2
    echo "$workarea" >&2
    exit 1
fi

DISPLAY="$display" xterm -title nobox-margin-client >"$test_dir/xterm.log" 2>&1 &
xterm_pid=$!
client_window=
for _ in $(seq 1 40); do
    for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
        grep -o '0x[0-9a-fA-F]*'); do
        if DISPLAY="$display" xprop -id "$candidate" WM_NAME 2>/dev/null |
            grep -q 'nobox-margin-client'; then
            client_window=$candidate
        fi
    done
    if [[ -n "$client_window" ]]; then break; fi
    sleep 0.05
done
if [[ -z "$client_window" ]]; then
    echo "margin client did not map" >&2
    exit 1
fi
frame_window=$(DISPLAY="$display" xwininfo -id "$client_window" -tree |
    sed -n 's/.*Parent window id: \(0x[0-9a-fA-F]*\).*/\1/p')

wait_for_frame_geometry() {
    local x=$1 y=$2 width=$3 height=$4
    for _ in $(seq 1 40); do
        local info
        info=$(DISPLAY="$display" xwininfo -id "$frame_window")
        if grep -Fq "Absolute upper-left X:  $x" <<<"$info" &&
            grep -Fq "Absolute upper-left Y:  $y" <<<"$info" &&
            grep -Fq "Width: $width" <<<"$info" &&
            grep -Fq "Height: $height" <<<"$info"; then
            return 0
        fi
        sleep 0.05
    done
    DISPLAY="$display" xwininfo -id "$frame_window" >&2
    return 1
}

wait_for_frame_geometry 40 10 740 560
write_config 15 25 35 45
kill -HUP "$nobox_pid"
for _ in $(seq 1 40); do
    workarea=$(DISPLAY="$display" xprop -root _NET_WORKAREA)
    if [[ $(grep -o '45, 15, 730, 550' <<<"$workarea" | wc -l) -eq 3 ]]; then break; fi
    sleep 0.05
done
if [[ $(grep -o '45, 15, 730, 550' <<<"$workarea" | wc -l) -ne 3 ]]; then
    echo "reloaded margins were not published" >&2
    echo "$workarea" >&2
    exit 1
fi
wait_for_frame_geometry 45 15 730 550

echo "X11 initial workspace and live reserved margins passed on $display"
