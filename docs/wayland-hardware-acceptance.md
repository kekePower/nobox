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

Before leaving the graphical session, its read-only inventory mode can confirm
the exact GPU and currently connected DRM connector names that the retained
record will contain:

```sh
tools/wayland-hardware-acceptance.sh --inventory
```

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

The automated portions retain exact GPU and initial connector identities,
inventory globals and each `wl_output`, run an SHM client, run
`glmark2-es2-wayland --validate`, deliberately submit a
non-importable DMA-BUF, and prove that a later SHM client still receives frame
callbacks. The recorder's isolated configuration enables XWayland, requires a
compositor-owned XWayland process to map an XTerm, and retains both native and
X11 clients across VT, suspend/resume, and output churn. It also verifies output
counts after unplug/replug, compositor liveness, clean runtime-socket removal,
the exact XWayland child generation's termination, and post-exit device
diagnostics.

The human checkpoints cover cursor visibility, VT switch, suspend/resume,
mixed scale and transform, a genuinely KMS-rejected two-output mode candidate,
and unplug during an Alacritty move/resize. A planner rejection is not accepted
as the KMS rollback proof: the compositor log must contain `KMS mode candidate
failed`. Any failed or skipped checkpoint leaves the record `IN PROGRESS` and
W4 remains incomplete.

## Incremental LightDM dogfood

A normal **nobox (Wayland)** LightDM login is useful before attempting the
guarded W4 record, but it does not replace that record. This reduced run does
not ask the tester to suspend the workstation, unplug displays, edit modes, or
operate a recorder from a second TTY. Keep the separate **nobox** X11 session
installed as the login-screen fallback, and keep remote access available until
VT switching has been observed on the machine.

For an incremental run, select **nobox (Wayland)** at LightDM, confirm both
outputs paint, move the pointer across both outputs, open a root menu on each
output, launch one native Wayland client and one XWayland client, and try one
Ctrl+Alt+F-key switch and return. If any step fails, remotely restart LightDM
and choose **nobox**; record the failure as dogfood evidence rather than marking
W4 accepted. Suspend/resume remains part of a future guarded release record and
may be performed on another suitable machine.

For the known Terminology 1.14/EFL 1.28 EGL incompatibility on this host, use
`ELM_ENGINE=wayland_shm terminology` as the native client and
`ELM_ENGINE=software_x11 terminology` as the XWayland client. These per-process
overrides keep the checkpoint deterministic without changing the session-wide
toolkit renderer.

Preliminary record (2026-08-18): LightDM successfully started a direct session
on Linux 6.18.39 with an NVIDIA GeForce GTX 1660 SUPER and connected DVI-D-1
and HDMI-A-1 outputs. The compositor stayed alive until LightDM was restarted
remotely. The run exposed a primary-output menu-placement bug, empty overlay
text caused by reversed render ordering, a server cursor painted below the
menu, missing themed cursor loading, missing direct Ctrl+Alt+F-key handling,
and autostart beginning before XWayland readiness. This is diagnostic evidence,
not a completed W4 record; the subsequent fixes require another LightDM run.

Second dogfood observation (2026-08-18): a later installed session again
started both 2560x1600 outputs and ran native Kitty and Zen (including browser
audio), but remained unsuitable for daily use. The compositor continuously
redrew both outputs at vblank and held roughly 64% CPU, with libinput reporting
26–44 ms event-processing delays. Kitty pointer selection and tab clicks were
unreliable. The seat used the US XKB default instead of the host's Norwegian
layout. GTK Settings was disconnected at its 257th cumulative
`wp_presentation.feedback` request even though earlier one-shot objects had
completed. Decorations were globally separated from client surfaces, allowing
Kitty to appear between the Settings titlebar and content. Menu text remained
visibly coarse, the cursor appearance was poor, and Ctrl+Alt+F-key switching
still failed. Version 0.2.67 stops idle vblank redraw chaining, makes pointer
motion explicitly damage the cursor, releases completed presentation slots,
interleaves each decoration with its own client surface, and adds explicit XKB
controls plus a canonical installed-theme cursor fallback. These are fixes
awaiting another installed-session observation, not acceptance claims. Menu
font quality and VT switching remain open defects.

The same session also ran XScreenSaver 6.15, but the daemon attached to
XWayland's synthetic `DISPLAY=:0` root rather than the native Wayland outputs.
It remained alive and reported its X11 idle state, but it is not an
`ext_session_lock_v1` client and therefore cannot serve as Nobox's secure
Wayland locker. Partial drawing or blanking through XWayland is compatibility
behavior, not lock acceptance; a native session-lock client is required for
that record.

On successful completion of the guarded record, retain its directory and add
the exact date, host, GPU, connectors, kernel, and path to the W4 evidence
paragraph in
[`wayland-roadmap.md`](wayland-roadmap.md). Only then may
`BackendCapabilities::direct_session` become true.
