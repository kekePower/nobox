#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: wayland-direct-doctor.sh /path/to/nobox}

card=
for candidate in /dev/dri/card*; do
    if [[ -c "$candidate" && -r "$candidate" && -w "$candidate" ]]; then
        card=$candidate
        break
    fi
done
render=
for candidate in /dev/dri/renderD*; do
    if [[ -c "$candidate" && -r "$candidate" && -w "$candidate" ]]; then
        render=$candidate
        break
    fi
done
if [[ -z "$card" || -z "$render" ]] || ! compgen -G '/dev/input/event*' >/dev/null; then
    echo "SKIP: accessible DRM card/render nodes and input discovery are required"
    exit 77
fi

test_dir=$(mktemp -d)
runtime_dir="$test_dir/runtime"
mkdir -m 700 "$runtime_dir"
cleanup() {
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

XDG_RUNTIME_DIR="$runtime_dir" XDG_CONFIG_HOME="$test_dir/config" \
    "$nobox_binary" --backend wayland doctor >"$test_dir/doctor.log"
grep -Fq '[ok] Wayland backend: Smithay 0.7.0 (direct-session prerequisites)' \
    "$test_dir/doctor.log"
grep -Fq "[ok] DRM card: $card" "$test_dir/doctor.log"
grep -Fq "[ok] DRM render node: $render" "$test_dir/doctor.log"
grep -Eq '^\[ok\] libinput event nodes discovered: [1-9][0-9]*$' "$test_dir/doctor.log"
grep -Fq '[info] direct protocols: zwp_linux_dmabuf_v1 v5; wp_linux_drm_syncobj_manager_v1 v1 when syncobj-eventfd is supported' \
    "$test_dir/doctor.log"
grep -Fq '[info] surface protocols: wp_viewporter v1; wp_fractional_scale_manager_v1 v1' \
    "$test_dir/doctor.log"
grep -Fq 'backend capabilities: nested-x11=true, direct=false, session-restore=true' \
    "$test_dir/doctor.log"
grep -Fq 'ready: yes (direct-session prerequisites; hardware acceptance pending)' \
    "$test_dir/doctor.log"
