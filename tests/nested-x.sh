#!/usr/bin/env bash

# Select a nested X server for an integration test. NOBOX_XSERVER may be
# xnest, xephyr, xvfb, or auto; auto preserves the established fallback order.
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
