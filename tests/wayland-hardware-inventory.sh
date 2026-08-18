#!/usr/bin/env bash
set -euo pipefail

recorder=${1:?usage: wayland-hardware-inventory.sh /path/to/wayland-hardware-acceptance.sh /path/to/nobox}
nobox_binary=${2:?missing nobox binary}

test_dir=$(mktemp -d)
cleanup() {
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

prepared_config="$test_dir/config.toml"
"$recorder" --prepare-config "$nobox_binary" "$prepared_config"
grep -Fqx 'xwayland = true' "$prepared_config"
if grep -Fqx 'xwayland = false' "$prepared_config"; then
    echo "hardware recorder retained the disabled XWayland default" >&2
    exit 1
fi
"$nobox_binary" --config "$prepared_config" check >/dev/null
if "$recorder" --prepare-config "$nobox_binary" "$prepared_config" >/dev/null 2>&1; then
    echo "hardware recorder replaced an existing acceptance configuration" >&2
    exit 1
fi

drm_root="$test_dir/drm"
pci_device="$test_dir/pci/0000:01:00.0"
mkdir -p "$drm_root/card7" "$drm_root/card7-DP-1" \
    "$drm_root/card7-HDMI-A-2" "$pci_device"
ln -s "$pci_device" "$drm_root/card7/device"
printf '0x1234\n' >"$pci_device/vendor"
printf '0xabcd\n' >"$pci_device/device"
printf 'connected\n' >"$drm_root/card7-DP-1/status"
printf 'disconnected\n' >"$drm_root/card7-HDMI-A-2/status"

inventory=$("$recorder" --inventory "$drm_root")
grep -Fqx -- '- GPU: card7 (0000:01:00.0 vendor=0x1234 device=0xabcd driver=unknown)' \
    <<<"$inventory"
grep -Fqx -- '- Connected connector at start: card7-DP-1' <<<"$inventory"
if grep -Fq 'HDMI-A-2' <<<"$inventory"; then
    echo "disconnected connector entered the hardware inventory" >&2
    exit 1
fi

printf 'disconnected\n' >"$drm_root/card7-DP-1/status"
if "$recorder" --inventory "$drm_root" >/dev/null 2>&1; then
    echo "hardware inventory accepted a topology without a connected output" >&2
    exit 1
fi

echo "Wayland hardware recorder fixtures passed"
