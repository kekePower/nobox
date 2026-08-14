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
nobox_pid=
cleanup() {
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
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
cat >>"$test_dir/config.toml" <<'CONFIG'

[agent.launch]
policy = "allow_listed"
allow = []
deny = []
user_entries = false
CONFIG
mkdir -p "$test_dir/data/applications" "$test_dir/system-data/applications"
cat >"$test_dir/system-data/applications/nobox-settings-selected.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=Selected system application
Exec=true
Categories=Utility;
ENTRY
cat >"$test_dir/data/applications/nobox-settings-user.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=Selected user application
Exec=true
Categories=Development;
ENTRY
cat >"$test_dir/system-data/applications/nobox-settings-hidden.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=Hidden application
Exec=true
NoDisplay=true
ENTRY
DISPLAY="$display" RUST_LOG=nobox_x11=debug \
    XDG_DATA_HOME="$test_dir/data" XDG_DATA_DIRS="$test_dir/system-data" \
    "$nobox_binary" --config "$test_dir/config.toml" run --no-autostart \
    >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then
        echo "nobox exited while the settings test was starting" >&2
        sed -n '1,160p' "$test_dir/nobox.log" >&2
        exit 1
    fi
    if grep -q 'loaded X11 key bindings' "$test_dir/nobox.log"; then break; fi
    sleep 0.1
done
if ! grep -q 'loaded X11 key bindings' "$test_dir/nobox.log"; then
    echo "nobox did not become ready for the settings test" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi
reload_count=$(grep -c 'reloaded configuration in place' "$test_dir/nobox.log" || true)
DISPLAY="$display" GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 \
    XDG_DATA_HOME="$test_dir/data" XDG_DATA_DIRS="$test_dir/system-data" \
    "$settings_binary" --config "$test_dir/config.toml" --test-save-follow-mouse \
    >"$test_dir/settings.log" 2>&1
if ! grep -q 'settings window mapped and saved' "$test_dir/settings.log"; then
    echo "settings application did not complete its mapped save" >&2
    sed -n '1,160p' "$test_dir/settings.log" >&2
    exit 1
fi
for _ in $(seq 1 50); do
    current_reload_count=$(
        grep -c 'reloaded configuration in place' "$test_dir/nobox.log" || true
    )
    if (( current_reload_count > reload_count )); then break; fi
    sleep 0.1
done
if (( current_reload_count <= reload_count )); then
    echo "settings save did not ask the running nobox session to reload" >&2
    sed -n '1,160p' "$test_dir/settings.log" >&2
    sed -n '1,200p' "$test_dir/nobox.log" >&2
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
if ! grep -A4 '^\[agent.launch\]' "$test_dir/config.toml" |
    grep -q '^policy = "allow_listed"$' ||
    ! grep -A4 '^\[agent.launch\]' "$test_dir/config.toml" |
        grep -q '^allow = \["nobox-settings-selected.desktop", "nobox-settings-user.desktop"\]$' ||
    ! grep -A4 '^\[agent.launch\]' "$test_dir/config.toml" |
        grep -q '^user_entries = false$'; then
    echo "agent launch picker controls did not preserve selected system and user entries" >&2
    grep -n -A8 -B2 'agent' "$test_dir/config.toml" >&2 || true
    exit 1
fi
if ! grep -q '^# Focus clients as the pointer enters them' "$test_dir/config.toml" ||
    ! grep -q '^inherit_defaults = true$' "$test_dir/config.toml"; then
    echo "settings save discarded comments or advanced bindings" >&2
    exit 1
fi
if [[ $(stat -c '%a' "$test_dir/config.toml") != 600 ]]; then
    echo "settings save did not use private file permissions" >&2
    exit 1
fi
"$nobox_binary" --config "$test_dir/config.toml" check >"$test_dir/check.log"
grep -q 'configuration is valid' "$test_dir/check.log"
