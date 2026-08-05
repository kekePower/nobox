#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: x11-settings.sh /path/to/nobox /path/to/nobox-settings /path/to/default.toml}
settings_binary=${2:?usage: x11-settings.sh /path/to/nobox /path/to/nobox-settings /path/to/default.toml}
default_config=${3:?usage: x11-settings.sh /path/to/nobox /path/to/nobox-settings /path/to/default.toml}
for dependency in xdpyinfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the graphical settings test"
        exit 77
    fi
done
source "$(dirname "$0")/nested-x.sh"
select_nested_x_server 1024 768

test_dir=$(mktemp -d)
xserver_pid=
cleanup() {
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

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
if ! DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then
    echo "nested X server did not become ready" >&2
    sed -n '1,120p' "$test_dir/xserver.log" >&2
    exit 1
fi

cp "$default_config" "$test_dir/config.toml"
DISPLAY="$display" GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 \
    "$settings_binary" --config "$test_dir/config.toml" --test-save-follow-mouse \
    >"$test_dir/settings.log" 2>&1
if ! grep -q 'settings window mapped and saved' "$test_dir/settings.log"; then
    echo "settings application did not complete its mapped save" >&2
    sed -n '1,160p' "$test_dir/settings.log" >&2
    exit 1
fi
if ! grep -A5 '^\[focus\]' "$test_dir/config.toml" | grep -q '^follow_mouse = true$'; then
    echo "friendly settings control did not update follow_mouse" >&2
    exit 1
fi
if ! grep -A4 '^\[workspaces\]' "$test_dir/config.toml" |
    grep -q '^names = \["main", "web", "chat", "media", "five", "six"\]$'; then
    echo "friendly desktop controls did not save the count and names" >&2
    exit 1
fi
if ! grep -q '^# Focus clients as the pointer enters them' "$test_dir/config.toml" ||
    ! grep -q '^\[\[keyboard.bindings\]\]' "$test_dir/config.toml"; then
    echo "settings save discarded comments or advanced bindings" >&2
    exit 1
fi
if [[ $(stat -c '%a' "$test_dir/config.toml") != 600 ]]; then
    echo "settings save did not use private file permissions" >&2
    exit 1
fi
"$nobox_binary" --config "$test_dir/config.toml" check >"$test_dir/check.log"
grep -q 'configuration is valid' "$test_dir/check.log"
