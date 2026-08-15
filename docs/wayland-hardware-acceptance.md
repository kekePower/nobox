# Wayland W4 hardware acceptance

W4 is accepted only by a named record produced on a disposable local VT. The
nested suite proves policy and protocol behavior, but it cannot prove DRM
master ownership, libseat pause/resume, KMS rollback, connector churn, or input
device recovery. Do not run this procedure inside X11 or another Wayland
session.

The guarded recorder is `tools/wayland-hardware-acceptance.sh`. It refuses to
start unless all of these are true:

- `DISPLAY` and `WAYLAND_DISPLAY` are unset;
- the current logind session is an active, local, non-remote `tty` user session
  with a seat and VT number;
- the caller supplies the exact `disposable-vt` acknowledgement;
- two physical outputs are connected; and
- a new, explicit record directory is supplied, so prior evidence is never
  overwritten.

Build and pass the normal gate before leaving the graphical session:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
env NOBOX_XSERVER=xvfb /usr/bin/ctest --preset dev --output-on-failure
```

Then switch to a fresh local TTY, log in as the ordinary desktop user, and
attach a second output. The compositor will replace that VT's text display, so
start the recorder in a fresh tmux server that can be attached from another
local TTY. From the repository root on the disposable VT:

```sh
unset DISPLAY WAYLAND_DISPLAY
: "${XDG_STATE_HOME:?set XDG_STATE_HOME to an explicit state directory}"
record_dir="$XDG_STATE_HOME/nobox/wayland-w4-$(date +%Y%m%d-%H%M%S)"
tmux_socket="nobox-w4-$(date +%Y%m%d-%H%M%S)"
tmux -L "$tmux_socket" new-session -d -s nobox-w4 \
  "NOBOX_WAYLAND_HARDWARE_ACCEPTANCE=disposable-vt \
   tools/wayland-hardware-acceptance.sh \
   '$PWD/build/dev/cargo/debug/nobox' \
   '$PWD/build/dev/cargo/debug/nobox-wayland-probe' \
   '$record_dir'"
echo "recorder tmux socket: $tmux_socket"
```

When Nobox appears on that VT, switch to a second text VT, log in as the same
user, and attach to the recorder with
`tmux -L "$tmux_socket" attach -t nobox-w4` (substitute the printed socket name
if this is a separate shell). Use the compositor VT for visual and input checks
and the second VT for the recorder prompts. Use a fresh tmux socket as shown;
reusing a server launched from the active desktop would defeat the
logind-session safety boundary.

Detach from the recorder tmux client when a checkpoint asks you to edit its
generated `config.toml`, make the edit from the second TTY, and then reattach.
Do not edit the normal desktop configuration for this run.

If `XDG_STATE_HOME` is unset, set `record_dir` to another explicit directory
owned by the test user; do not use the repository's `tmp/`. The recorder never
suspends the machine, switches VTs, changes output configuration, or unplugs
hardware on its own. It pauses with exact instructions and records a step only
after the human types `PASS`.

The automated portions inventory globals and each `wl_output`, run an SHM
client, run `glmark2-es2-wayland --validate`, deliberately submit a
non-importable DMA-BUF, and prove that a later SHM client still receives frame
callbacks. They also verify output counts after unplug/replug, compositor
liveness, clean runtime-socket removal, and post-exit device diagnostics.

The human checkpoints cover cursor visibility, VT switch, suspend/resume,
mixed scale and transform, a genuinely KMS-rejected two-output mode candidate,
and unplug during an Alacritty move/resize. A planner rejection is not accepted
as the KMS rollback proof: the compositor log must contain `KMS mode candidate
failed`. Any failed or skipped checkpoint leaves the record `IN PROGRESS` and
W4 remains incomplete.

On success, retain the record directory and add its exact date, host, GPU,
connectors, kernel, and path to the W4 evidence paragraph in
[`wayland-roadmap.md`](wayland-roadmap.md). Only then may
`BackendCapabilities::direct_session` become true.
