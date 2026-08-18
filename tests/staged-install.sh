#!/usr/bin/env bash
set -euo pipefail

usage="usage: staged-install.sh CMAKE BUILD_DIR SOURCE_DIR WAYLAND_ENABLED WAYLAND_PROBE"
cmake_binary=${1:?$usage}
build_dir=${2:?$usage}
source_dir=${3:?$usage}
wayland_enabled=${4:?$usage}
wayland_probe=${5:-}

test_dir=$(mktemp -d)
cleanup() {
    find "$test_dir" -type f -delete 2>/dev/null || true
    find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
}
trap cleanup EXIT INT TERM

prefix="$test_dir/prefix"
"$cmake_binary" --install "$build_dir" --prefix "$prefix" \
    >"$test_dir/install.log"

test -x "$prefix/bin/nobox"
test -f "$prefix/share/xsessions/nobox.desktop"
test -f "$prefix/share/doc/nobox/examples/config.toml"
test -f "$prefix/share/doc/nobox/architecture.md"
test -f "$prefix/share/doc/nobox/wayland-roadmap.md"

NOBOX_XSERVER="${NOBOX_XSERVER:-}" \
    bash "$source_dir/tests/x11-smoke.sh" "$prefix/bin/nobox"

if [[ ${wayland_enabled,,} == on || ${wayland_enabled} == 1 || \
      ${wayland_enabled,,} == true ]]; then
    test -x "$prefix/bin/nobox-wayland"
    test -f "$prefix/share/wayland-sessions/nobox-wayland.desktop"
    grep -Fxq 'Exec=nobox --backend wayland run --tty' \
        "$prefix/share/wayland-sessions/nobox-wayland.desktop"
    test -x "$prefix/libexec/nobox/nobox-lightdm-session-wrapper"
    test -x "$prefix/libexec/nobox/nobox-lightdm-session-setup"
    test -x "$prefix/libexec/nobox/nobox-lightdm-session-cleanup"
    test -f "$prefix/share/doc/nobox/lightdm/90-nobox-wayland.conf.example"
    bash "$source_dir/tests/lightdm-session-integration.sh" "$source_dir"
    [[ -x "$wayland_probe" ]]
    NOBOX_XSERVER="${NOBOX_XSERVER:-}" \
        bash "$source_dir/tests/wayland-managed-shell.sh" \
        "$prefix/bin/nobox" "$prefix/bin/nobox-wayland" "$wayland_probe"
fi

echo "staged install layout and nested backend smoke tests passed"
