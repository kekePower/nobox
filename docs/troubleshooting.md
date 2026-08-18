# Troubleshooting nobox

Start with the read-only doctor for the backend you intend to run:

```sh
nobox --backend x11 doctor
nobox --backend wayland doctor --nested-x11
nobox --backend wayland doctor --tty
```

The nested Wayland form validates the host X display and GLES/Pixman path. The
TTY form validates config, runtime-directory ownership, libseat selection, DRM
card/render access, input discovery, and optional XWayland without claiming the
seat or opening a compositor socket.

## A Wayland login returns to the display manager

Choose the separate **nobox** X11 session as the fallback. From a text TTY run
the TTY doctor above. Common blockers are a runtime directory not owned by the
user, no active local logind/seatd session, inaccessible DRM/input devices, or
an output rule that names an unavailable mode. Do not test `run --tty` from
inside another graphical session; use the guarded procedure in
[wayland-hardware-acceptance.md](wayland-hardware-acceptance.md).

Some Mageia LightDM installations search only `/usr/share/xsessions` and run
the XDM `Xstartup`, `Xreset`, and `Xsession` scripts for every session. Native
Wayland has no `$DISPLAY` while these hooks run, so `sessreg` exits unsuccessfully
and LightDM rejects the session before Nobox starts. Install the example at
`/usr/share/doc/nobox/lightdm/90-nobox-wayland.conf.example` as a LightDM
configuration drop-in. Its helpers preserve the distribution hooks for X11
and skip only the X-specific paths for Wayland. Verify the effective result
with `lightdm --show-config` before logging out; restarting LightDM ends the
current graphical session.

Set `RUST_LOG=nobox_wayland=debug` only for a short diagnostic run. Logs redact
application titles, command strings, activation tokens, clipboard content,
Agent Seat payloads, and pixels by design.

## A client is blank or fails to start

Use the nested doctor to confirm both renderers. A failed DMA-BUF import is a
client/buffer problem when later SHM clients continue to render. Try a simple
SHM client, then inspect the client's own EGL/Vulkan diagnostics. Nobox does not
fall back to displaying stale or partially imported content.

If an X11 application is involved, confirm `[wayland].xwayland = true`, that
the build used `-DNOBOX_BUILD_XWAYLAND=ON`, and that `Xwayland` is installed.
XWayland failure should remove only X11 clients and recover; native clients
remaining healthy is expected.

## Input, clipboard, panel, or lock behavior

- Input-method globals are private and exist only when a strict
  `[wayland].input_method` argv is configured. Ordinary clients never receive
  the privileged manager.
- Clipboard ownership disappears when its source disconnects. Cross-XWayland
  bridging is available only while XWayland is healthy.
- The panel is optional. A replacement must publish readiness before the old
  panel exits; failure intentionally retains the working instance.
- A lock client that dies after lock acceptance leaves a black locked session.
  Start a new locker or end the compositor from another TTY; ordinary clients
  are deliberately not restored into an ambiguous security state.

## Agent Seat

Under Wayland, start the companion from a compositor-launched command so it
inherits `AGENT_SEAT_SOCKET`, or pass `--socket` explicitly. There is no X11
root-property fallback. Run `nobox-agent doctor` in that same environment to
distinguish discovery, peer identity, grant, and protocol-version failures.
