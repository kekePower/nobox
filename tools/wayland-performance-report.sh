#!/usr/bin/env bash
set -euo pipefail

usage="usage: wayland-performance-report.sh NOBOX WAYLAND_PROBE [RUNS]"
nobox_binary=${1:?$usage}
probe_binary=${2:?$usage}
runs=${3:-5}
[[ "$runs" =~ ^[1-9][0-9]*$ ]] || { echo "RUNS must be positive" >&2; exit 2; }

for dependency in awk date xdpyinfo; do
    command -v "$dependency" >/dev/null 2>&1 || {
        echo "SKIP: $dependency is required for Wayland profiling"
        exit 77
    }
done

source "$(dirname "$0")/../tests/nested-x.sh"
if [[ -z ${NOBOX_XSERVER:-} ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 1280 720

test_dir=$(mktemp -d)
xserver_pid=
compositor_pid=
profile_pid=
cleanup() {
    if [[ -n "$profile_pid" ]]; then kill "$profile_pid" 2>/dev/null || true; fi
    if [[ -n "$compositor_pid" ]]; then kill "$compositor_pid" 2>/dev/null || true; fi
    if [[ -n "$xserver_pid" ]]; then kill "$xserver_pid" 2>/dev/null || true; fi
    find "$test_dir" -type f -delete 2>/dev/null || true
    find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
}
trap cleanup EXIT INT TERM

display=
for number in $(seq 311 330); do
    if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
        display=":$number"
        break
    fi
done
[[ -n "$display" ]]
"${x_server[@]}" "$display" "${x_server_args[@]}" >"$test_dir/xserver.log" 2>&1 &
xserver_pid=$!
for _ in $(seq 1 100); do
    if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
    sleep 0.05
done
DISPLAY="$display" xdpyinfo >/dev/null

status_value() {
    awk -v key="$2" '$1 == key ":" { print $2; exit }' "/proc/$1/status"
}

printf 'run\tstartup_us\tidle_rss_kib\tloaded_rss_kib\tthreads\tfds\tp50_us\tp95_us\tmax_us\n'
for run in $(seq 1 "$runs"); do
    runtime_dir="$test_dir/runtime-$run"
    mkdir -m 700 "$runtime_dir"
    log="$test_dir/nobox-$run.log"
    started=$(date +%s%N)
    env -u WAYLAND_DISPLAY DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
        NOBOX_STATE_FILE="$test_dir/session-$run.toml" \
        "$nobox_binary" --backend wayland run --nested-x11 --no-autostart \
        >"$log" 2>&1 &
    compositor_pid=$!
    socket=
    for _ in $(seq 1 200); do
        socket=$(sed -n 's/^ready: //p' "$log" | head -n 1)
        if [[ -n "$socket" ]]; then break; fi
        kill -0 "$compositor_pid" 2>/dev/null
        sleep 0.01
    done
    [[ -n "$socket" ]]
    startup_us=$(( ($(date +%s%N) - started) / 1000 ))
    idle_rss=$(status_value "$compositor_pid" VmRSS)

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY="$socket" \
        "$probe_binary" --frame-profile >"$test_dir/profile-$run.log" &
    profile_pid=$!
    sleep 0.2
    loaded_rss=$(status_value "$compositor_pid" VmRSS)
    threads=$(status_value "$compositor_pid" Threads)
    fds=$(find "/proc/$compositor_pid/fd" -mindepth 1 -maxdepth 1 | wc -l)
    wait "$profile_pid"
    profile_pid=
    read -r frames p50 p95 maximum < <(
        sed -n 's/^frame-profile frames=\([0-9]*\) p50_us=\([0-9]*\) p95_us=\([0-9]*\) max_us=\([0-9]*\)$/\1 \2 \3 \4/p' \
            "$test_dir/profile-$run.log")
    [[ "$frames" == 120 ]]
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$run" "$startup_us" "$idle_rss" "$loaded_rss" "$threads" "$fds" \
        "$p50" "$p95" "$maximum"

    DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
        "$nobox_binary" --backend wayland --exit
    wait "$compositor_pid"
    compositor_pid=
    [[ ! -e "$runtime_dir/$socket" && ! -e "$runtime_dir/$socket.lock" ]]
done
