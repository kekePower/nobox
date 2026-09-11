#!/usr/bin/env bash
set -euo pipefail

wm_binary=${1:?usage: x11-workspace-visibility.sh /path/to/nobox [--openbox]}
wm_mode=${2:-nobox}
for dependency in cc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the workspace visibility test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600
test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
xserver_pid=
wm_pid=
cleanup() {
    if [[ -n "$wm_pid" ]]; then kill "$wm_pid" 2>/dev/null || true; wait "$wm_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; wait "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

cc -std=c11 -Wall -Wextra -Werror "$(dirname "$0")/workspace-visibility.c" \
    -o "$test_dir/workspace-visibility" -lX11
display=
for number in $(seq 951 970); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then display=":$number"; break; fi
done
[[ -n "$display" ]] || { echo "no unused nested X11 display found" >&2; exit 1; }
"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.1
done

if [[ "$wm_mode" == --openbox ]]; then
    cat >"$test_dir/rc.xml" <<'EOF'
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <desktops><number>3</number><firstdesk>1</firstdesk><popupTime>0</popupTime></desktops>
  <focus><focusNew>yes</focusNew><followMouse>no</followMouse><raiseOnFocus>no</raiseOnFocus></focus>
  <theme><name>Clearlooks</name><animateIconify>no</animateIconify></theme>
</openbox_config>
EOF
    DISPLAY="$display" "$wm_binary" --config-file "$test_dir/rc.xml" --sm-disable \
        >"$test_dir/wm.log" 2>&1 &
else
    cat >"$test_dir/config.toml" <<'EOF'
[workspaces]
names = ["one", "two", "three"]
[focus]
raise_on_focus = false
EOF
    DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
        "$wm_binary" run --no-autostart >"$test_dir/wm.log" 2>&1 &
fi
wm_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; then break; fi
    sleep 0.1
done
for mode in normal maximized fullscreen; do
    # Let the manager finish withdrawing the previous fixture before the
    # server reuses its client IDs. Openbox clears old desktop/state hints
    # during withdrawal, which can otherwise hit the next fixture's windows.
    for _ in $(seq 1 100); do
        clients=$(DISPLAY="$display" xprop -root _NET_CLIENT_LIST)
        if [[ "$clients" == '_NET_CLIENT_LIST(WINDOW)'* && "$clients" != *0x* ]]; then break; fi
        sleep 0.02
    done
    [[ "$clients" == '_NET_CLIENT_LIST(WINDOW)'* && "$clients" != *0x* ]]
    if ! DISPLAY="$display" "$test_dir/workspace-visibility" "$mode"; then
        tail -n 40 "$test_dir/wm.log" >&2
        exit 1
    fi
done
