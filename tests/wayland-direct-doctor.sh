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
grep -Fq '[info] selection protocols: wl_data_device_manager v3; zwp_primary_selection_device_manager_v1 v1' \
    "$test_dir/doctor.log"
grep -Fq '[info] selection limits per client: 64 sources; 16 devices; 32 MIME types/source; 256 bytes/MIME type' \
    "$test_dir/doctor.log"
grep -Fq '[info] pointer protocols: zwp_relative_pointer_manager_v1; zwp_pointer_constraints_v1 v1; zwp_pointer_gestures_v1 v3; wp_cursor_shape_manager_v1 v2; 64 extension objects/client; 64 gesture objects/client; 64 cursor-shape devices/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] touch protocol: wl_touch via wl_seat v9; 16 touch devices/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] tablet protocol: zwp_tablet_manager_v2 v1; 16 tablet seats/client; 16 tablets/seat; 64 tools/seat; 16 pads/seat; 16/16/16 groups/rings/strips per pad; deterministic removal' \
    "$test_dir/doctor.log"
grep -Fq '[info] timing protocol: wp_presentation v2; 256 feedbacks/client' \
    "$test_dir/doctor.log"
grep -Fq '[info] inhibition and idle protocols: zwp_keyboard_shortcuts_inhibit_manager_v1 v1 (64 inhibitors/client); zwp_idle_inhibit_manager_v1 v1 (64 inhibitors/client); ext_idle_notifier_v1 v2 (64 notifications/client)' \
    "$test_dir/doctor.log"
grep -Fq 'backend capabilities: nested-x11=true, direct=false, session-restore=true' \
    "$test_dir/doctor.log"
grep -Fq 'ready: yes (direct-session prerequisites; hardware acceptance pending)' \
    "$test_dir/doctor.log"
