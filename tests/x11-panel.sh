#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-panel.sh /path/to/nobox}
for dependency in xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the optional panel test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

write_config() {
    local position=$1
    local height=$2
    cat >"$test_dir/config.toml" <<EOF
[panel]
enabled = true
position = "$position"
height = $height
EOF
}
write_config bottom 34

display=
for number in $(seq 751 770); do
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

DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    RUST_LOG=nobox=debug "$nobox_binary" run --no-autostart \
    >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!

find_panel() {
    panel_window=
    for _ in $(seq 1 80); do
        for candidate in $(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
            grep -o '0x[0-9a-fA-F]*'); do
            if DISPLAY="$display" xprop -id "$candidate" WM_CLASS 2>/dev/null |
                grep -qi 'nobox-panel'; then
                panel_window=$candidate
                return 0
            fi
        done
        sleep 0.05
    done
    echo "optional panel did not become managed" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    return 1
}

assert_panel() {
    local y=$1
    local height=$2
    local strut=$3
    local workarea=$4
    find_panel
    local info properties observed=
    info=$(DISPLAY="$display" xwininfo -id "$panel_window")
    for expected in "Absolute upper-left X:  0" "Absolute upper-left Y:  $y" \
        "Width: 800" "Height: $height"; do
        if ! grep -q "$expected" <<<"$info"; then
            echo "panel geometry did not contain '$expected'" >&2
            echo "$info" >&2
            return 1
        fi
    done
    properties=$(DISPLAY="$display" xprop -id "$panel_window" \
        _NET_WM_WINDOW_TYPE _NET_WM_STRUT_PARTIAL)
    if ! grep -q '_NET_WM_WINDOW_TYPE_DOCK' <<<"$properties" ||
        ! grep -q "= $strut" <<<"$properties"; then
        echo "panel did not publish its dock type and strut: $properties" >&2
        return 1
    fi
    for _ in $(seq 1 50); do
        observed=$(DISPLAY="$display" xprop -root _NET_WORKAREA 2>/dev/null || true)
        if grep -q "$workarea" <<<"$observed"; then return 0; fi
        sleep 0.05
    done
    echo "panel strut was not reflected in work area: $observed" >&2
    return 1
}

assert_panel 566 34 '0, 0, 0, 34, 0, 0, 0, 0, 0, 0, 0, 799' \
    '= 0, 0, 800, 566'
write_config top 28
kill -HUP "$nobox_pid"
assert_panel 0 28 '0, 0, 28, 0, 0, 0, 0, 0, 0, 799, 0, 0' \
    '= 0, 28, 800, 572'

if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "nobox exited while reconfiguring the optional panel" >&2
    exit 1
fi
echo "optional EWMH panel lifecycle and struts passed on $display"
