#!/usr/bin/env bash
set -euo pipefail

usage="usage: x11-agent-seat.sh /path/to/nobox /path/to/nobox-agent-wire-probe [/path/to/nobox-agent] [/path/to/fault-helper]"
nobox_binary=${1:?$usage}
probe_binary=${2:?$usage}
# The MCP companion is optional: without it the protocol itself is still
# exercised in full, and only the companion's own section is skipped.
companion_binary=${3:-}
fault_helper=${4:-}
for dependency in cc xdpyinfo xprop xterm python3; do
    if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "SKIP: $dependency is required for the agent seat test"
        exit 77
    fi
done
if [[ ! -x "$probe_binary" ]]; then
    echo "SKIP: the agent seat probe was not built at $probe_binary"
    exit 77
fi

# Grants bind to absolute executable paths, so resolve what we were handed.
nobox_binary=$(readlink -f "$nobox_binary")
probe_binary=$(readlink -f "$probe_binary")
if [[ -n "$companion_binary" && -x "$companion_binary" ]]; then
    companion_binary=$(readlink -f "$companion_binary")
else
    companion_binary=
fi

helpers=$(mktemp -d)
trap 'rm -rf -- "$helpers"' EXIT
if ! cc "$(dirname "$0")/agent-input-client.c" -o "$helpers/agent-input-client" -lX11 \
    2>/dev/null; then
    echo "SKIP: Xlib development files are required for the agent seat test"
    exit 77
fi
if ! cc "$(dirname "$0")/press-key.c" -o "$helpers/press-key" -lXtst -lX11 2>/dev/null; then
    echo "SKIP: XTest development files are required for the agent seat test"
    exit 77
fi
if ! cc -std=c11 -Wall -Wextra -Wpedantic -Werror \
    "$(dirname "$0")/agent-seat-owner.c" -o "$helpers/agent-seat-owner" -lX11 \
    2>/dev/null; then
    echo "SKIP: Xlib development files are required for Agent Seat ownership tests"
    exit 77
fi

source "$(dirname "$0")/nested-x.sh"
# Prefer a server that offers Composite, so covered-window capture is actually
# exercised rather than skipped; an explicit NOBOX_XSERVER still wins.
if [[ -z "${NOBOX_XSERVER:-}" ]] && command -v Xvfb >/dev/null 2>&1; then
    NOBOX_XSERVER=xvfb
fi
select_nested_x_server 800 600

test_dir=$(mktemp -d)
isolate_nested_session "$test_dir"
semantic_helper_mode=
if [[ -n "$fault_helper" && -f "$fault_helper" ]]; then
    cp -- "$nobox_binary" "$test_dir/nobox"
    cp -- "$fault_helper" "$test_dir/agent-semantic-helper"
    chmod 700 "$test_dir/nobox" "$test_dir/agent-semantic-helper"
    nobox_binary="$test_dir/nobox"
    semantic_helper_mode="$test_dir/semantic-helper-mode"
    printf '%s\n' unavailable >"$semantic_helper_mode"
fi
xserver_pid=
nobox_pid=
visible_pid=
secret_pid=
late_pid=
post_loss_pid=
watched_xterm=
watch_pid=
scoped_pid=
managed_pid=
input_client_pid=
freeze_pid=
text_interrupt_pid=
consent_pid=
revoke_pid=
semantic_pid=
provider_pid=
cleanup() {
    rm -rf -- "$helpers"
    for pid in "$watch_pid" "$scoped_pid" "$freeze_pid" "$text_interrupt_pid" \
        "$consent_pid" "$revoke_pid" \
        "$semantic_pid" "$provider_pid" \
        "$watched_xterm" \
        "$managed_pid" \
        "$input_client_pid" "$post_loss_pid" "$late_pid" "$secret_pid" "$visible_pid" "$nobox_pid" \
        "$xserver_pid"; do
        if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; fi
    done
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

fail() {
    echo "$1" >&2
    sed -n '1,200p' "$test_dir/nobox.log" >&2
    exit 1
}

# A private runtime directory keeps the seat's socket inside the test.
runtime_dir="$test_dir/run"
mkdir -p "$runtime_dir"
chmod 700 "$runtime_dir"

# The grant binds to the probe's executable. The impostor is a byte-for-byte
# copy that declares the same harness name from a different path, so the test
# proves the binding is the executable and not anything the peer says.
probe="$test_dir/nobox-agent-wire-probe"
impostor="$test_dir/impostor-probe"
scoped="$test_dir/scoped-probe"
manager="$test_dir/manage-probe"
driver="$test_dir/input-probe"
camera="$test_dir/capture-probe"
launcher="$test_dir/launch-probe"
asker="$test_dir/consent-probe"
semantic="$test_dir/semantic-probe"
cp -- "$probe_binary" "$probe"
cp -- "$probe_binary" "$impostor"
cp -- "$probe_binary" "$scoped"
cp -- "$probe_binary" "$manager"
cp -- "$probe_binary" "$driver"
cp -- "$probe_binary" "$camera"
cp -- "$probe_binary" "$launcher"
cp -- "$probe_binary" "$asker"
cp -- "$probe_binary" "$semantic"

cat >"$test_dir/config.toml" <<EOF
[agent]
enabled = true
suppression_ms = 1500
kill_chord = "C-A-Escape"
policy = "ask"

[agent.launch]
policy = "allow_listed"
allow = ["nobox-agent-launched.desktop"]
user_entries = false

[[agent.grants]]
label = "integration probe"
executable = "$probe"
capabilities = ["observe"]

[[agent.grants]]
label = "semantic probe"
executable = "$semantic"
capabilities = ["observe", "accessibility"]

# A grant that may act, not only observe.
[[agent.grants]]
label = "management probe"
executable = "$manager"
capabilities = ["observe", "manage"]

# A grant that may inject input. The manager marks the session while it holds
# this and briefly highlights the window it types into.
[[agent.grants]]
label = "input probe"
executable = "$driver"
capabilities = ["observe", "input", "manage.activate"]

# A grant that may look at pixels, including covered windows.
[[agent.grants]]
label = "capture probe"
executable = "$camera"
capabilities = ["observe", "capture"]

# A grant that may start applications from the catalog.
[[agent.grants]]
label = "launch probe"
executable = "$launcher"
capabilities = ["observe", "launch"]

# A scoped grant: this session may only ever perceive the watched window.
[[agent.grants]]
label = "scoped probe"
executable = "$scoped"
capabilities = ["observe"]
scope = { title = "nobox-agent-watched" }

# A window the user marks hidden must be absent from every agent answer, and
# indistinguishable from one that never existed.
[[applications]]
match = { title = "nobox-agent-secret" }
agent_visibility = "hidden"
EOF

if [[ -n "$companion_binary" ]]; then
    cat >>"$test_dir/config.toml" <<EOF

[[agent.grants]]
label = "MCP companion"
executable = "$companion_binary"
capabilities = ["observe"]
EOF
fi

# Fixture desktop entries: the only things an agent can start are catalog
# identifiers, so the catalog is what the test controls.
data_home="$test_dir/data"
mkdir -p "$data_home/applications"
cat >"$data_home/applications/nobox-agent-launched.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=nobox agent launch fixture
Exec=xterm -title nobox-agent-launched -e sleep 600
StartupNotify=true
StartupWMClass=XTerm
Categories=Utility;
ENTRY
cat >"$data_home/applications/nobox-agent-forbidden.desktop" <<'ENTRY'
[Desktop Entry]
Type=Application
Name=nobox agent forbidden fixture
Exec=xterm -title nobox-agent-forbidden -e sleep 600
Categories=Utility;
ENTRY

display=
for number in $(seq 471 490); do
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
if DISPLAY="$display" "$helpers/agent-seat-owner" owner >/dev/null 2>&1; then
    fail "the isolated screen unexpectedly began with an Agent Seat owner"
fi

# Exercise a real level-3 character when the nested server has the XKB rules
# installed. The same assertion remains useful on the server's default layout.
if command -v setxkbmap >/dev/null 2>&1; then
    DISPLAY="$display" setxkbmap no >"$test_dir/setxkbmap.log" 2>&1 || true
fi

DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" XDG_DATA_HOME="$data_home" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then
        fail "nobox exited during startup"
    fi
    if grep -q 'agent seat listening' "$test_dir/nobox.log"; then break; fi
    sleep 0.1
done
grep -q 'agent seat listening' "$test_dir/nobox.log" ||
    fail "nobox did not start its agent seat"

socket="$runtime_dir/nobox/agent-seat-${display#:}.sock"
[[ -S "$socket" ]] || fail "the agent seat socket is missing at $socket"

# The seat lives in a private directory behind a private socket.
directory_mode=$(stat -c %a "$runtime_dir/nobox")
socket_mode=$(stat -c %a "$socket")
[[ "$directory_mode" == "700" ]] ||
    fail "the agent seat directory is mode $directory_mode, not 700"
[[ "$socket_mode" == "600" ]] ||
    fail "the agent seat socket is mode $socket_mode, not 600"

# Discovery is the traditional X11 route: a root property naming the protocol
# version and the socket path.
advertisement=$(DISPLAY="$display" xprop -root _AGENT_SEAT)
grep -q 'agent-seat' <<<"$advertisement" ||
    fail "the root window does not advertise the protocol: $advertisement"
grep -qF "$socket" <<<"$advertisement" ||
    fail "the advertisement does not name the socket: $advertisement"
seat_owner=$(DISPLAY="$display" "$helpers/agent-seat-owner" owner) ||
    fail "nobox did not claim the per-screen Agent Seat selection"
support_owner=$(DISPLAY="$display" xprop -root _NET_SUPPORTING_WM_CHECK |
    grep -o '0x[0-9a-fA-F]*' | head -n 1)
[[ -n "$support_owner" && "$seat_owner" != "$support_owner" ]] ||
    fail "the Agent Seat did not use a dedicated owner window"
root_advertisement=$(DISPLAY="$display" xprop -notype -root _AGENT_SEAT | sed 's/^[^=]*= *//')
owner_advertisement=$(DISPLAY="$display" xprop -notype -id "$seat_owner" _AGENT_SEAT |
    sed 's/^[^=]*= *//')
[[ "$root_advertisement" == "$owner_advertisement" ]] ||
    fail "the Agent Seat owner and root advertisements differ"

run_probe() {
    local binary=$1 scenario=$2 label=$3
    shift 3
    if ! "$binary" "$socket" "$scenario" nobox-integration-probe "$@" \
        >"$test_dir/probe-$scenario.log" 2>&1; then
        echo "the $label scenario failed" >&2
        sed -n '1,60p' "$test_dir/probe-$scenario.log" >&2
        exit 1
    fi
}

# Structured logs carry ANSI attributes here, so plain greps on field names
# need the escapes removed first.
log_contains() {
    sed 's/\x1b\[[0-9;]*m//g' "$test_dir/nobox.log" | grep -q "$1"
}

count_managed_windows() {
    DISPLAY="$display" xprop -root _NET_CLIENT_LIST | tr ',' '\n' | grep -c '0x' || true
}

wait_for_managed_windows() {
    local wanted=$1
    for _ in $(seq 1 80); do
        if [[ "$(count_managed_windows)" -ge "$wanted" ]]; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

# A grant confers exactly its atoms: observe is answered, manage is refused,
# and a refusal about a missing window is not a refusal about the grant.
run_probe "$probe" granted "stored grant"
grep -q 'granted=observe.structure,observe.titles' "$test_dir/probe-granted.log" ||
    fail "the stored grant was not issued as configured"
grep -q 'client.get -> no_such_client' "$test_dir/probe-granted.log" ||
    fail "a granted call about a missing window was not answered as such"
grep -q 'workspace.switch -> denied' "$test_dir/probe-granted.log" ||
    fail "an ungranted capability was not denied"

# The same declared harness name from an unnamed executable holds nothing.
run_probe "$impostor" unbound "deny by default"
grep -q 'granted=0' "$test_dir/probe-unbound.log" ||
    fail "an executable without a grant received capabilities"
grep -q 'desktop.snapshot -> denied' "$test_dir/probe-unbound.log" ||
    fail "a session without a grant was not denied"

# Two windows: one ordinary, one the configuration hides from agents.
DISPLAY="$display" xterm -title nobox-agent-visible -geometry 40x10+30+40 -e sleep 600 \
    >"$test_dir/xterm-visible.log" 2>&1 &
visible_pid=$!
DISPLAY="$display" xterm -title nobox-agent-secret -geometry 40x10+300+40 -e sleep 600 \
    >"$test_dir/xterm-secret.log" 2>&1 &
secret_pid=$!
wait_for_managed_windows 2 || fail "nobox did not manage both test windows"
mapfile -t managed_windows < <(DISPLAY="$display" xprop -root _NET_CLIENT_LIST |
    sed 's/.*# //' | tr ',' '\n' | tr -d ' ' | grep '^0x')

# Structured observation: the world model an agent works from.
run_probe "$probe" snapshot "desktop snapshot"
grep -q 'class=XTerm' "$test_dir/probe-snapshot.log" ||
    fail "the snapshot did not describe the managed windows"
grep -q 'title=nobox-agent-visible' "$test_dir/probe-snapshot.log" ||
    fail "the snapshot did not carry window titles"
if grep -q 'nobox-agent-secret' "$test_dir/probe-snapshot.log"; then
    echo "a hidden window appeared in an agent snapshot" >&2
    cat "$test_dir/probe-snapshot.log" >&2
    exit 1
fi

# Semantic discovery runs outside the event loop and releases every helper
# outcome at the manager's fixed deadline. A concurrent snapshot must finish
# while that request is still pending.
"$semantic" "$socket" semantic-unavailable nobox-integration-probe \
    nobox-agent-visible >"$test_dir/probe-semantic-unavailable.log" 2>&1 &
semantic_pid=$!
sleep 0.1
"$probe" "$socket" snapshot nobox-integration-probe \
    >"$test_dir/probe-semantic-concurrent.log" 2>&1 ||
    fail "the manager blocked while semantic discovery was pending"
if ! kill -0 "$semantic_pid" 2>/dev/null; then
    fail "semantic discovery returned before its fixed deadline"
fi
wait "$semantic_pid" || fail "the bounded semantic discovery scenario failed"
semantic_pid=
grep -q 'semantic root failed closed' "$test_dir/probe-semantic-unavailable.log" ||
    fail "semantic discovery did not return the generic unavailable result"

# Every helper is disposable. Crashes, truncated JSON, and output beyond the
# manager's hard cap must be byte-equivalent failures, and none may poison the
# worker that launches the next helper. Human activity cancels work without
# creating a new public error. Disconnect, freeze, and revocation each discard
# the pending helper through their own live session paths.
if [[ -n "$semantic_helper_mode" ]]; then
    for mode in crash truncate oversize; do
        printf '%s\n' "$mode" >"$semantic_helper_mode"
        if ! "$semantic" "$socket" semantic-unavailable nobox-integration-probe \
            nobox-agent-visible >"$test_dir/probe-semantic-$mode.log" 2>&1; then
            fail "semantic helper mode $mode escaped the generic failure boundary"
        fi
        grep -q 'semantic root failed closed' "$test_dir/probe-semantic-$mode.log" ||
            fail "semantic helper mode $mode returned a distinct public result"
    done

    printf '%s\n' matched >"$semantic_helper_mode"
    run_probe "$semantic" semantic-once "semantic helper recovery" nobox-agent-visible
    grep -q 'semantic helper recovered with one bounded root' \
        "$test_dir/probe-semantic-once.log" ||
        fail "a valid helper did not recover after process-boundary failures"

    printf '%s\n' unavailable >"$semantic_helper_mode"
    "$semantic" "$socket" semantic-unavailable nobox-integration-probe \
        nobox-agent-visible >"$test_dir/probe-semantic-human.log" 2>&1 &
    semantic_pid=$!
    sleep 0.1
    DISPLAY="$display" "$helpers/press-key" --plain s >/dev/null 2>&1 || true
    wait "$semantic_pid" || fail "human cancellation escaped semantic failure equivalence"
    semantic_pid=
    grep -q 'semantic root failed closed' "$test_dir/probe-semantic-human.log" ||
        fail "human cancellation changed the semantic public result"

    "$semantic" "$socket" semantic-unavailable nobox-integration-probe \
        nobox-agent-visible >"$test_dir/probe-semantic-disconnect.log" 2>&1 &
    semantic_pid=$!
    sleep 0.1
    kill "$semantic_pid" 2>/dev/null || true
    wait "$semantic_pid" 2>/dev/null || true
    semantic_pid=
    printf '%s\n' matched >"$semantic_helper_mode"
    run_probe "$semantic" semantic-once "semantic recovery after disconnect" \
        nobox-agent-visible

    printf '%s\n' unavailable >"$semantic_helper_mode"
    "$semantic" "$socket" semantic-frozen nobox-integration-probe \
        nobox-agent-visible >"$test_dir/probe-semantic-frozen.log" 2>&1 &
    semantic_pid=$!
    for _ in $(seq 1 50); do
        if grep -q '^ready' "$test_dir/probe-semantic-frozen.log"; then break; fi
        sleep 0.1
    done
    grep -q '^ready' "$test_dir/probe-semantic-frozen.log" ||
        fail "the semantic freeze probe never reached its request boundary"
    sleep 0.1
    DISPLAY="$display" "$helpers/press-key" --control --alt Escape \
        >/dev/null 2>&1 || true
    wait "$semantic_pid" || fail "freezing did not terminate pending semantic work"
    semantic_pid=
    grep -q 'SessionFrozen' "$test_dir/probe-semantic-frozen.log" ||
        fail "pending semantic work did not carry the freeze decision"

    "$semantic" "$socket" semantic-revoked nobox-integration-probe \
        nobox-agent-visible >"$test_dir/probe-semantic-revoked.log" 2>&1 &
    semantic_pid=$!
    for _ in $(seq 1 50); do
        if grep -q '^ready' "$test_dir/probe-semantic-revoked.log"; then break; fi
        sleep 0.1
    done
    grep -q '^ready' "$test_dir/probe-semantic-revoked.log" ||
        fail "the semantic revocation probe never reached its request boundary"
    sleep 0.1
    python3 - "$test_dir/config.toml" "$semantic" <<'STRIP'
import sys

path, executable = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as stream:
    blocks = stream.read().split("[[agent.grants]]")
kept = [blocks[0]] + [block for block in blocks[1:] if executable not in block]
with open(path, "w", encoding="utf-8") as stream:
    stream.write("[[agent.grants]]".join(kept))
STRIP
    kill -HUP "$nobox_pid"
    wait "$semantic_pid" || fail "revocation did not terminate pending semantic work"
    semantic_pid=
    grep -q 'SessionRevoked' "$test_dir/probe-semantic-revoked.log" ||
        fail "pending semantic work did not carry the revocation decision"
fi

# The hidden window must answer exactly as a window that never existed.
run_probe "$probe" hidden-oracle "hidden client oracle" "${managed_windows[@]}"
grep -q 'withheld 1 of' "$test_dir/probe-hidden-oracle.log" ||
    fail "the hidden window was not withheld exactly once"

# The event stream: subscribe atomically, then follow a window through its
# whole life. The probe checks sequence monotonicity itself, and the scoped
# session additionally fails if it is ever told about a window outside its
# scope.
"$probe" "$socket" watch nobox-integration-probe nobox-agent-watched \
    >"$test_dir/probe-watch.log" 2>&1 &
watch_pid=$!
"$scoped" "$socket" watch nobox-integration-probe nobox-agent-watched \
    >"$test_dir/probe-scoped.log" 2>&1 &
scoped_pid=$!
sleep 0.5
DISPLAY="$display" xterm -title nobox-agent-watched -geometry 30x8+400+300 -e sleep 600 \
    >"$test_dir/xterm-watched.log" 2>&1 &
watched_xterm=$!
wait_for_managed_windows 3 || fail "nobox did not manage the watched window"
sleep 0.4
kill "$watched_xterm" 2>/dev/null || true
watched_xterm=
watch_status=0
wait "$watch_pid" || watch_status=$?
watch_pid=
scoped_status=0
wait "$scoped_pid" || scoped_status=$?
scoped_pid=
if [[ "$watch_status" -ne 0 ]]; then
    echo "the event stream did not describe the window's life" >&2
    cat "$test_dir/probe-watch.log" >&2
    exit 1
fi
if [[ "$scoped_status" -ne 0 ]]; then
    echo "the scoped event stream leaked or missed events" >&2
    cat "$test_dir/probe-scoped.log" >&2
    exit 1
fi
grep -q 'mapped .* title=nobox-agent-watched' "$test_dir/probe-watch.log" ||
    fail "the stream did not carry the mapped window's descriptor"
grep -q 'watched window appeared and went away' "$test_dir/probe-watch.log" ||
    fail "the stream did not report the window closing"
# The scoped session subscribed while two other windows were already managed
# and must have seen neither of them: its scope names a window that does not
# exist yet, so its snapshot is empty and its stream begins when that window
# appears.
grep -q 'subscribed .* clients=0 ' "$test_dir/probe-scoped.log" ||
    fail "the scoped session's snapshot was not restricted to its scope"
grep -q 'mapped .* title=nobox-agent-watched' "$test_dir/probe-scoped.log" ||
    fail "the scoped session did not receive its own window's events"

# Management: cross-workspace activation, the freshness contract, and a
# negotiated close, all through the manager's ordinary action paths.
DISPLAY="$display" xterm -title nobox-agent-managed -geometry 30x8+200+200 -e sleep 600 \
    >"$test_dir/xterm-managed.log" 2>&1 &
managed_pid=$!
wait_for_managed_windows 3 || fail "nobox did not manage the window to drive"
sleep 0.3
run_probe "$manager" manage "window management" nobox-agent-managed
grep -q 'activated across a workspace boundary' "$test_dir/probe-manage.log" ||
    fail "activation did not cross the workspace boundary"
grep -q 'stale_state -> re-observe at generation' "$test_dir/probe-manage.log" ||
    fail "a stale precondition was not refused with the current generation"
grep -q 'the window closed through its own protocol' "$test_dir/probe-manage.log" ||
    fail "the negotiated close did not close the window"
managed_pid=
# Put the desktop back where the rest of the test expects it.
"$manager" "$socket" workspace-home nobox-integration-probe \
    >"$test_dir/probe-workspace-home.log" 2>&1 ||
    fail "the desktop could not be returned to its first workspace"

# Window-addressed input: injected against the window's live geometry, marked
# by the manager while it happens, and reported step by step.
DISPLAY="$display" "$helpers/agent-input-client" nobox-agent-input \
    >"$test_dir/input-client.log" 2>&1 &
input_client_pid=$!
wait_for_managed_windows 3 || fail "nobox did not manage the input client"
sleep 0.4
run_probe "$driver" input "window-addressed input" nobox-agent-input
grep -q 'clicked, committed' "$test_dir/probe-input.log" ||
    fail "the pointer injection did not commit"
grep -q 'a point outside the window was refused' "$test_dir/probe-input.log" ||
    fail "a point outside the window was not refused"
delivered=
for _ in $(seq 1 40); do
    if grep -q 'button 1 at 40,24' "$test_dir/input-client.log" &&
        grep -q 'key h text h' "$test_dir/input-client.log" &&
        grep -q 'key i text i' "$test_dir/input-client.log" &&
        grep -q 'key at text @' "$test_dir/input-client.log" &&
        grep -q 'key Return text ' "$test_dir/input-client.log" &&
        grep -q 'key t text t' "$test_dir/input-client.log" &&
        grep -q 'paste Blåbærgrøt – mañana' "$test_dir/input-client.log" &&
        grep -q 'paste-followup ' "$test_dir/input-client.log"; then
        delivered=yes
        break
    fi
    sleep 0.1
done
if [[ -z "$delivered" ]]; then
    echo "the injected input did not arrive inside the target window" >&2
    cat "$test_dir/input-client.log" >&2
    exit 1
fi
python3 - "$test_dir/input-client.log" <<'CHECK_TEXT' || fail "paced multiline text did not arrive once and in order"
import sys

actual = []
with open(sys.argv[1], encoding="utf-8") as stream:
    for line in stream:
        if not line.startswith("key "):
            continue
        symbol, text = line.removeprefix("key ").rstrip("\n").split(" text ", 1)
        if symbol in {"Shift_L", "Control_L"}:
            continue
        actual.append((symbol, text))
expected = [
    ("h", "h"), ("i", "i"), ("at", "@"), ("Return", ""),
    ("s", "s"), ("l", "l"), ("o", "o"), ("w", "w"),
    ("space", " "), ("t", "t"), ("e", "e"), ("x", "x"), ("t", "t"),
]
if actual != expected:
    raise SystemExit(f"expected {expected!r}, received {actual!r}")

with open(sys.argv[1], encoding="utf-8") as stream:
    pasted = [line.removeprefix("paste ").rstrip("\n")
              for line in stream if line.startswith("paste ")]
if not any(value == "q" * 5000 for value in pasted):
    raise SystemExit("the 5000-character exact-text transfer did not arrive")
CHECK_TEXT
log_contains 'agent request served.*tool="client.pointer"' ||
    fail "the pointer injection was not attributed in tracing"
grep -q 'typed exact Unicode text, committed' "$test_dir/probe-input.log" ||
    fail "the exact Unicode text transfer did not commit"
grep -q 'typed 5000 ASCII characters, committed' "$test_dir/probe-input.log" ||
    fail "text beyond the old 4096-byte limit did not commit"

# Capture: stamped pixels, and refusal of the capture that would show a
# window the user marked sensitive.
run_probe "$camera" capture "window capture" nobox-agent-input
grep -q 'captured .* sequence' "$test_dir/probe-capture.log" ||
    fail "the window capture did not return stamped pixels"
grep -q 'a non-zero-origin crop returned its own pixels' "$test_dir/probe-capture.log" ||
    fail "a non-zero-origin crop did not return the pixels named by its metadata"
grep -q 'captured the frame as' "$test_dir/probe-capture.log" ||
    fail "the frame capture did not differ from the content capture"
grep -q 'output capture refused' "$test_dir/probe-capture.log" ||
    fail "an output capture was allowed while a hidden window was displayed"

# Nothing can capture a window that is not rendered anywhere, and the manager
# says so rather than returning the wrong pixels.
run_probe "$manager" minimize "minimize a window" nobox-agent-input
run_probe "$camera" capture-unrendered "unrendered window capture" nobox-agent-input
grep -q 'unrendered capture refused' "$test_dir/probe-capture-unrendered.log" ||
    fail "capturing an unrendered window was not refused"
run_probe "$manager" restore "restore a window" nobox-agent-input

# A covered window is a separate capability and needs a compositing server.
# The manager either does it or says exactly why it cannot.
run_probe "$manager" cover "cover a window" nobox-agent-visible nobox-agent-input
run_probe "$camera" capture-covered "covered window capture" nobox-agent-input
grep -qE 'captured a covered window|covered capture unsupported here' \
    "$test_dir/probe-capture-covered.log" ||
    fail "the covered capture neither succeeded nor said why not"
grep -E 'captured a covered window|covered capture unsupported here' \
    "$test_dir/probe-capture-covered.log"

# Target-owned Composite pixels can still show the lower client, but pointer
# and keyboard calls must follow the live interactive owner. A fresh snapshot
# is the recovery boundary; no input may be sent to either client on refusal.
input_lines_before=$(wc -l <"$test_dir/input-client.log")
run_probe "$driver" input-covered "covered input refusal" \
    nobox-agent-input nobox-agent-visible
sleep 0.1
input_lines_after=$(wc -l <"$test_dir/input-client.log")
[[ "$input_lines_after" -eq "$input_lines_before" ]] ||
    fail "covered input injected events before refusing"
grep -q 'covered pointer and unfocused key were refused before injection' \
    "$test_dir/probe-input-covered.log" ||
    fail "covered input did not return the observation-retry boundary"

# The human wins: input during the suppression window is refused, and the
# manager never counts its own injections as human activity. A long write is
# also preemptible between its paced character strokes.
"$driver" "$socket" text-interrupted nobox-integration-probe nobox-agent-input \
    >"$test_dir/probe-text-interrupted.log" 2>&1 &
text_interrupt_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^ready' "$test_dir/probe-text-interrupted.log"; then break; fi
    sleep 0.1
done
grep -q '^ready' "$test_dir/probe-text-interrupted.log" ||
    fail "the paced text probe never became ready"
sleep 0.1
DISPLAY="$display" "$helpers/press-key" --plain z >/dev/null 2>&1 || true
text_interrupt_status=0
wait "$text_interrupt_pid" || text_interrupt_status=$?
text_interrupt_pid=
if [[ "$text_interrupt_status" -ne 0 ]]; then
    echo "the paced text interruption failed" >&2
    cat "$test_dir/probe-text-interrupted.log" >&2
    exit 1
fi
grep -q 'text interrupted after a committed prefix' \
    "$test_dir/probe-text-interrupted.log" ||
    fail "paced text did not report its committed prefix"
run_probe "$driver" interrupted "human preemption" nobox-agent-input
grep -q 'interrupted, committed' "$test_dir/probe-interrupted.log" ||
    fail "agent input was not preempted by human input"

# The kill chord freezes every session ahead of any agent traffic, and
# freezing is not revocation.
"$driver" "$socket" freeze nobox-integration-probe >"$test_dir/probe-freeze.log" 2>&1 &
freeze_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^ready' "$test_dir/probe-freeze.log"; then break; fi
    sleep 0.1
done
grep -q '^ready' "$test_dir/probe-freeze.log" || fail "the freeze probe never subscribed"
sleep 1.6
DISPLAY="$display" "$helpers/press-key" --control --alt Escape >/dev/null 2>&1 || true
for _ in $(seq 1 50); do
    if grep -q '^refused while frozen' "$test_dir/probe-freeze.log"; then break; fi
    sleep 0.1
done
grep -q '^frozen' "$test_dir/probe-freeze.log" ||
    fail "the kill chord did not freeze the live session"
DISPLAY="$display" "$helpers/press-key" --control --alt Escape >/dev/null 2>&1 || true
freeze_status=0
wait "$freeze_pid" || freeze_status=$?
freeze_pid=
if [[ "$freeze_status" -ne 0 ]]; then
    echo "the freeze and resume lifecycle failed" >&2
    cat "$test_dir/probe-freeze.log" >&2
    exit 1
fi
grep -q '^served after resume' "$test_dir/probe-freeze.log" ||
    fail "the grant did not survive the freeze"
log_contains 'agent sessions changed by the kill chord' ||
    fail "the kill chord was not recorded"

# A selected user-installed entry remains refused until the separate
# user-entry switch is enabled.
run_probe "$launcher" launch-denied "user desktop entry default" \
    nobox-agent-launched.desktop
grep -q 'launch refused' "$test_dir/probe-launch-denied.log" ||
    fail "a selected user-installed entry launched while user entries were disabled"
python3 - "$test_dir/config.toml" <<'USER_ENTRIES'
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as stream:
    source = stream.read()
old = "user_entries = false"
if source.count(old) != 1:
    raise SystemExit("expected exactly one disabled user-entry switch")
with open(path, "w", encoding="utf-8") as stream:
    stream.write(source.replace(old, "user_entries = true"))
USER_ENTRIES
kill -HUP "$nobox_pid"
sleep 0.1

# Launching: only from the catalog, only what policy allows, and the window
# that appears carries the token of the launch that produced it. The allowed
# and forbidden fixtures share the now-enabled user applications directory,
# so list membership is the deciding gate.
run_probe "$launcher" launch "desktop entry launch" nobox-agent-launched.desktop \
    nobox-agent-forbidden.desktop
grep -q 'launch refused' "$test_dir/probe-launch.log" ||
    fail "a launch outside the policy was allowed"
grep -q 'correlated .* to the launch' "$test_dir/probe-launch.log" ||
    fail "the launched window did not carry its correlation token"

# Consent: with no stored grant, a person answers, and the answer is what the
# session gets.
"$asker" "$socket" consent nobox-integration-probe denied \
    >"$test_dir/probe-consent-denied.log" 2>&1 &
consent_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^asked' "$test_dir/probe-consent-denied.log"; then break; fi
    sleep 0.1
done
sleep 0.5
DISPLAY="$display" "$helpers/press-key" --plain n >/dev/null 2>&1 || true
consent_status=0
wait "$consent_pid" || consent_status=$?
consent_pid=
if [[ "$consent_status" -ne 0 ]]; then
    echo "the denied consent flow failed" >&2
    cat "$test_dir/probe-consent-denied.log" >&2
    exit 1
fi
grep -q 'answered granted=$' "$test_dir/probe-consent-denied.log" ||
    fail "a denied consent still granted something"
log_contains 'the human answered an agent consent request' ||
    fail "the consent answer was not recorded"

"$asker" "$socket" consent nobox-integration-probe granted \
    >"$test_dir/probe-consent-granted.log" 2>&1 &
consent_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^asked' "$test_dir/probe-consent-granted.log"; then break; fi
    sleep 0.1
done
sleep 0.5
DISPLAY="$display" "$helpers/press-key" --plain p >/dev/null 2>&1 || true
consent_status=0
wait "$consent_pid" || consent_status=$?
consent_pid=
if [[ "$consent_status" -ne 0 ]]; then
    echo "the granted consent flow failed" >&2
    cat "$test_dir/probe-consent-granted.log" >&2
    exit 1
fi
grep -q 'answered granted=observe.structure,observe.titles' \
    "$test_dir/probe-consent-granted.log" ||
    fail "consent did not grant what was asked for"
# Remembering the answer writes it where the user can see and undo it.
grep -q "$asker" "$test_dir/config.toml" ||
    fail "the remembered grant was not stored in the configuration"
grep -q 'harness' "$test_dir/config.toml" &&
    fail "a declared name was stored as if it were a matching key"

# A grant taken away in configuration stops working now, not at the next
# connection.
"$launcher" "$socket" revoke nobox-integration-probe >"$test_dir/probe-revoke.log" 2>&1 &
revoke_pid=$!
for _ in $(seq 1 50); do
    if grep -q '^ready' "$test_dir/probe-revoke.log"; then break; fi
    sleep 0.1
done
grep -q '^ready' "$test_dir/probe-revoke.log" || fail "the revoke probe never subscribed"
python3 - "$test_dir/config.toml" "$launcher" <<'STRIP'
import sys

path, executable = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as stream:
    blocks = stream.read().split("[[agent.grants]]")
kept = [blocks[0]] + [block for block in blocks[1:] if executable not in block]
with open(path, "w", encoding="utf-8") as stream:
    stream.write("[[agent.grants]]".join(kept))
STRIP
kill -HUP "$nobox_pid"
revoke_status=0
wait "$revoke_pid" || revoke_status=$?
revoke_pid=
if [[ "$revoke_status" -ne 0 ]]; then
    echo "the live revocation flow failed" >&2
    cat "$test_dir/probe-revoke.log" >&2
    exit 1
fi
grep -q '^refused after revocation' "$test_dir/probe-revoke.log" ||
    fail "a revoked session was still served"
log_contains 'agent grants revoked by configuration' ||
    fail "the revocation was not recorded"

# Protocol faults end their own session and nothing else.
run_probe "$probe" version "version mismatch"
run_probe "$probe" no-hello "request before handshake"
run_probe "$probe" second-hello "repeated handshake"
run_probe "$probe" oversize "oversized frame"
run_probe "$probe" garbage "malformed frame"
run_probe "$probe" truncate "abandoned mid-frame"
run_probe "$probe" flood "request flood"
# A companion that stops reading its own responses is shed rather than allowed
# to slow the manager down.
log_contains 'disconnecting an agent session that stopped reading' ||
    fail "a session that stopped reading was not disconnected"

kill -0 "$nobox_pid" 2>/dev/null || fail "nobox died while serving agent sessions"

# The MCP companion, driven exactly as a stock harness would drive it.
if [[ -z "$companion_binary" ]]; then
    echo "the MCP companion was not built; skipping its section"
fi
if [[ -n "$companion_binary" ]]; then
cat >"$test_dir/mcp-input.jsonl" <<'REQUESTS'
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_snapshot","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-11-25","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":6,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
REQUESTS
if ! DISPLAY="$display" AGENT_SEAT_SOCKET="$socket" "$companion_binary" \
    <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-output.jsonl" \
    2>"$test_dir/mcp-stderr.log"; then
    echo "the MCP companion exited with a failure" >&2
    cat "$test_dir/mcp-stderr.log" >&2
    exit 1
fi
if ! python3 "$(dirname "$0")/agent-mcp-check.py" "$test_dir/mcp-output.jsonl"; then
    echo "the MCP companion did not behave as revision 2026-07-28 requires" >&2
    cat "$test_dir/mcp-output.jsonl" >&2
    exit 1
fi

# Discovery precedence is exact: an explicit path wins even over a broken
# environment value, and the environment wins even while the root binding is
# invalid. Without either override, only a live matching owner/root pair works.
DISPLAY="$display" "$helpers/agent-seat-owner" set-root /tmp/not-the-nobox-seat.sock
if ! DISPLAY="$display" AGENT_SEAT_SOCKET=/nonexistent/agent-seat.sock \
    "$companion_binary" --socket "$socket" <"$test_dir/mcp-input.jsonl" \
    >"$test_dir/mcp-explicit-output.jsonl" 2>"$test_dir/mcp-explicit-stderr.log" ||
    ! python3 "$(dirname "$0")/agent-mcp-check.py" \
        "$test_dir/mcp-explicit-output.jsonl"; then
    fail "an explicit Agent Seat socket did not win discovery precedence"
fi
if ! DISPLAY="$display" AGENT_SEAT_SOCKET="$socket" "$companion_binary" \
    <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-environment-output.jsonl" \
    2>"$test_dir/mcp-environment-stderr.log" ||
    ! python3 "$(dirname "$0")/agent-mcp-check.py" \
        "$test_dir/mcp-environment-output.jsonl"; then
    fail "AGENT_SEAT_SOCKET did not win over the root advertisement"
fi
if ! env -u AGENT_SEAT_SOCKET DISPLAY="$display" "$companion_binary" \
    <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-mismatch-output.jsonl" \
    2>"$test_dir/mcp-mismatch-stderr.log"; then
    fail "the companion crashed while rejecting mismatched discovery"
fi
if ! grep -q 'no live agent seat is advertised' "$test_dir/mcp-mismatch-output.jsonl"; then
    fail "the companion trusted mismatched owner/root advertisements"
fi
DISPLAY="$display" "$helpers/agent-seat-owner" set-root "$socket"
if ! env -u AGENT_SEAT_SOCKET DISPLAY="$display" "$companion_binary" \
    <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-root-output.jsonl" \
    2>"$test_dir/mcp-root-stderr.log" ||
    ! python3 "$(dirname "$0")/agent-mcp-check.py" "$test_dir/mcp-root-output.jsonl"; then
    fail "the companion did not discover the live selection-bound root advertisement"
fi
fi

# With nothing sensitive displayed, the same output capture must succeed, so
# the refusal above is a decision rather than a blanket failure.
kill "$secret_pid" 2>/dev/null || true
secret_pid=
for _ in $(seq 1 50); do
    if [[ "$(count_managed_windows)" -le 2 ]]; then break; fi
    sleep 0.1
done
run_probe "$camera" output-capture "unobstructed output capture"
grep -q 'captured the output as' "$test_dir/probe-output-capture.log" ||
    fail "an output capture was refused with nothing sensitive on screen"
grep -q 'captured an output crop at' "$test_dir/probe-output-capture.log" ||
    fail "an output-region capture did not return the requested pixels"

# The real test of isolation: after all of that, window management still works.
DISPLAY="$display" xterm -title nobox-agent-late -geometry 40x10+60+200 -e sleep 600 \
    >"$test_dir/xterm-late.log" 2>&1 &
late_pid=$!
wait_for_managed_windows 3 ||
    fail "nobox stopped managing windows after agent traffic"

DISPLAY="$display" xprop -root _AGENT_SEAT >/dev/null ||
    fail "the agent seat advertisement disappeared"

# Turning the seat off takes effect on reload, not at the next start.
python3 - "$test_dir/config.toml" off <<'TOGGLE'
import sys

path, state = sys.argv[1], sys.argv[2]
wanted = "true" if state == "on" else "false"
with open(path, encoding="utf-8") as stream:
    lines = stream.read().splitlines()
lines = [
    f"enabled = {wanted}" if line.strip() == "enabled = true" or
    line.strip() == "enabled = false" else line
    for line in lines
]
with open(path, "w", encoding="utf-8") as stream:
    stream.write("\n".join(lines) + "\n")
TOGGLE
kill -HUP "$nobox_pid"
for _ in $(seq 1 50); do
    if [[ ! -S "$socket" ]]; then break; fi
    sleep 0.1
done
[[ ! -S "$socket" ]] || fail "disabling the seat left its socket behind"
if DISPLAY="$display" xprop -root _AGENT_SEAT 2>/dev/null | grep -q 'agent-seat'; then
    fail "disabling the seat left it advertised"
fi

# No selection owner makes even a stale root property inert.
if DISPLAY="$display" "$helpers/agent-seat-owner" owner >/dev/null 2>&1; then
    fail "disabling the seat left its selection owned"
fi
DISPLAY="$display" "$helpers/agent-seat-owner" set-root /tmp/stale-agent-seat.sock
if [[ -n "$companion_binary" ]]; then
    if ! env -u AGENT_SEAT_SOCKET DISPLAY="$display" "$companion_binary" \
        <"$test_dir/mcp-input.jsonl" >"$test_dir/mcp-stale-output.jsonl" \
        2>"$test_dir/mcp-stale-stderr.log"; then
        fail "the companion crashed while ignoring a stale root advertisement"
    fi
    grep -q 'no live agent seat is advertised' "$test_dir/mcp-stale-output.jsonl" ||
        fail "the companion trusted a stale root advertisement with no owner"
fi
DISPLAY="$display" "$helpers/agent-seat-owner" delete-root

# A foreign provider wins atomically. Enabling Nobox's integrated seat refuses
# without touching that provider's selection or root advertisement.
foreign_socket="$test_dir/foreign-agent-seat.sock"
DISPLAY="$display" "$helpers/agent-seat-owner" hold "$foreign_socket" \
    >"$test_dir/foreign-owner.log" 2>"$test_dir/foreign-owner.err" &
provider_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/foreign-owner.log" ]]; then break; fi
    sleep 0.1
done
kill -0 "$provider_pid" 2>/dev/null || fail "the foreign Agent Seat owner did not start"

# Turning the configured seat back on while that owner is live must not replace
# it or expose Nobox's listener.
python3 - "$test_dir/config.toml" on <<'TOGGLE'
import sys

path, state = sys.argv[1], sys.argv[2]
wanted = "true" if state == "on" else "false"
with open(path, encoding="utf-8") as stream:
    lines = stream.read().splitlines()
lines = [
    f"enabled = {wanted}" if line.strip() == "enabled = true" or
    line.strip() == "enabled = false" else line
    for line in lines
]
with open(path, "w", encoding="utf-8") as stream:
    stream.write("\n".join(lines) + "\n")
TOGGLE
kill -HUP "$nobox_pid"
sleep 0.3
[[ ! -e "$socket" ]] || fail "Nobox opened a socket while another provider owned the screen"
[[ "$(DISPLAY="$display" "$helpers/agent-seat-owner" owner)" == \
    "$(sed -n '1p' "$test_dir/foreign-owner.log")" ]] ||
    fail "Nobox replaced the live foreign Agent Seat provider"
DISPLAY="$display" xprop -root _AGENT_SEAT | grep -qF "$foreign_socket" ||
    fail "Nobox altered the foreign provider's advertisement"

kill "$provider_pid"
wait "$provider_pid"
provider_pid=
kill -HUP "$nobox_pid"
for _ in $(seq 1 50); do
    if [[ -S "$socket" ]]; then break; fi
    sleep 0.1
done
[[ -S "$socket" ]] || fail "Nobox did not claim the seat after the foreign owner left"

# Forced selection loss disables only the seat. The replacement publishes its
# artifacts under a server grab, so Nobox must leave them untouched while it
# closes sessions, removes its socket, and continues managing windows.
DISPLAY="$display" "$helpers/agent-seat-owner" replace "$foreign_socket" \
    >"$test_dir/replacement-owner.log" 2>"$test_dir/replacement-owner.err" &
provider_pid=$!
for _ in $(seq 1 50); do
    if [[ -s "$test_dir/replacement-owner.log" && ! -S "$socket" ]]; then break; fi
    sleep 0.1
done
kill -0 "$nobox_pid" 2>/dev/null || fail "selection loss terminated the window manager"
[[ ! -S "$socket" ]] || fail "selection loss left Nobox accepting Agent Seat peers"
DISPLAY="$display" xprop -root _AGENT_SEAT | grep -qF "$foreign_socket" ||
    fail "selection-loss cleanup removed the replacement provider's advertisement"

DISPLAY="$display" xterm -title nobox-after-seat-loss -geometry 40x10+80+240 -e sleep 600 \
    >"$test_dir/xterm-after-seat-loss.log" 2>&1 &
post_loss_pid=$!
wait_for_managed_windows 4 ||
    fail "window management stopped after Agent Seat selection loss"

# A reload while the replacement remains must still refuse it. Once the
# replacement leaves, the same unchanged configuration can recover the seat.
kill -HUP "$nobox_pid"
sleep 0.3
[[ ! -e "$socket" ]] || fail "reload replaced a live Agent Seat provider"
kill "$provider_pid"
wait "$provider_pid"
provider_pid=
kill -HUP "$nobox_pid"
for _ in $(seq 1 50); do
    if [[ -S "$socket" ]]; then break; fi
    sleep 0.1
done
[[ -S "$socket" ]] || fail "the integrated seat did not recover after ownership became free"

# A killed manager leaves only stale property/socket residue: its X selection
# disappears with the connection, and the next Nobox claims ownership before
# removing its own dead socket path.
kill -KILL "$nobox_pid"
wait "$nobox_pid" 2>/dev/null || true
nobox_pid=
[[ -S "$socket" ]] || fail "the crash-residue fixture did not leave a stale socket"
if DISPLAY="$display" "$helpers/agent-seat-owner" owner >/dev/null 2>&1; then
    fail "the Agent Seat selection survived its X11 owner connection"
fi
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" XDG_DATA_HOME="$data_home" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" \
    "$nobox_binary" run --no-autostart >"$test_dir/nobox-restarted.log" 2>&1 &
nobox_pid=$!
for _ in $(seq 1 50); do
    if [[ -S "$socket" ]] && DISPLAY="$display" "$helpers/agent-seat-owner" owner \
        >/dev/null 2>&1; then break; fi
    sleep 0.1
done
kill -0 "$nobox_pid" 2>/dev/null || fail "Nobox did not restart over crash residue"
[[ -S "$socket" ]] || fail "Nobox did not replace its stale socket after claiming ownership"
DISPLAY="$display" xprop -root _AGENT_SEAT | grep -qF "$socket" ||
    fail "the restarted seat did not replace the stale root advertisement"

# A clean shutdown withdraws the seat and removes its socket.
DISPLAY="$display" XDG_RUNTIME_DIR="$runtime_dir" \
    NOBOX_CONFIG_FILE="$test_dir/config.toml" "$nobox_binary" --exit
for _ in $(seq 1 50); do
    if ! kill -0 "$nobox_pid" 2>/dev/null; then break; fi
    sleep 0.1
done
if kill -0 "$nobox_pid" 2>/dev/null; then
    fail "nobox did not exit"
fi
nobox_pid=
[[ ! -e "$socket" ]] || fail "the agent seat socket outlived the manager"
if DISPLAY="$display" "$helpers/agent-seat-owner" owner >/dev/null 2>&1; then
    fail "clean shutdown left the Agent Seat selection owned"
fi
if DISPLAY="$display" xprop -root _AGENT_SEAT 2>/dev/null | grep -q 'agent-seat'; then
    fail "clean shutdown left the Agent Seat root advertisement"
fi

echo "agent seat test passed on $display"
