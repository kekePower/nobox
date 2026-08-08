#!/usr/bin/env bash
set -euo pipefail

usage="usage: x11-agent-browser-a11y.sh /path/to/nobox /path/to/seat-probe /path/to/browser /path/to/page [firefox|chromium]"
nobox_binary=${1:?$usage}
seat_probe=${2:?$usage}
browser=${3:?$usage}
page=${4:?$usage}
family=${5:-firefox}

for dependency in dbus-run-session gdbus mktemp pkill python3 setsid timeout xdpyinfo xprop xwininfo; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the browser semantic probe"
        exit 77
    fi
done
if [[ ! -x "$browser" ]]; then
    echo "SKIP: $family browser is unavailable: $browser"
    exit 77
fi
if [[ "$family" != firefox && "$family" != chromium ]]; then
    echo "$usage" >&2
    exit 2
fi

if [[ ${NOBOX_BROWSER_PRIVATE_BUS:-0} != 1 ]]; then
    session_log=$(mktemp)
    if dbus-run-session -- env NOBOX_BROWSER_PRIVATE_BUS=1 \
        bash "$0" "$nobox_binary" "$seat_probe" "$browser" "$page" "$family" \
        >"$session_log" 2>&1; then
        status=0
    else
        status=$?
    fi
    sed -n '1,400p' "$session_log"
    rm -f -- "$session_log"
    exit "$status"
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
[[applications]]
match = { kind = "normal" }
maximized = "none"
position = { x = 80, y = 60, output = "primary", force = true }
size = { width = 1100, height = 600, width_basis = "content", height_basis = "content" }

[agent]
enabled = true
policy = "deny"

[[agent.grants]]
label = "real browser semantic probe"
executable = "$seat_probe_bound"
capabilities = ["observe", "accessibility", "capture", "manage"]
EOF
cat >"$profile_dir/user.js" <<'EOF'
user_pref("accessibility.force_disabled", -1);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("layout.css.devPixelsPerPx", "1.5");
user_pref("toolkit.telemetry.enabled", false);
user_pref("zen.welcome-screen.seen", true);
EOF

xserver_pid=
nobox_pid=
browser_pid=
cleanup() {
    if [[ "$family" == chromium ]]; then
        pkill -KILL -f -- "--user-data-dir=$profile_dir" 2>/dev/null || true
    fi
    if [[ -n "$browser_pid" ]]; then kill -KILL -- "-$browser_pid" 2>/dev/null || true; fi
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
if [[ "$family" == firefox ]]; then
    browser_arguments=(--no-remote --new-instance --profile "$profile_dir" "file://$page")
else
    browser_arguments=(
        --user-data-dir="$profile_dir"
        --no-first-run
        --no-default-browser-check
        --disable-gpu
        --disable-dev-shm-usage
        --force-renderer-accessibility
        --new-window
        "file://$page"
    )
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" NO_AT_BRIDGE=0 MOZ_ENABLE_WAYLAND=0 \
    setsid "$browser" "${browser_arguments[@]}" >"$test_dir/browser.log" 2>&1 &
browser_pid=$!

client=
for _ in $(seq 1 200); do
    while read -r candidate; do
        [[ -n "$candidate" ]] || continue
        candidate_title=$(DISPLAY="$display" xprop -id "$candidate" _NET_WM_NAME 2>/dev/null |
            sed -n 's/^[^=]*= "\(.*\)"$/\1/p')
        if [[ "$candidate_title" == *"Nobox Semantic Video Fixture"* ]]; then
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

measurements="$test_dir/semantic-video.jsonl"
if [[ "$family" == firefox ]]; then
    for geometry in "80 40 1100 600" "420 40 760 600" "80 40 1100 600"; do
        read -r x y width height <<<"$geometry"
        if ! DISPLAY="$display" timeout 5s "$seat_probe_bound" "$socket" move-resize \
            nobox-browser-semantic-probe "$title" "$x" "$y" "$width" "$height" \
            >"$test_dir/move-resize.log" 2>&1; then
            echo "the browser fixture could not be resized through the seat" >&2
            sed -n '1,80p' "$test_dir/move-resize.log" >&2
            exit 1
        fi
        if ! DISPLAY="$display" timeout 10s "$seat_probe_bound" "$socket" semantic-video \
            nobox-browser-semantic-probe "$title" >>"$measurements" 2>&1; then
            echo "the browser video was not actionable through semantics" >&2
            tail -80 "$measurements" >&2
            DISPLAY="$display" xwininfo -id "$client" >&2 || true
            sed -n '1,160p' "$test_dir/nobox.log" >&2
            exit 1
        fi
    done
else
    for _ in 1 2 3; do
        if ! DISPLAY="$display" timeout 5s "$seat_probe_bound" "$socket" semantic-fallback \
            nobox-browser-semantic-probe "$title" >>"$measurements" 2>&1; then
            echo "the Chromium fallback was not safe and grounded" >&2
            tail -80 "$measurements" >&2
            sed -n '1,160p' "$test_dir/nobox.log" >&2
            exit 1
        fi
    done
fi

python3 - "$measurements" "$family" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    rows = [json.loads(line) for line in source]
assert len(rows) == 3, rows
if "bounds" in rows[0]:
    widths = [row["bounds"]["width"] for row in rows]
    assert widths[1] < widths[0], widths
    assert abs(widths[2] - widths[0]) <= 4, widths
    for row in rows:
        assert row["semantic"]["calls"] == 2, row
        assert row["fallback"]["semantic"]["calls"] == 1, row
        assert row["fallback"]["capture"]["png_bytes"] > 0, row
else:
    widths = []
    assert all(row["semantic"]["status"] == "unavailable" for row in rows), rows
    assert all(row["semantic"]["calls"] == 1 for row in rows), rows
for row in rows:
    assert row["capture"]["calls"] == 1, row
    assert row["semantic"]["json_bytes"] < row["capture"]["json_bytes"], row
    assert row["capture"]["png_bytes"] > 0, row
print(json.dumps({"family": sys.argv[2], "runs": len(rows),
                  "video_widths": widths, "measurements": rows},
                 separators=(",", ":")))
PY
