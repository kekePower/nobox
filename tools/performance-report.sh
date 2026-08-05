#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: performance-report.sh NOBOX OPENBOX [RUNS] [CLIENTS] [smart|positioned]}
openbox_binary=${2:?usage: performance-report.sh NOBOX OPENBOX [RUNS] [CLIENTS] [smart|positioned]}
runs=${3:-5}
clients=${4:-50}
workload=${5:-smart}

if [[ ! -x "$nobox_binary" || ! -x "$openbox_binary" ]]; then
    echo "NOBOX and OPENBOX must be executable files" >&2
    exit 2
fi
if [[ ! "$runs" =~ ^[0-9]+$ ]] || ((runs < 1 || runs > 25)); then
    echo "RUNS must be between 1 and 25" >&2
    exit 2
fi
if [[ ! "$clients" =~ ^[0-9]+$ ]] || ((clients < 1 || clients > 500)); then
    echo "CLIENTS must be between 1 and 500" >&2
    exit 2
fi
if [[ "$workload" != smart && "$workload" != positioned ]]; then
    echo "workload must be smart or positioned" >&2
    exit 2
fi
for dependency in awk cc date find hostname ldd readlink sed seq sort stat \
    tee uname wc xdpyinfo xprop; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "$dependency is required for the performance report" >&2
        exit 2
    fi
done

if command -v Xnest >/dev/null 2>&1; then
    x_server=(Xnest)
    x_server_args=(-geometry 1280x800 -ac)
elif command -v Xephyr >/dev/null 2>&1; then
    x_server=(Xephyr)
    x_server_args=(-screen 1280x800 -ac)
elif command -v Xvfb >/dev/null 2>&1; then
    x_server=(Xvfb)
    x_server_args=(-screen 0 1280x800x24 -ac)
else
    echo "Xnest, Xephyr, or Xvfb is required for the performance report" >&2
    exit 2
fi

report_dir=$(mktemp -d)
x_server_pid=
wm_pid=
client_pid=
cleanup_processes() {
    if [[ -n "$client_pid" ]]; then kill "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$wm_pid" ]]; then kill "$wm_pid" 2>/dev/null || true; fi
    if [[ -n "$x_server_pid" ]]; then kill "$x_server_pid" 2>/dev/null || true; fi
    if [[ -n "$client_pid" ]]; then wait "$client_pid" 2>/dev/null || true; fi
    if [[ -n "$wm_pid" ]]; then wait "$wm_pid" 2>/dev/null || true; fi
    if [[ -n "$x_server_pid" ]]; then wait "$x_server_pid" 2>/dev/null || true; fi
    client_pid=
    wm_pid=
    x_server_pid=
}
cleanup() {
    cleanup_processes
    rm -rf -- "$report_dir"
}
trap cleanup EXIT INT TERM

source_dir=$(cd "$(dirname "$0")/.." && pwd)
cc -O2 -std=c11 -Wall -Wextra -Werror "$source_dir/tests/performance-clients.c" \
    -o "$report_dir/performance-clients" -lX11

cat >"$report_dir/openbox-rc.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
  <focus><focusNew>yes</focusNew><followMouse>no</followMouse></focus>
  <desktops><number>4</number><firstdesk>1</firstdesk></desktops>
</openbox_config>
EOF

next_display() {
    local number
    for number in $(seq 90 130); do
        if ! DISPLAY=":$number" xdpyinfo >/dev/null 2>&1; then
            printf ':%s\n' "$number"
            return 0
        fi
    done
    return 1
}

status_value() {
    local process=$1
    local key=$2
    awk -v key="$key" '$1 == key ":" { print $2; found = 1 } END { if (!found) print 0 }' \
        "/proc/$process/status"
}

file_descriptor_count() {
    local process=$1
    find "/proc/$process/fd" -mindepth 1 -maxdepth 1 -printf '.\n' 2>/dev/null | wc -l
}

run_one() {
    local manager=$1
    local iteration=$2
    local display
    display=$(next_display) || {
        echo "no unused nested X11 display found" >&2
        return 1
    }
    local run_dir="$report_dir/$manager-$iteration"
    mkdir "$run_dir"

    "${x_server[@]}" "$display" "${x_server_args[@]}" >"$run_dir/xserver.log" 2>&1 &
    x_server_pid=$!
    for _ in $(seq 1 100); do
        if DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then break; fi
        sleep 0.01
    done
    if ! DISPLAY="$display" xdpyinfo >/dev/null 2>&1; then
        echo "nested X server did not become ready on $display" >&2
        return 1
    fi

    local started_ns
    started_ns=$(date +%s%N)
    if [[ "$manager" == nobox ]]; then
        DISPLAY="$display" NOBOX_CONFIG_FILE="$run_dir/config.toml" \
            NOBOX_STATE_FILE="$run_dir/session.toml" \
            "$nobox_binary" run --no-autostart >"$run_dir/wm.log" 2>&1 &
    else
        DISPLAY="$display" "$openbox_binary" --sm-disable \
            --config-file "$report_dir/openbox-rc.xml" >"$run_dir/wm.log" 2>&1 &
    fi
    wm_pid=$!

    DISPLAY="$display" "$report_dir/performance-clients" 1 --retry-map \
        >"$run_dir/probe.out" 2>"$run_dir/probe.err" &
    client_pid=$!
    for _ in $(seq 1 1000); do
        if ! kill -0 "$wm_pid" 2>/dev/null; then
            echo "$manager exited during startup" >&2
            sed -n '1,160p' "$run_dir/wm.log" >&2
            return 1
        fi
        if [[ -s "$run_dir/probe.out" ]]; then break; fi
        if ! kill -0 "$client_pid" 2>/dev/null; then
            echo "$manager did not manage the startup probe" >&2
            sed -n '1,120p' "$run_dir/probe.err" >&2
            return 1
        fi
        sleep 0.01
    done
    if [[ ! -s "$run_dir/probe.out" ]]; then
        echo "$manager did not become ready for its first client" >&2
        sed -n '1,120p' "$run_dir/probe.err" >&2
        return 1
    fi
    local ready_ns
    ready_ns=$(date +%s%N)
    local startup_us=$(((ready_ns - started_ns) / 1000))
    kill "$client_pid" 2>/dev/null || true
    wait "$client_pid" 2>/dev/null || true
    client_pid=
    for _ in $(seq 1 100); do
        if DISPLAY="$display" xprop -root _NET_CLIENT_LIST 2>/dev/null |
            grep -Eq 'window id # *$'; then
            break
        fi
        sleep 0.01
    done
    local idle_rss
    idle_rss=$(status_value "$wm_pid" VmRSS)

    local workload_args=()
    if [[ "$workload" == positioned ]]; then workload_args=(--positioned); fi
    DISPLAY="$display" "$report_dir/performance-clients" "$clients" "${workload_args[@]}" \
        >"$run_dir/client.out" 2>"$run_dir/client.err" &
    client_pid=$!
    for _ in $(seq 1 1000); do
        if [[ -s "$run_dir/client.out" ]]; then break; fi
        if ! kill -0 "$client_pid" 2>/dev/null; then
            echo "performance client exited before the workload was managed" >&2
            sed -n '1,120p' "$run_dir/client.err" >&2
            return 1
        fi
        sleep 0.01
    done
    local manage_us
    manage_us=$(sed -n 's/^manage_us=//p' "$run_dir/client.out")
    if [[ ! "$manage_us" =~ ^[0-9]+$ ]]; then
        echo "performance client did not report valid management latency" >&2
        sed -n '1,120p' "$run_dir/client.err" >&2
        return 1
    fi

    local loaded_rss threads descriptors
    loaded_rss=$(status_value "$wm_pid" VmRSS)
    threads=$(status_value "$wm_pid" Threads)
    descriptors=$(file_descriptor_count "$wm_pid")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$manager" "$iteration" "$startup_us" "$manage_us" "$idle_rss" \
        "$loaded_rss" "$threads" "$descriptors" | tee -a "$report_dir/runtime.tsv"
    cleanup_processes
}

dependency_footprint() {
    local manager=$1
    local executable=$2
    local dependency_file="$report_dir/$manager-dependencies"
    ldd "$executable" 2>/dev/null | awk \
        '$2 == "=>" && $3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' |
        while IFS= read -r dependency; do readlink -f "$dependency"; done |
        sort -u >"$dependency_file"
    local count bytes=0 dependency
    count=$(wc -l <"$dependency_file")
    while IFS= read -r dependency; do
        bytes=$((bytes + $(stat -Lc %s "$dependency")))
    done <"$dependency_file"
    printf '%s\t%s\t%s\t%s\n' "$manager" "$(stat -Lc %s "$executable")" "$count" "$bytes"
}

printf '# nobox/Openbox nested-X performance report\n'
printf '# generated=%s host=%s kernel=%s x_server=%s runs=%s clients=%s workload=%s\n' \
    "$(date --iso-8601=seconds)" "$(hostname)" "$(uname -r)" "${x_server[0]}" "$runs" "$clients" "$workload"
printf '# versions: %s; %s\n' \
    "$("$nobox_binary" --version | sed -n '1p')" \
    "$("$openbox_binary" --version | sed -n '1p')"
printf '# Footprint bytes include the executable and resolved shared objects separately.\n'
printf 'footprint\tmanager\texecutable_bytes\tshared_objects\tresolved_shared_bytes\n'
while IFS=$'\t' read -r manager executable_bytes shared_objects shared_bytes; do
    printf 'footprint\t%s\t%s\t%s\t%s\n' \
        "$manager" "$executable_bytes" "$shared_objects" "$shared_bytes"
done < <(dependency_footprint nobox "$nobox_binary"; dependency_footprint openbox "$openbox_binary")
printf '# Runtime units: startup_us, manage_us, and KiB RSS from /proc. Lower is better.\n'
printf 'runtime\tmanager\trun\tstartup_us\tmanage_us\tidle_rss_kib\tloaded_rss_kib\tthreads\tfds\n'
printf 'manager\trun\tstartup_us\tmanage_us\tidle_rss_kib\tloaded_rss_kib\tthreads\tfds\n' \
    >"$report_dir/runtime.tsv"
for manager in nobox openbox; do
    for iteration in $(seq 1 "$runs"); do
        printf 'runtime\t'
        run_one "$manager" "$iteration"
    done
done

printf '# Arithmetic means retain the raw runs above so variance remains visible.\n'
printf 'summary\tmanager\truns\tstartup_mean_us\tmanage_mean_us\tidle_rss_mean_kib\tloaded_rss_mean_kib\n'
awk -F '\t' 'NR > 1 {
    count[$1] += 1; startup[$1] += $3; manage[$1] += $4;
    idle[$1] += $5; loaded[$1] += $6
} END {
    for (manager in count) {
        printf "summary\t%s\t%d\t%.0f\t%.0f\t%.0f\t%.0f\n", manager,
            count[manager], startup[manager] / count[manager],
            manage[manager] / count[manager], idle[manager] / count[manager],
            loaded[manager] / count[manager]
    }
}' "$report_dir/runtime.tsv" | sort
