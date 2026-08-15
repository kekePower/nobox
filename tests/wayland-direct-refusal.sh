#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-direct-refusal.sh /path/to/nobox}

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
cleanup() {
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

if XDG_RUNTIME_DIR="$runtime_dir" XDG_CONFIG_HOME="$test_dir/config" \
    LIBSEAT_BACKEND=nobox-invalid \
    "$nobox_binary" --backend wayland run --tty --no-autostart \
    >"$test_dir/stdout.log" 2>"$test_dir/stderr.log"; then
    echo "direct Wayland unexpectedly accepted an unavailable libseat backend" >&2
    exit 1
fi

grep -Fq 'direct Wayland event loop stopped' "$test_dir/stderr.log"
grep -Fq 'libseat failed' "$test_dir/stderr.log"
