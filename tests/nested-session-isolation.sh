#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "$0")/nested-x.sh"

if [[ ${1:-} == --probe-private-atspi ]]; then
    test_dir=${2:?private AT-SPI probe requires the test directory}
    isolate_nested_session "$test_dir" private-bus
    address=$(gdbus call --session --dest org.a11y.Bus \
        --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress)
    [[ "$address" == *"unix:path=$XDG_RUNTIME_DIR/at-spi/"* ]]
    echo "private AT-SPI activation used the isolated runtime"
    exit 0
fi

test_dir=$(mktemp -d)
cleanup() {
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

export XDG_RUNTIME_DIR=/run/user/host-session
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/host-session/bus
export AT_SPI_BUS_ADDRESS=unix:path=/run/user/host-session/at-spi/bus_0
export DBUS_STARTER_ADDRESS=unix:path=/run/user/host-session/starter
export DBUS_STARTER_BUS_TYPE=session
export SESSION_MANAGER=local/host-session
export WAYLAND_DISPLAY=wayland-host
unset NO_AT_BRIDGE

isolate_nested_session "$test_dir"
expected_runtime="$test_dir/nested-runtime"
[[ "$XDG_RUNTIME_DIR" == "$expected_runtime" ]]
[[ "$DBUS_SESSION_BUS_ADDRESS" == "unix:path=$expected_runtime/no-session-bus" ]]
[[ "$AT_SPI_BUS_ADDRESS" == "unix:path=$expected_runtime/no-at-spi-bus" ]]
[[ "$NO_AT_BRIDGE" == 1 ]]
[[ ! -v DBUS_STARTER_ADDRESS && ! -v DBUS_STARTER_BUS_TYPE ]]
[[ ! -v SESSION_MANAGER && ! -v WAYLAND_DISPLAY ]]
[[ -d "$expected_runtime" && $(stat -c '%a' "$expected_runtime") == 700 ]]

private_bus=unix:path="$test_dir/private-session-bus"
export DBUS_SESSION_BUS_ADDRESS="$private_bus"
export AT_SPI_BUS_ADDRESS=unix:path=/run/user/host-session/at-spi/bus_0
export NO_AT_BRIDGE=1
isolate_nested_session "$test_dir" private-bus
[[ "$DBUS_SESSION_BUS_ADDRESS" == "$private_bus" ]]
[[ ! -v AT_SPI_BUS_ADDRESS && ! -v NO_AT_BRIDGE ]]

if command -v dbus-run-session >/dev/null 2>&1 && command -v gdbus >/dev/null 2>&1; then
    activation_root="$test_dir/activation"
    mkdir -m 700 -p "$activation_root/nested-runtime"
    env XDG_RUNTIME_DIR="$activation_root/nested-runtime" DISPLAY=:65000 \
        dbus-run-session -- bash "$0" --probe-private-atspi "$activation_root"
fi

echo "nested session environment isolation passed"
