# nobox

`nobox` is an experimental, Openbox-inspired X11 window manager written in
Rust. The immediate goal is a small, dependable X11 daily driver. A native
Wayland compositor may follow only after the window-management policy and its
Openbox compatibility tests are mature.

The current vertical slice is real but intentionally small. It can own an X11
screen, adopt existing top-level windows, manage newly mapped clients, honor
configure requests, track focus and stacking, publish basic EWMH properties,
own the ICCCM window-manager selection, draw crash-safe reparenting frames with
configurable titles, minimize/maximize/close buttons, and move or resize a
window with Super + mouse. It also provides named workspaces with EWMH pager
interoperability, sticky clients, window moves, and independent focus history.
New unpositioned clients use deterministic least-overlap smart placement;
explicit ICCCM positions are preserved, and dialogs center over their parents.
Taskbar/pager visibility and urgency hints update live; urgent clients use a
distinct theme palette, and taskbar-skipped clients stay out of Alt+Tab.
RandR monitors are selected through shared output policy for placement,
maximize, fullscreen, per-monitor struts, and safe recovery after disconnects;
servers without RandR retain a single-root fallback.

## Try it safely

The recommended first run is inside a nested X server:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset test

Xnest :2 -geometry 1280x800 -ac &
DISPLAY=:2 ./build/dev/cargo/debug/nobox
DISPLAY=:2 xterm &
```

Hold Super and drag with the left mouse button to move a window. Super + right
drag resizes it, and Escape cancels either operation. Both operations snap to
work-area edges using the configurable mouse resistance. The initial keyboard
actions include Alt+Tab/Alt+Shift+Tab to cycle windows, Super+Return to start
`xterm`, Super+Q to close the focused client, Super+Left/Right to switch
workspaces, Super+Shift+Left/Right to move the focused window, and
Super+Shift+Escape to exit nobox. Do not replace your daily Openbox session
with this milestone yet: menus, session management, and substantial
ICCCM/EWMH behavior remain.

## Configure

`nobox` uses one strict TOML file plus the intentionally simple Openbox-style
autostart script:

```sh
cargo run -p nobox -- init
$EDITOR ~/.config/nobox/config.toml
$EDITOR ~/.config/nobox/autostart
cargo run -p nobox -- check
```

Unknown configuration keys are errors instead of silently ignored typos. If no
file exists, the built-in defaults are used. `NOBOX_CONFIG_FILE` and `--config`
make isolated tests easy.

The `[mouse]` table keeps move/resize buttons and `edge_resistance` together.
Resistance is measured in pixels and may be set to zero to disable magnetic
work-area edge snapping.

The `[placement]` table controls smart initial placement. Nobox scores
decorated outer rectangles on a grid formed by existing window and work-area
edges. `center_free_space = true` centers a window within the first completely
free field. ICCCM user/program positions are honored, existing windows adopted
at startup are not moved, and dialogs or splashes follow Openbox-style
parent/work-area centering.

The `[workspaces]` `names` array is deliberately the only workspace-count
setting: four names mean four workspaces. Names and count reload in place, and
clients on removed workspaces move to the final survivor. `columns = 0` uses a
single row; a positive column count creates a rectangular grid, and `wrap`
controls navigation at its edges. A standards-compliant EWMH pager that owns
the desktop-layout selection may override the visible grid while it is active.

Ordered `[[applications]]` rules match a newly managed client's X11 instance
name, class, role, title, and functional kind. Text patterns are
case-insensitive and support `*` and `?`; every field in `match` must match.
Later matching rules override only the settings they specify. Rules can select
an initial one-based workspace, `below`/`normal`/`above` layer, decorations,
and initial focus behavior. They affect initial management rather than pinning
the client against later user actions. The shipped config contains a commented
example.

Send `SIGHUP` to a running nobox process to validate and reload the effective
TOML file in place. Invalid replacements are diagnosed and the active config is
kept. `SIGINT` and `SIGTERM` request a clean event-loop shutdown, including
releasing input grabs and X11 ownership resources.

The theme schema includes border width, titlebar height,
focused/unfocused/urgent border and titlebar colors, title text, and button
colors. A titlebar height of zero explicitly disables the titlebar without
requiring a second theme file.

Key chords use `C`, `A`, `S`, and `W` for Control, Alt, Shift, and Super,
followed by an X11 keysym name. Space-separated chords form Openbox-style key
sequences such as `W-x W-t`; incomplete sequences time out and the configured
quit chord cancels them. Legacy singular `action` remains valid, while `actions`
runs an ordered list at a sequence leaf. Caps Lock and Num Lock are ignored when
matching bindings. Available actions include command execution, close/exit, absolute,
linear, or four-direction workspace switching, moving the focused client with
optional `follow = true` behavior, and forward/reverse window cycling. A focus
cycle snapshots visible, focusable clients in most-recently-used order while
its modifier remains held; modal families appear as their active focus target.
Focus assignment respects the ICCCM `WM_HINTS` input model and
`WM_TAKE_FOCUS` protocol. Client-requested and Super+right-drag resizing honor
ICCCM minimum/maximum sizes, base sizes, and resize increments.
Client resize requests also preserve the anchor described by window gravity.
Modal transients, including ICCCM window groups, receive focus and are raised
when an application tries to activate a blocked parent or group member.
Specific transient families move between workspaces together, inherit higher
parent layers, and remain stacked above their parents even after restacking or
relationship changes.
ICCCM iconic initial state and `WM_CHANGE_STATE` requests keep clients managed
while unmapped; activating an iconified client restores it normally. Genuine
minimization publishes EWMH hidden state, while off-workspace windows are not
misreported as minimized. Read-only EWMH focused state tracks the decorations
across direct focus, Alt+Tab, minimization, and workspace changes.
Client and pager restacking requests support all X11 stack modes while keeping
the EWMH stacking list synchronized with the server's actual order.
Framed clients publish `_NET_FRAME_EXTENTS`, retain content-root geometry across
configure requests, and are protected by the X save set if nobox terminates.
EWMH window types and Motif hints select per-client roles, capabilities, and
decorations; live hint changes update frames without remanaging the client, and
pre-map `_NET_REQUEST_FRAME_EXTENTS` estimates use the same policy.
UTF-8 and legacy X11 titles are mirrored onto frames and refresh live. The
minimize button uses the same ICCCM iconic/restore lifecycle as client requests.
Initial and runtime EWMH maximize requests support independent axes and preserve
exact restore geometry; the maximize button toggles both axes together.
Dock and panel struts update `_NET_WORKAREA` dynamically, reflow maximized
clients, and fall back from `_NET_WM_STRUT_PARTIAL` to legacy `_NET_WM_STRUT`.
Work areas are independent per workspace: sticky docks reserve every workspace,
while local docks affect only their assigned workspace.
Desktop and dock roles do not steal focus and occupy their default EWMH layers.
Fullscreen clients cover the complete output without decorations, stay above
docks, reject application geometry churn, and restore maximized or normal
geometry exactly. EWMH above/below requests are mutually exclusive and remain
within the core's deterministic desktop/below/normal/dock/above/fullscreen
stacking model.
EWMH skip-taskbar and skip-pager hints are honored both initially and at
runtime. Skip-taskbar clients are omitted from the MRU focus cycle. ICCCM
urgency and EWMH demands-attention share the urgent theme state; activation
clears demands-attention while leaving the client-owned ICCCM hint untouched.
Taskbars and pagers receive live EWMH allowed actions derived from the same core
capabilities used by nobox. Fixed-size clients do not advertise resize or
maximize, and fullscreen clients temporarily expose only meaningful actions.
Pager close requests use normal ICCCM `WM_DELETE_WINDOW` negotiation and policy
checks. Pager moveresize requests share ordinary client geometry handling,
including field masks, gravity anchoring, size constraints, and synthetic
configure notifications.

Useful commands:

```sh
cargo run -p nobox -- paths
cargo run -p nobox -- print-default
RUST_LOG=nobox=debug cargo run -p nobox -- run --display :2 --no-autostart
```

## Build, test, and install

CMake is the developer-facing build layer and uses Ninja presets. Cargo remains
the source of truth for Rust compilation and dependencies.

```sh
cmake --preset release
cmake --build --preset release
cmake --preset dev
cmake --build --preset check
cmake --build --preset test
cmake --install build/release --prefix ~/.local
```

The install includes `share/xsessions/nobox.desktop`, so a display manager can
offer a **nobox** X11 session after `~/.local/share/xsessions` is in its session
search path. A system-wide install normally uses `--prefix /usr` and appropriate
privileges.

Direct Cargo workflows remain available:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo install --path crates/nobox
```

## Workspace

- `nobox-core`: protocol-neutral roles, capabilities, focus, layers, work
  areas, workspaces, fullscreen state, stacking, and geometry
- `nobox-config`: strict TOML config, defaults, validation, and XDG paths
- `nobox-x11`: X11 ownership, events, client management, and EWMH plumbing
- `nobox`: the small CLI/session executable

See [docs/architecture.md](docs/architecture.md) for the design boundaries and
[docs/x11-roadmap.md](docs/x11-roadmap.md) for the staged compatibility plan.

## License

GPL-2.0-only. We study Openbox's behavior and tests, but new code is written
independently in Rust.
