# Using nobox

Day-to-day interaction with a nobox session: the default mouse and keyboard
controls, menus, diagnostics, and how to control, reload, and end a running
session. The complete configuration model behind these defaults is in
[configuration.md](configuration.md).

## Trying nobox in a nested X server

The recommended first run is inside a nested X server:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset test

Xnest :2 -geometry 1280x800 -ac &
DISPLAY=:2 ./build/dev/cargo/debug/nobox doctor
DISPLAY=:2 ./build/dev/cargo/debug/nobox
DISPLAY=:2 xterm &
```

The integration suite accepts `NOBOX_XSERVER=xnest`, `xephyr`, or `xvfb` to
select an installed server explicitly; `auto` retains the default fallback
order. Xephyr is useful for interactive testing, while Xvfb provides a
headless protocol test server.

This Xnest recipe is for safe window-management and protocol testing. Xnest
commonly does not expose GLX, so GPU-dependent clients can fail even though
nobox and ordinary X11 clients are working. Typical symptoms are
Electron/Chromium GPU-process warnings or kitty reporting that the GLX
extension is missing. Check a nested server before using those clients:

```sh
DISPLAY=:2 xdpyinfo | grep -q GLX && printf 'GLX available\n' || printf 'GLX unavailable\n'
```

Use `xterm` or another software-rendered client for the initial Xnest smoke
test. Test GL-dependent applications in a disposable real Xorg session, or in
a Xephyr/other nested server whose `xdpyinfo` output confirms GLX support.
GLX is a client rendering requirement, not a nobox requirement.

Start with a nested server and deliberate dogfooding before replacing a daily
Openbox session; the compatibility gate is broad, but the project has not yet
earned years of real-desktop exposure.

## Diagnostics: `nobox doctor`

`nobox doctor` is read-only: it validates the effective config and saved
session, then reports the X server, screen, outputs, configured font,
RandR/Shape/Sync availability, and any existing WM selection owner without
claiming events or changing the desktop. Missing optional extensions are
warnings with explicit fallbacks; an invalid config/session, unreachable
display, or unavailable font makes the command fail with `ready: no`.

For Wayland, `nobox --backend wayland doctor --nested-x11` checks the isolated
nested renderer path. `nobox --backend wayland doctor --tty` checks direct-session
prerequisites instead: the private runtime directory, selected seat/session,
DRM card and render nodes, input discovery, kernel access result, and optional
XWayland executable. This W4 diagnostic does not open libseat, DRM/input
devices, or a Wayland listening socket and therefore does not claim the active
desktop. A successful prerequisite report does not claim that the direct
backend has passed its hardware acceptance record.

The explicit `nobox --backend wayland run --tty` W4 bring-up path is intentionally
separate from nested development. It acquires the session through libseat,
opens DRM and libinput through that session, applies the typed `[outputs]`
policy across available connectors, initializes GBM/GLES without an unsafe
Nobox call site, serves the private Wayland socket, and schedules independently
damaged KMS frames from each output's vblank. It advertises linux-dmabuf v5
feedback from the actual render-node formats and linux-drm-syncobj v1 only when
the DRM device supports eventfd waits. Failed client imports are rejected or
omitted without ending the compositor. Pause suspends input and DRM; activation
resumes both without reconstructing compositor policy. In direct sessions,
Ctrl+Alt+F1 through Ctrl+Alt+F12 request the matching VT through libseat.
Autostart waits for the configured XWayland instance to become ready (bounded
to five seconds), then receives `WAYLAND_DISPLAY` plus the real `DISPLAY` when
available; XWayland-disabled or failed startup leaves `DISPLAY` unset.

Connector hotplug, multi-output transactions, compositor cursor fallback, and
configuration rollback have deterministic coverage, but the complete W4
hardware acceptance record remains outstanding. Run `--tty` only from a
dedicated or disposable graphical VT; do not invoke it from inside a desktop
whose DRM master must remain in use.

The exact guarded two-VT procedure and evidence boundary are in
[`wayland-hardware-acceptance.md`](wayland-hardware-acceptance.md).

The user-facing `nobox` command is a small selector. It defaults to the X11
backend for compatibility and forwards every argument unchanged to
`libexec/nobox/nobox-x11`; `--backend wayland` selects
`libexec/nobox/nobox-wayland`. The backend executables are independently
linked and may also be invoked directly for packaging or diagnosis. If the
selected backend package is absent, the selector fails explicitly instead of
falling back to a different display server.

A source install places distinct **nobox** and **nobox (Wayland)** entries in
the X11 and Wayland session directories. The Wayland entry always executes
`nobox --backend wayland run --tty`; it cannot silently redirect an X11 login.
If direct startup fails, return to the separate **nobox** entry and run the TTY
doctor from a text console. See [troubleshooting.md](troubleshooting.md).

The direct compositor loads the current XCursor theme from `XCURSOR_THEME` and
`XCURSOR_SIZE`, falling back to the system `default` theme and finally a small
server cursor if no usable theme image exists. Compositor menus and switchers
are always painted below that cursor.

## Mouse controls

Drag a titlebar with the left mouse button to move a window, or drag a border
to resize from that edge or corner. Super + left/right drag moves or resizes
from anywhere in the frame; traditional Alt + left/right drag works too.
Escape cancels either operation. Both operations snap to work-area edges using
the configurable mouse resistance.

Double-clicking a titlebar toggles maximize, middle-clicking lowers the
window, the desktop wheel changes workspaces, and right-clicking the root
opens the configured `root` menu. Right-clicking a titlebar opens its client
menu, middle-clicking the root opens the window list, and Alt+Space opens the
focused client's menu.

## Menus

Menus support pointer selection, wheel and arrow-key navigation, Home/End,
Enter, Left/Right submenu traversal, and Escape or an outside click to
dismiss. An underscore marks a keyboard accelerator; use two underscores for a
literal one.

## Keyboard controls

The initial keyboard actions include Alt+Tab/Alt+Shift+Tab to cycle windows,
with an on-screen list while Alt remains held and Escape to cancel.
Super+Return starts the configured terminal (`xterm` by default), Ctrl+Alt+T
is a traditional alias, Print takes a full-screen screenshot, and Alt+Print
captures the active window. Their commands and common aliases are editable in
Settings. Super+Q closes the focused client, Super+D toggles EWMH show-desktop
mode, Super+Left/Right switches workspaces, Super+Shift+Left/Right moves the
focused window, and Super+Shift+Escape exits nobox.

A focus cycle snapshots visible, focusable clients in most-recently-used order
while its modifier remains held; modal families appear as their active focus
target. Its backend-owned overlay contains only core-selected client titles;
modifier release commits the current target and Escape restores the snapshot's
original focus. Linear and spatial cycles share one bounded snapshot, keyboard
grab, overlay, and cleanup path. Skip-taskbar clients are omitted from the MRU
focus cycle.

Keyboard-driven move/resize (entered from a key binding or menu) uses
eight-pixel steps, Control for single-pixel adjustment, and Shift to jump to a
work-area edge; Enter commits and Escape cancels.

## Controlling a running session

- **Reload**: send `SIGHUP`, use the typed `reconfigure` action, or choose
  **Reconfigure** from the session menu. The file is validated first; invalid
  replacements are diagnosed and the active config is kept.
- **Restart**: the session menu's `restart` captures session state, releases
  X11 ownership, rebuilds the backend in the same process, and does not rerun
  autostart. An optional `command` replaces nobox after clean release,
  allowing an intentional handoff to another window manager.
- **Remote exit**: external session dialogs and logout tools can stop the
  running window manager with `nobox --exit`, matching Openbox's
  remote-control option. The request is sent only after verifying nobox's
  EWMH supporting window; the running process then saves session state, stops
  its optional panel, releases X11 ownership, and exits cleanly. `SIGINT` and
  `SIGTERM` request the same clean event-loop shutdown, including releasing
  input grabs and manager-owned X11 properties and selections.
- **Session logout**: the default session menu exposes `session_logout`. When
  `commands.session` is non-empty, the action launches that command directly
  so a dedicated dialog such as `ssdd` can own the available choices and
  confirmation. Otherwise its grabbed confirmation starts on **Cancel** and
  supports the normal pointer, arrow, accelerator, Enter, and Escape menu
  controls. After confirmation nobox asks the connected XSMP manager for a
  global interactive logout and remains alive until that manager cancels or
  sends `Die`. With no usable session manager it falls back to the same clean
  local exit as Openbox. Bindings may set `prompt = false` when a separate
  trusted confirmation layer already exists; the XSMP request itself remains
  interactive so applications may still participate in shutdown.
- **Exit**: the final, separated **Exit nobox** entry in the root menu releases
  only nobox and leaves session coordination alone. It uses the same
  cancel-first confirmation by default; `prompt = false` is the explicit
  immediate form.

## Session state

On a clean exit, nobox atomically saves bounded window-session state at
`$XDG_STATE_HOME/nobox/session.toml` (falling back to
`~/.local/state/nobox/session.toml`). `nobox paths` prints the effective path,
and `NOBOX_STATE_FILE` overrides it. When `NOBOX_CONFIG_FILE` is used for an
isolated environment, the default session file is placed beside that config.

The next run restores the current workspace plus matched clients' normal
geometry, workspace/sticky assignment, minimized/shaded/fullscreen/maximized
state, taskbar/pager visibility, layer, stacking order, and focus. Matching
prefers `SM_CLIENT_ID` and falls back to `WM_COMMAND`, combined with class,
instance, role, and type; ambiguous duplicate identities are deliberately not
restored. Clients without either stable identifier are omitted.

When `SESSION_MANAGER` is present, a CMake build with `libSM`/`libICE`
development files automatically starts the optional `nobox-xsmp` companion. It
registers the current/restart identity and process metadata, turns
`SaveYourself` into an in-place durable snapshot, honors save completion and
shutdown cancellation, and routes `Die` through the same clean X11 release
path as a signal or exit action. It also carries confirmed `session_logout`
requests out to the external manager rather than treating them as an alias for
killing the window manager. The companion is a separate process: ordinary X11
sessions neither start it nor add `libSM`/`libICE` to the Rust executable's
dependencies. Application relaunch remains the session manager and each
application's responsibility; the intentionally simple autostart script
remains nobox's startup mechanism.

## The agent seat

With `[agent].enabled` set, nobox offers a second seat on the session: an AI
agent harness can read structured desktop state, follow an event stream, act
on windows, inject window-addressed input, capture pixels, and start approved
applications — all through the window manager, with a grant the user issued.
`docs/agent-harness.md` walks through turning the seat on and connecting a
harness; `docs/agent-protocol.md` is the contract, and `docs/configuration.md`
covers the `[agent]` section.

Nobox exposes two visible agent indicators. A marker sits in the corner of
the primary output whenever a session holds input or capture, and a window
that receives agent input is highlighted in the theme's
`agent_marker` color during the action and for 1.5 seconds afterward. Both are
drawn by the manager, and nothing in the protocol can create, cover, target,
or dismiss either.

The person at the keyboard always wins. Any human input suppresses agent input
for `suppression_ms`; a call made during that window is refused and reports
exactly which of its steps had already committed. The kill chord — Control +
Alt + Escape by default — freezes every session immediately and resumes them
when pressed again. It is handled ahead of all agent traffic, so it works even
while a session is flooding the socket.

To connect a harness, point it at the `nobox-agent` companion, which speaks
MCP on stdio. It is built and installed with nobox unless the build turned it
off with `-DNOBOX_BUILD_AGENT=OFF`:

```json
{
  "mcpServers": {
    "nobox": { "command": "/usr/local/bin/nobox-agent" }
  }
}
```

The companion finds the seat from `--socket`, then `AGENT_SEAT_SOCKET`, then a
live selection-bound `_AGENT_SEAT` property on the selected X11 root. It never
synthesizes a Nobox filesystem path. A valid root value must match the property
on the current `_AGENT_SEAT_S<screen>` owner window:

```sh
xprop -root _AGENT_SEAT
```

The first connection is denied everything unless a grant names the
companion's executable, so with `policy = "ask"` the useful first step is to
let the harness connect and answer the dialog with `p` to store the grant.
