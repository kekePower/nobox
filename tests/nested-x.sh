#!/usr/bin/env bash

# Select a nested X server for an integration test. NOBOX_XSERVER may be
# xnest, xephyr, xvfb, or auto; auto preserves the established fallback order.

# Keep nested clients away from the login session's runtime and IPC services.
# In particular, a private D-Bus with the real XDG_RUNTIME_DIR is not private
# enough: its AT-SPI launcher can replace the live desktop's at-spi/bus_* socket.
isolate_nested_session() {
    local test_root=${1:?isolate_nested_session requires the test directory}
    local bus_mode=${2:-blocked}
    local isolated_runtime="$test_root/nested-runtime"

    mkdir -p -- "$isolated_runtime"
    chmod 700 "$isolated_runtime"
    export XDG_RUNTIME_DIR="$isolated_runtime"
    unset DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE SESSION_MANAGER WAYLAND_DISPLAY

    case "$bus_mode" in
        blocked)
            export DBUS_SESSION_BUS_ADDRESS="unix:path=$isolated_runtime/no-session-bus"
            export AT_SPI_BUS_ADDRESS="unix:path=$isolated_runtime/no-at-spi-bus"
            export NO_AT_BRIDGE=1
            ;;
        private-bus)
            if [[ -z ${DBUS_SESSION_BUS_ADDRESS:-} ]]; then
                echo "private nested session bus is unavailable" >&2
                return 1
            fi
            unset AT_SPI_BUS_ADDRESS NO_AT_BRIDGE
            ;;
        *)
            echo "nested session bus mode must be blocked or private-bus: $bus_mode" >&2
            return 2
            ;;
    esac
}

select_nested_x_server() {
    local width=${1:-800}
    local height=${2:-600}
    local requested=${NOBOX_XSERVER:-auto}
    local candidate=

    case "${requested,,}" in
        auto)
            for candidate in Xnest Xephyr Xvfb; do
                if command -v "$candidate" >/dev/null 2>&1; then
                    break
                fi
                candidate=
            done
            ;;
        xnest) candidate=Xnest ;;
        xephyr) candidate=Xephyr ;;
        xvfb) candidate=Xvfb ;;
        *)
            echo "NOBOX_XSERVER must be auto, xnest, xephyr, or xvfb: $requested" >&2
            return 2
            ;;
    esac

    if [[ -z "$candidate" ]] || ! command -v "$candidate" >/dev/null 2>&1; then
        echo "SKIP: requested nested X server is unavailable: $requested"
        return 77
    fi

    nested_x_server=$candidate
    case "$candidate" in
        Xnest)
            x_server=(Xnest)
            x_server_args=(-geometry "${width}x${height}" -depth 24 -ac)
            ;;
        Xephyr)
            x_server=(Xephyr)
            x_server_args=(-screen "${width}x${height}x24" -ac)
            ;;
        Xvfb)
            x_server=(Xvfb)
            x_server_args=(-screen 0 "${width}x${height}x24" -ac)
            ;;
    esac
}
