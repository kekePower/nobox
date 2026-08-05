#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-xsmp.sh /path/to/nobox /path/to/nobox-xsmp}
xsmp_helper=${2:?usage: x11-xsmp.sh /path/to/nobox /path/to/nobox-xsmp}
for dependency in cc mkfifo pkg-config xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the XSMP bridge test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 800 600

test_dir=$(mktemp -d)
xserver_pid=
nobox_pid=
manager_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$manager_pid" ]]; then kill "$manager_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if ! pkg-config --exists sm ice; then
    echo "SKIP: libSM and libICE development metadata is required for XSMP tests"
    exit 77
fi
cc -std=c11 -Wall -Wextra -Wpedantic -Werror "$(dirname "$0")/xsmp-test-manager.c" \
    -o "$test_dir/xsmp-test-manager" $(pkg-config --cflags --libs sm ice)
if ! cc -std=c11 -Wall -Wextra -Wpedantic -Werror "$(dirname "$0")/press-key.c" \
    -o "$test_dir/press-key" -lX11 -lXtst; then
    echo "SKIP: XTest development libraries are required for SessionLogout tests"
    exit 77
fi

cat >"$test_dir/mock-xsmp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo $'CONNECTED\tnobox-test-client'
echo SAVE
IFS= read -r save_done
printf '%s\n' "$save_done" >"$XSMP_TEST_DIR/save-done"
while [[ ! -e "$XSMP_TEST_DIR/allow-die" ]]; do sleep 0.02; done
echo DIE
IFS= read -r close
printf '%s\n' "$close" >"$XSMP_TEST_DIR/close"
EOF
chmod +x "$test_dir/mock-xsmp"

display=
for number in $(seq 411 430); do
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

DISPLAY="$display" SESSION_MANAGER=local/nobox-test \
    NOBOX_XSMP_HELPER="$test_dir/mock-xsmp" XSMP_TEST_DIR="$test_dir" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" NOBOX_STATE_FILE="$test_dir/session.toml" \
    RUST_LOG=nobox=debug "$nobox_binary" --sm-client-id old-id run --no-autostart \
    >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 100); do
    if [[ -e "$test_dir/save-done" && -e "$test_dir/session.toml" ]]; then break; fi
    sleep 0.05
done
if [[ ! -e "$test_dir/save-done" || ! -e "$test_dir/session.toml" ]]; then
    echo "XSMP SaveYourself did not produce and acknowledge a live snapshot" >&2
    tail -n 100 "$test_dir/nobox.log" >&2 || true
    exit 1
fi
if [[ "$(<"$test_dir/save-done")" != $'SAVE_DONE\t1' ]]; then
    echo "XSMP save acknowledgement was not successful" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "XSMP save request stopped nobox instead of taking an in-place snapshot" >&2
    exit 1
fi
if ! grep -q 'connected to the XSMP session manager' "$test_dir/nobox.log" ||
    ! grep -q 'external session snapshot completed' "$test_dir/nobox.log"; then
    echo "XSMP connection or save completion was not diagnosed" >&2
    tail -n 100 "$test_dir/nobox.log" >&2
    exit 1
fi

touch "$test_dir/allow-die"
for _ in $(seq 1 100); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if kill -0 "$nobox_pid" 2>/dev/null; then
    echo "XSMP Die request did not stop nobox" >&2
    exit 1
fi
if ! wait "$nobox_pid"; then
    echo "XSMP Die request did not produce a successful nobox exit" >&2
    exit 1
fi
nobox_pid=
if [[ ! -e "$test_dir/close" || "$(<"$test_dir/close")" != $'CLOSE\t1' ]]; then
    echo "nobox did not permanently close its XSMP bridge after Die" >&2
    exit 1
fi
if ! grep -q 'X11 event loop stopped cleanly' "$test_dir/nobox.log"; then
    echo "XSMP Die did not use the clean X11 shutdown path" >&2
    exit 1
fi

mkfifo "$test_dir/xsmp-control"
"$test_dir/xsmp-test-manager" "$test_dir/xsmp-address" \
    "$test_dir/xsmp-events" "$test_dir/xsmp-control" \
    >"$test_dir/manager.out" 2>"$test_dir/manager.err" &
manager_pid=$!
for _ in $(seq 1 100); do
    if [[ -s "$test_dir/xsmp-address" ]]; then break; fi
    sleep 0.02
done
if [[ ! -s "$test_dir/xsmp-address" ]]; then
    echo "test XSMP manager did not publish its ICE address" >&2
    cat "$test_dir/manager.err" >&2 || true
    exit 1
fi
session_address=$(<"$test_dir/xsmp-address")
cat >"$test_dir/config.toml" <<'EOF'
[keyboard]
inherit_defaults = false

[[keyboard.bindings]]
key = "W-F10"
action = { type = "session_logout" }

[[keyboard.bindings]]
key = "W-F11"
action = { type = "session_logout", prompt = false }
EOF
printf 'invalid pending snapshot\n' >"$test_dir/session.toml"
DISPLAY="$display" SESSION_MANAGER="$session_address" \
    NOBOX_XSMP_HELPER="$xsmp_helper" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    NOBOX_STATE_FILE="$test_dir/session.toml" RUST_LOG=nobox=debug \
    "$nobox_binary" --sm-client-id nobox-reconnect-test run --no-autostart \
    >"$test_dir/native-nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 100); do
    if grep -q $'REGISTERED\tnobox-reconnect-test' "$test_dir/xsmp-events" 2>/dev/null &&
        grep -q $'RESTART_ID\tnobox-reconnect-test' "$test_dir/xsmp-events" 2>/dev/null; then
        break
    fi
    sleep 0.05
done
if ! grep -q $'REGISTERED\tnobox-reconnect-test' "$test_dir/xsmp-events" 2>/dev/null; then
    echo "native XSMP helper did not reconnect with the requested identity" >&2
    cat "$test_dir/manager.err" >&2 || true
    tail -n 100 "$test_dir/native-nobox.log" >&2 || true
    exit 1
fi
for property in Program ProcessID UserID RestartStyleHint _GSM_Priority CloneCommand RestartCommand; do
    if ! grep -q $'PROPERTY\t'"$property" "$test_dir/xsmp-events"; then
        echo "native XSMP helper did not publish $property" >&2
        cat "$test_dir/xsmp-events" >&2
        exit 1
    fi
done
if ! grep -q $'RESTART_ID\tnobox-reconnect-test' "$test_dir/xsmp-events"; then
    echo "native XSMP restart command did not preserve the registered identity" >&2
    cat "$test_dir/xsmp-events" >&2
    exit 1
fi

printf 'SAVE\n' >"$test_dir/xsmp-control"
for _ in $(seq 1 100); do
    if grep -q '^SAVE_DONE$' "$test_dir/xsmp-events" &&
        grep -q '^version = 1$' "$test_dir/session.toml" 2>/dev/null; then
        break
    fi
    sleep 0.05
done
if ! grep -q '^SAVE_DONE$' "$test_dir/xsmp-events" ||
    ! grep -q '^version = 1$' "$test_dir/session.toml"; then
    echo "real XSMP SaveYourself was not acknowledged after durable state" >&2
    cat "$test_dir/xsmp-events" >&2
    tail -n 100 "$test_dir/native-nobox.log" >&2 || true
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "real XSMP SaveYourself stopped nobox" >&2
    exit 1
fi
printf 'COMPLETE\n' >"$test_dir/xsmp-control"
press() {
    DISPLAY="$display" "$test_dir/press-key" "$@"
}

press F10
menu_window=
for _ in $(seq 1 100); do
    menu_window=$(DISPLAY="$display" xwininfo -root -tree 2>/dev/null |
        awk '/"nobox:menu"/ { print $1; exit }')
    if [[ -n "$menu_window" ]] &&
        DISPLAY="$display" xprop -id "$menu_window" _NOBOX_MENU 2>/dev/null |
            grep -q '__nobox_session_logout'; then
        break
    fi
    sleep 0.05
done
if [[ -z "$menu_window" ]] ||
    ! DISPLAY="$display" xprop -id "$menu_window" _NOBOX_MENU 2>/dev/null |
        grep -q '__nobox_session_logout'; then
    echo "SessionLogout did not show the grabbed confirmation menu" >&2
    exit 1
fi
if grep -q '^LOGOUT_REQUEST' "$test_dir/xsmp-events"; then
    echo "SessionLogout contacted the session manager before confirmation" >&2
    exit 1
fi
press --plain Return
for _ in $(seq 1 100); do
    if ! DISPLAY="$display" xprop -id "$menu_window" _NOBOX_MENU 2>/dev/null |
        grep -q '__nobox_session_logout'; then
        break
    fi
    sleep 0.05
done
if grep -q '^LOGOUT_REQUEST' "$test_dir/xsmp-events" ||
    ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "the default SessionLogout cancellation did not preserve the session" >&2
    exit 1
fi

press F10
for _ in $(seq 1 100); do
    if DISPLAY="$display" xprop -id "$menu_window" _NOBOX_MENU 2>/dev/null |
        grep -q '__nobox_session_logout'; then
        break
    fi
    sleep 0.05
done
if ! DISPLAY="$display" xprop -id "$menu_window" _NOBOX_MENU 2>/dev/null |
    grep -q '__nobox_session_logout'; then
    echo "SessionLogout confirmation could not be reopened after cancellation" >&2
    tail -n 100 "$test_dir/native-nobox.log" >&2 || true
    exit 1
fi
press --plain Down
press --plain Return
for _ in $(seq 1 100); do
    if grep -q $'^LOGOUT_REQUEST\tinteractive$' "$test_dir/xsmp-events"; then break; fi
    sleep 0.05
done
if ! grep -q $'^LOGOUT_REQUEST\tinteractive$' "$test_dir/xsmp-events"; then
    echo "confirmed SessionLogout did not request an interactive global logout" >&2
    cat "$test_dir/xsmp-events" >&2
    exit 1
fi
if ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "SessionLogout exited before the session manager sent Die" >&2
    exit 1
fi
printf 'CANCEL\n' >"$test_dir/xsmp-control"
for _ in $(seq 1 100); do
    if grep -q 'XSMP shutdown was cancelled' "$test_dir/native-nobox.log"; then break; fi
    sleep 0.05
done
if ! grep -q 'XSMP shutdown was cancelled' "$test_dir/native-nobox.log" ||
    ! kill -0 "$nobox_pid" 2>/dev/null; then
    echo "cancelled SessionLogout did not leave nobox running" >&2
    exit 1
fi

press F11
for _ in $(seq 1 100); do
    logout_requests=$(grep -c $'^LOGOUT_REQUEST\tinteractive$' \
        "$test_dir/xsmp-events" || true)
    if [[ "$logout_requests" -ge 2 ]]; then break; fi
    sleep 0.05
done
if [[ "$logout_requests" -lt 2 ]]; then
    echo "prompt-free SessionLogout did not directly request logout" >&2
    cat "$test_dir/xsmp-events" >&2
    exit 1
fi
printf 'DIE\n' >"$test_dir/xsmp-control"
for _ in $(seq 1 100); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if kill -0 "$nobox_pid" 2>/dev/null || ! wait "$nobox_pid"; then
    echo "real XSMP Die did not cleanly stop nobox" >&2
    exit 1
fi
nobox_pid=
for _ in $(seq 1 100); do
    if grep -q '^CLOSED$' "$test_dir/xsmp-events"; then break; fi
    sleep 0.02
done
if ! grep -q '^CLOSED$' "$test_dir/xsmp-events" ||
    grep -q '^ICE_IO_ERROR$' "$test_dir/xsmp-events"; then
    echo "native XSMP connection did not close cleanly" >&2
    cat "$test_dir/xsmp-events" >&2
    exit 1
fi
printf 'QUIT\n' >"$test_dir/xsmp-control"
if ! wait "$manager_pid"; then
    echo "test XSMP manager did not exit cleanly" >&2
    cat "$test_dir/manager.err" >&2 || true
    exit 1
fi
manager_pid=

unset SESSION_MANAGER
DISPLAY="$display" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    NOBOX_STATE_FILE="$test_dir/session.toml" RUST_LOG=nobox=debug \
    "$nobox_binary" run --no-autostart >"$test_dir/fallback-nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 100); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1; then break; fi
    sleep 0.05
done
press F11
for _ in $(seq 1 100); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.05
done
if kill -0 "$nobox_pid" 2>/dev/null || ! wait "$nobox_pid"; then
    echo "SessionLogout without XSMP did not fall back to a clean local exit" >&2
    exit 1
fi
nobox_pid=
if ! grep -q 'no external session manager accepted logout' \
    "$test_dir/fallback-nobox.log"; then
    echo "SessionLogout fallback was not diagnosed" >&2
    exit 1
fi

if "$xsmp_helper" -- /bin/true >"$test_dir/native.out" 2>"$test_dir/native.err"; then
    echo "native XSMP helper connected without SESSION_MANAGER" >&2
    exit 1
fi
if ! grep -q 'could not connect' "$test_dir/native.err"; then
    echo "native XSMP helper did not diagnose a missing session manager" >&2
    cat "$test_dir/native.err" >&2
    exit 1
fi

echo "XSMP save, interactive logout, cancellation, fallback, Die, and close passed on $display"
