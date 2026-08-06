#!/usr/bin/env bash
set -euo pipefail

usage="usage: x11-agent-browser-a11y.sh /path/to/nobox /path/to/seat-probe /path/to/browser /path/to/page"
nobox_binary=${1:?$usage}
seat_probe=${2:?$usage}
browser=${3:?$usage}
page=${4:?$usage}

for dependency in dbus-run-session gdbus timeout xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the browser semantic probe"
        exit 77
    fi
done
if [[ ! -x "$browser" ]]; then
    echo "SKIP: Firefox-family browser is unavailable: $browser"
    exit 77
fi

if [[ ${NOBOX_BROWSER_PRIVATE_BUS:-0} != 1 ]]; then
    exec dbus-run-session -- env NOBOX_BROWSER_PRIVATE_BUS=1 \
        bash "$0" "$nobox_binary" "$seat_probe" "$browser" "$page"
fi

source "$(dirname "$0")/nested-x.sh"
if [[ ${NOBOX_XSERVER:-auto} == auto ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 1280 800

test_dir=$(mktemp -d)
runtime_dir="$test_dir/run"
profile_dir="$test_dir/profile"
mkdir -p "$runtime_dir" "$profile_dir"
chmod 700 "$runtime_dir" "$profile_dir"
seat_probe_bound="$test_dir/agent-seat-probe"
cp -- "$seat_probe" "$seat_probe_bound"
cat >"$test_dir/config.toml" <<EOF
[agent]
enabled = true
policy = "deny"

[[agent.grants]]
label = "real browser semantic probe"
executable = "$seat_probe_bound"
capabilities = ["observe", "accessibility"]
EOF
cat >"$profile_dir/user.js" <<'EOF'
user_pref("accessibility.force_disabled", -1);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("zen.welcome-screen.seen", true);
EOF

xserver_pid=
nobox_pid=
browser_pid=
cleanup() {
    if [[ -n "$browser_pid" ]]; then kill "$browser_pid" 2>/dev/null || true; fi
    if [[ -n "$nobox_pid" ]]; then kill "$nobox_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    find "$test_dir" -type f -delete 2>/dev/null || true
    find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 111 125); do
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
    echo "$nested_x_server did not become ready" >&2
    exit 1
fi

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
        grep -q 'window id'; then
        break
    fi
    sleep 0.1
done
socket="$runtime_dir/nobox/agent-seat-${display#:}.sock"
if [[ ! -S "$socket" ]]; then
    echo "nobox did not publish the browser test agent seat" >&2
    exit 1
fi

gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus \
    --method org.a11y.Bus.GetAddress >/dev/null
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" NO_AT_BRIDGE=0 MOZ_ENABLE_WAYLAND=0 \
    "$browser" --no-remote --new-instance --profile "$profile_dir" "file://$page" \
    >"$test_dir/browser.log" 2>&1 &
browser_pid=$!

client=
for _ in $(seq 1 200); do
    while read -r candidate; do
        [[ -n "$candidate" ]] || continue
        pid=$(DISPLAY="$display" xprop -id "$candidate" _NET_WM_PID 2>/dev/null |
            sed -n 's/.* = //p')
        if [[ "$pid" == "$browser_pid" ]]; then
            client=$candidate
            break
        fi
    done < <(DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
        grep -oE '0x[0-9a-fA-F]+')
    [[ -n "$client" ]] && break
    sleep 0.1
done
if [[ -z "$client" ]]; then
    echo "the private browser did not map a managed window" >&2
    sed -n '1,120p' "$test_dir/browser.log" >&2
    exit 1
fi

title=
for _ in $(seq 1 100); do
    title=$(DISPLAY="$display" xprop -id "$client" _NET_WM_NAME 2>/dev/null |
        sed -n 's/^[^=]*= "\(.*\)"$/\1/p')
    [[ "$title" == *"Nobox Semantic Video Fixture"* ]] && break
    sleep 0.1
done
if [[ "$title" != *"Nobox Semantic Video Fixture"* ]]; then
    echo "the private browser did not load the local semantic fixture: $title" >&2
    exit 1
fi

if ! DISPLAY="$display" timeout 5s "$seat_probe_bound" "$socket" semantic-video \
    nobox-browser-semantic-probe "$title" >"$test_dir/semantic-video.log" 2>&1; then
    echo "the browser video was not actionable through semantics" >&2
    sed -n '1,80p' "$test_dir/semantic-video.log" >&2
    sed -n '1,160p' "$test_dir/nobox.log" >&2
    exit 1
fi
sed -n '1p' "$test_dir/semantic-video.log"
