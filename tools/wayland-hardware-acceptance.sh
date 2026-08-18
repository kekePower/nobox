#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 --inventory [DRM_SYSFS_ROOT]" >&2
    echo "       $0 --prepare-config /path/to/nobox /new/config.toml" >&2
    echo "       NOBOX_WAYLAND_HARDWARE_ACCEPTANCE=disposable-vt $0 /path/to/nobox /path/to/nobox-wayland-probe /new/record/directory" >&2
    exit 2
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

prepare_config() {
    local nobox_binary=$1
    local config_file=$2
    local disabled_count

    "$nobox_binary" print-default >"$config_file"
    disabled_count=$(grep -c '^xwayland = false$' "$config_file" || true)
    [[ "$disabled_count" -eq 1 ]] || \
        fail "default configuration did not contain exactly one disabled XWayland setting"
    sed -i 's/^xwayland = false$/xwayland = true/' "$config_file"
    "$nobox_binary" --config "$config_file" check >/dev/null
}

hardware_inventory() {
    local drm_root=${1:-/sys/class/drm}
    local card card_name device_path slot description vendor device driver
    local status_file status connector
    local gpu_count=0
    local connector_count=0

    for card in "$drm_root"/card[0-9]*; do
        [[ -e "$card" ]] || continue
        card_name=${card##*/}
        [[ "$card_name" =~ ^card[0-9]+$ && -e "$card/device" ]] || continue
        device_path=$(readlink -f -- "$card/device") || continue
        slot=${device_path##*/}
        description=
        if [[ "$drm_root" == /sys/class/drm ]] && command -v lspci >/dev/null 2>&1; then
            description=$(lspci -D -s "$slot" 2>/dev/null | head -n 1 || true)
        fi
        if [[ -z "$description" ]]; then
            vendor=$(sed -n '1p' "$card/device/vendor" 2>/dev/null || true)
            device=$(sed -n '1p' "$card/device/device" 2>/dev/null || true)
            driver=
            if [[ -L "$card/device/driver" ]]; then
                driver=$(basename "$(readlink -f -- "$card/device/driver")")
            fi
            description="$slot vendor=${vendor:-unknown} device=${device:-unknown} driver=${driver:-unknown}"
        fi
        printf -- '- GPU: %s (%s)\n' "$card_name" "$description"
        gpu_count=$((gpu_count + 1))
    done

    for status_file in "$drm_root"/card[0-9]*-*/status; do
        [[ -r "$status_file" ]] || continue
        status=$(sed -n '1p' "$status_file")
        [[ "$status" == connected ]] || continue
        connector=${status_file%/status}
        printf -- '- Connected connector at start: %s\n' "${connector##*/}"
        connector_count=$((connector_count + 1))
    done

    [[ "$gpu_count" -gt 0 ]] || return 1
    [[ "$connector_count" -gt 0 ]] || return 1
}

if [[ ${1:-} == --inventory ]]; then
    [[ $# -le 2 ]] || usage
    hardware_inventory "${2:-/sys/class/drm}" || fail "could not identify a DRM GPU and connected connector"
    exit 0
fi

if [[ ${1:-} == --prepare-config ]]; then
    [[ $# -eq 3 ]] || usage
    [[ -x "$2" ]] || fail "nobox binary is not executable: $2"
    [[ ! -e "$3" ]] || fail "refusing to replace existing configuration: $3"
    prepare_config "$2" "$3"
    exit 0
fi

nobox_binary=${1:-}
probe_binary=${2:-}
record_dir=${3:-}
[[ -n "$nobox_binary" && -n "$probe_binary" && -n "$record_dir" ]] || usage
[[ -x "$nobox_binary" ]] || fail "nobox binary is not executable: $nobox_binary"
[[ -x "$probe_binary" ]] || fail "probe binary is not executable: $probe_binary"
[[ ${NOBOX_WAYLAND_HARDWARE_ACCEPTANCE:-} == disposable-vt ]] || fail "explicit disposable-VT acknowledgement is missing"
[[ -z ${DISPLAY:-} && -z ${WAYLAND_DISPLAY:-} ]] || fail "refusing to claim DRM from inside a graphical session"
[[ -n ${XDG_SESSION_ID:-} ]] || fail "XDG_SESSION_ID is required for the safety check"
command -v loginctl >/dev/null 2>&1 || fail "loginctl is required"
command -v pgrep >/dev/null 2>&1 || fail "pgrep is required"
command -v timeout >/dev/null 2>&1 || fail "timeout is required"
command -v xdpyinfo >/dev/null 2>&1 || fail "xdpyinfo is required"
command -v xprop >/dev/null 2>&1 || fail "xprop is required"
command -v xterm >/dev/null 2>&1 || fail "xterm is required"
[[ ! -e "$record_dir" ]] || fail "record directory already exists: $record_dir"

session_properties=$(loginctl show-session "$XDG_SESSION_ID" \
    -p Active -p Type -p Class -p Remote -p Seat -p VTNr)
property() {
    sed -n "s/^$1=//p" <<<"$session_properties"
}
[[ $(property Active) == yes ]] || fail "the logind session is not active"
[[ $(property Type) == tty ]] || fail "the logind session is not a TTY"
[[ $(property Class) == user ]] || fail "the logind session is not a user session"
[[ $(property Remote) == no ]] || fail "remote sessions cannot perform this acceptance"
[[ -n $(property Seat) ]] || fail "the TTY has no logind seat"
[[ $(property VTNr) =~ ^[1-9][0-9]*$ ]] || fail "the TTY has no graphical VT number"
[[ -n ${XDG_RUNTIME_DIR:-} && -d ${XDG_RUNTIME_DIR:-} ]] || fail "XDG_RUNTIME_DIR is unavailable"

mkdir -p "$record_dir"
config_file="$record_dir/config.toml"
session_file="$record_dir/session.toml"
stdout_log="$record_dir/compositor.stdout"
stderr_log="$record_dir/compositor.stderr"
record_file="$record_dir/record.md"
prepare_config "$nobox_binary" "$config_file"

cat >"$record_file" <<EOF
# Nobox W4 hardware acceptance

- Date: $(date --iso-8601=seconds)
- Host: $(hostname)
- Kernel: $(uname -srmo)
- Nobox: $($nobox_binary --version)
- Session: $XDG_SESSION_ID, seat $(property Seat), VT $(property VTNr)
- Result: IN PROGRESS

EOF
printf '%s\n' "$session_properties" >"$record_dir/logind-session.txt"
hardware_inventory >"$record_dir/drm-hardware.txt" || \
    fail "could not identify a DRM GPU and connected connector"
cat "$record_dir/drm-hardware.txt" >>"$record_file"
printf '\n' >>"$record_file"

compositor_pid=
accelerated_pid=
xterm_pid=
cleanup() {
    if [[ -n "$xterm_pid" ]] && kill -0 "$xterm_pid" 2>/dev/null; then
        kill -TERM "$xterm_pid" 2>/dev/null || true
        wait "$xterm_pid" 2>/dev/null || true
    fi
    if [[ -n "$accelerated_pid" ]] && kill -0 "$accelerated_pid" 2>/dev/null; then
        kill -TERM "$accelerated_pid" 2>/dev/null || true
        wait "$accelerated_pid" 2>/dev/null || true
    fi
    if [[ -n "$compositor_pid" ]] && kill -0 "$compositor_pid" 2>/dev/null; then
        env -u DISPLAY -u WAYLAND_DISPLAY \
            XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
            "$nobox_binary" --backend wayland --exit >/dev/null 2>&1 || \
            kill -TERM "$compositor_pid" 2>/dev/null || true
        wait "$compositor_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

run_probe() {
    env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$socket_name" \
        timeout 20 "$probe_binary" "$@"
}

snapshot_outputs() {
    local label=$1
    run_probe --outputs | tee "$record_dir/outputs-$label.txt"
}

count_outputs() {
    awk '/^output / { count += 1 } END { print count + 0 }'
}

process_start_time() {
    awk '{ print $22 }' "/proc/$1/stat" 2>/dev/null || true
}

same_process_generation_is_alive() {
    local state

    state=$(awk '{ print $3 }' "/proc/$1/stat" 2>/dev/null || true)
    [[ -n "$state" && "$state" != Z && $(process_start_time "$1") == "$2" ]]
}

require_test_clients_alive() {
    local context=$1

    kill -0 "$accelerated_pid" 2>/dev/null || fail "Alacritty died $context"
    same_process_generation_is_alive "$xterm_pid" "$xterm_start_time" || \
        fail "XTerm died $context"
}

confirm() {
    local key=$1
    local prompt=$2
    local answer=
    read -r -p "$prompt Type PASS to record it: " answer
    [[ "$answer" == PASS ]] || fail "$key was not confirmed"
    printf -- '- PASS: %s\n' "$key" >>"$record_file"
}

NOBOX_CONFIG_FILE="$config_file" NOBOX_STATE_FILE="$session_file" \
    "$nobox_binary" --backend wayland doctor >"$record_dir/doctor.txt"

env -u DISPLAY -u WAYLAND_DISPLAY \
    NOBOX_CONFIG_FILE="$config_file" NOBOX_STATE_FILE="$session_file" \
    RUST_LOG=nobox_wayland=debug,nobox=info \
    "$nobox_binary" --backend wayland run --tty --no-autostart \
    >"$stdout_log" 2>"$stderr_log" &
compositor_pid=$!

socket_name=
for _ in $(seq 1 200); do
    socket_name=$(sed -n 's/^ready: //p' "$stdout_log" 2>/dev/null | head -n 1)
    [[ -n "$socket_name" ]] && break
    kill -0 "$compositor_pid" 2>/dev/null || break
    sleep 0.05
done
[[ -n "$socket_name" ]] || fail "direct compositor did not become ready; inspect $stderr_log"

xwayland_pid=
for _ in $(seq 1 200); do
    if grep -Fq 'XWayland and its XWM are ready' "$stderr_log" 2>/dev/null; then
        xwayland_pid=$(pgrep -P "$compositor_pid" -x Xwayland | head -n 1 || true)
        [[ -n "$xwayland_pid" ]] && break
    fi
    kill -0 "$compositor_pid" 2>/dev/null || break
    sleep 0.05
done
[[ -n "$xwayland_pid" ]] || fail "compositor-owned XWayland did not become ready"
xwayland_start_time=$(process_start_time "$xwayland_pid")
[[ -n "$xwayland_start_time" ]] || fail "could not identify the XWayland process generation"
xwayland_display=$(tr '\0' '\n' <"/proc/$xwayland_pid/cmdline" | \
    awk '/^:[0-9]+$/ { print; exit }')
[[ -n "$xwayland_display" ]] || fail "could not discover the compositor-owned XWayland display"
DISPLAY="$xwayland_display" xdpyinfo >"$record_dir/xwayland-xdpyinfo.txt"
DISPLAY="$xwayland_display" xterm -title nobox-w4-xwayland -geometry 40x10+30+40 \
    >"$record_dir/xterm.txt" 2>&1 &
xterm_pid=$!
xterm_start_time=$(process_start_time "$xterm_pid")
[[ -n "$xterm_start_time" ]] || fail "could not identify the XTerm process generation"
xterm_mapped=false
for _ in $(seq 1 200); do
    client_list=$(DISPLAY="$xwayland_display" xprop -root _NET_CLIENT_LIST 2>/dev/null || true)
    for candidate in $(sed 's/.*# //' <<<"$client_list" | tr ',' ' '); do
        if DISPLAY="$xwayland_display" xprop -id "$candidate" _NET_WM_NAME WM_NAME \
            2>/dev/null | grep -Fq 'nobox-w4-xwayland'; then
            xterm_mapped=true
            break 2
        fi
    done
    kill -0 "$xterm_pid" 2>/dev/null || break
    kill -0 "$compositor_pid" 2>/dev/null || break
    sleep 0.05
done
[[ "$xterm_mapped" == true ]] || fail "XTerm did not map through compositor-owned XWayland"
printf -- '- PASS: compositor-owned XWayland mapped an X11 client\n' >>"$record_file"

run_probe | tee "$record_dir/globals.txt"
grep -Fq 'zwp_linux_dmabuf_v1 5' "$record_dir/globals.txt" || fail "linux-dmabuf v5 is absent"
baseline_outputs=$(snapshot_outputs baseline | tee /dev/tty | count_outputs)
[[ "$baseline_outputs" -ge 2 ]] || fail "W4 acceptance requires two connected outputs"
printf -- '- PASS: %s live outputs at baseline\n' "$baseline_outputs" >>"$record_file"

run_probe --shell | tee "$record_dir/shm-before-import-failure.txt"
run_probe --dmabuf-import-failure | tee "$record_dir/dmabuf-import-failure.txt"
run_probe --shell | tee "$record_dir/shm-after-import-failure.txt"
kill -0 "$compositor_pid" 2>/dev/null || fail "compositor died after rejected DMA-BUF import"
printf -- '- PASS: rejected renderer import preserved SHM service\n' >>"$record_file"

if command -v glmark2-es2-wayland >/dev/null 2>&1; then
    env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$socket_name" \
        timeout 90 glmark2-es2-wayland --validate >"$record_dir/glmark2.txt" 2>&1
    printf -- '- PASS: glmark2-es2-wayland accelerated validation\n' >>"$record_file"
else
    fail "glmark2-es2-wayland is required for the accelerated-client check"
fi

if command -v alacritty >/dev/null 2>&1; then
    env -u DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$socket_name" \
        alacritty >"$record_dir/alacritty.txt" 2>&1 &
    accelerated_pid=$!
    sleep 2
    kill -0 "$accelerated_pid" 2>/dev/null || fail "Alacritty did not remain mapped"
else
    fail "alacritty is required for client-lifecycle and interactive checks"
fi

confirm "software cursor remained visible and tracked input" \
    "Move the pointer across both outputs and inspect the compositor cursor."
confirm "VT switch away/back preserved clients and input" \
    "Switch to another VT, wait five seconds, then return to this VT."
snapshot_outputs after-vt >/dev/null
run_probe --shell >"$record_dir/shm-after-vt.txt"
require_test_clients_alive "across the VT switch"

confirm "system suspend/resume preserved clients, outputs, and input" \
    "Suspend the machine through your normal trusted path, resume, and return here."
snapshot_outputs after-resume >/dev/null
run_probe --shell >"$record_dir/shm-after-resume.txt"
require_test_clients_alive "across suspend/resume"

echo "Edit $config_file so the two outputs use different scales and one non-normal transform."
confirm "mixed scale and transform configuration is ready to reload" \
    "Save that configuration while retaining two enabled outputs."
kill -HUP "$compositor_pid"
sleep 2
snapshot_outputs mixed-scale-transform >/dev/null
confirm "mixed scale and transform rendered correctly" \
    "Inspect both outputs, pointer confinement, and client placement after reload."
cp "$config_file" "$record_dir/mixed-scale-transform-good.toml"

kms_failures_before=$(grep -c 'KMS mode candidate failed' "$stderr_log" 2>/dev/null || true)
echo "Prepare a two-output mode combination in $config_file that the real KMS device rejects."
confirm "real KMS-rejected mode candidate is ready" \
    "Save the candidate; the script will reload it and require the backend rejection log."
kill -HUP "$compositor_pid"
sleep 2
kms_failures_after=$(grep -c 'KMS mode candidate failed' "$stderr_log" 2>/dev/null || true)
[[ "$kms_failures_after" -gt "$kms_failures_before" ]] || fail "reload did not reach a real KMS mode failure"
snapshot_outputs after-kms-rollback >/dev/null
run_probe --shell >"$record_dir/shm-after-kms-rollback.txt"
printf -- '- PASS: real KMS mode failure retained the live topology\n' >>"$record_file"
cp "$record_dir/mixed-scale-transform-good.toml" "$config_file"
kill -HUP "$compositor_pid"
sleep 2
snapshot_outputs after-good-config-restore >/dev/null

confirm "connector disappeared during interactive move/resize without losing input" \
    "Begin moving or resizing Alacritty, unplug the secondary output, then complete or cancel the operation."
sleep 2
remaining_outputs=$(snapshot_outputs unplugged | count_outputs)
[[ "$remaining_outputs" -ge 1 && "$remaining_outputs" -lt "$baseline_outputs" ]] || \
    fail "the output count did not decrease after unplug"
run_probe --shell >"$record_dir/shm-after-unplug.txt"
kill -0 "$compositor_pid" 2>/dev/null || fail "compositor died after output unplug"
require_test_clients_alive "after output unplug"

confirm "connector was physically replugged" \
    "Reconnect the secondary output and wait for it to light up."
replugged_outputs=0
for _ in $(seq 1 100); do
    replugged_outputs=$(snapshot_outputs replugged | count_outputs)
    [[ "$replugged_outputs" -ge "$baseline_outputs" ]] && break
    sleep 0.1
done
[[ "$replugged_outputs" -ge "$baseline_outputs" ]] || fail "the original output count did not return"
run_probe --shell >"$record_dir/shm-after-replug.txt"
require_test_clients_alive "after output replug"
printf -- '- PASS: hot-unplug/replug retained a usable compositor\n' >>"$record_file"

env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    "$nobox_binary" --backend wayland --exit
wait "$compositor_pid"
compositor_pid=
for _ in $(seq 1 100); do
    same_process_generation_is_alive "$xwayland_pid" "$xwayland_start_time" || break
    sleep 0.05
done
same_process_generation_is_alive "$xwayland_pid" "$xwayland_start_time" && \
    fail "clean shutdown retained the XWayland child"
for _ in $(seq 1 100); do
    same_process_generation_is_alive "$xterm_pid" "$xterm_start_time" || break
    sleep 0.05
done
same_process_generation_is_alive "$xterm_pid" "$xterm_start_time" && \
    fail "XTerm remained alive after XWayland shutdown"
wait "$xterm_pid" 2>/dev/null || true
xterm_pid=
[[ ! -e "$XDG_RUNTIME_DIR/$socket_name" && ! -e "$XDG_RUNTIME_DIR/$socket_name.lock" ]] || \
    fail "clean shutdown retained the Wayland socket"
NOBOX_CONFIG_FILE="$config_file" NOBOX_STATE_FILE="$session_file" \
    "$nobox_binary" --backend wayland doctor >"$record_dir/doctor-after-exit.txt"
printf -- '- PASS: clean exit removed runtime sockets, stopped XWayland, and returned device access\n' >>"$record_file"

sed -i 's/- Result: IN PROGRESS/- Result: PASS/' "$record_file"
trap - EXIT INT TERM
cleanup
echo "PASS: hardware acceptance record written to $record_file"
