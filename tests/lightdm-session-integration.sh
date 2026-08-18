#!/usr/bin/env bash
set -euo pipefail

usage="usage: lightdm-session-integration.sh SOURCE_DIR"
source_dir=${1:?$usage}
helpers="$source_dir/data/lightdm"

for helper in session-wrapper session-setup session-cleanup; do
    test -x "$helpers/nobox-lightdm-$helper"
done

output=$(
    env -u DISPLAY XDG_SESSION_TYPE=wayland \
        "$helpers/nobox-lightdm-session-wrapper" \
        'printf "%s" "native wayland session"'
)
test "$output" = "native wayland session"

env -u DISPLAY XDG_SESSION_TYPE=wayland \
    "$helpers/nobox-lightdm-session-setup"
env -u DISPLAY XDG_SESSION_TYPE=wayland \
    "$helpers/nobox-lightdm-session-cleanup"

if env -u DISPLAY XDG_SESSION_TYPE=wayland \
    "$helpers/nobox-lightdm-session-wrapper" one two \
    > /dev/null 2>&1; then
    echo "LightDM wrapper accepted an invalid argument count" >&2
    exit 1
fi

grep -Fq 'sessions-directory=/usr/share/xsessions:/usr/share/wayland-sessions' \
    "$helpers/90-nobox-wayland.conf.example"

echo "LightDM native Wayland compatibility helpers passed"
