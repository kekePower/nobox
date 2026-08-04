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
window with Super + mouse.

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
drag resizes it. The initial keyboard actions are Super+Return to start `xterm`,
Super+Q to close the focused client, and Super+Shift+Escape to exit nobox. Do
not replace your daily Openbox session with this milestone yet: desktops,
menus, decorations, session management, and most ICCCM/EWMH behavior remain to
be implemented.

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

The theme schema includes border width, titlebar height, focused/unfocused
border and titlebar colors, title text, and button colors. A titlebar height of
zero explicitly disables the titlebar without requiring a second theme file.

Key chords use `C`, `A`, `S`, and `W` for Control, Alt, Shift, and Super,
followed by an X11 keysym name. Caps Lock and Num Lock are ignored when matching
bindings. Available actions currently are `execute`, `close`, and `exit`.
Focus assignment respects the ICCCM `WM_HINTS` input model and
`WM_TAKE_FOCUS` protocol. Client-requested and Super+right-drag resizing honor
ICCCM minimum/maximum sizes, base sizes, and resize increments.
Client resize requests also preserve the anchor described by window gravity.
Modal transients, including ICCCM window groups, receive focus and are raised
when an application tries to activate a blocked parent or group member.
ICCCM iconic initial state and `WM_CHANGE_STATE` requests keep clients managed
while unmapped; activating an iconified client restores it normally.
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
Desktop and dock roles do not steal focus and occupy their default EWMH layers.

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

- `nobox-core`: protocol-neutral roles, capabilities, focus, stacking, and
  geometry
- `nobox-config`: strict TOML config, defaults, validation, and XDG paths
- `nobox-x11`: X11 ownership, events, client management, and EWMH plumbing
- `nobox`: the small CLI/session executable

See [docs/architecture.md](docs/architecture.md) for the design boundaries and
[docs/x11-roadmap.md](docs/x11-roadmap.md) for the staged compatibility plan.

## License

GPL-2.0-only. We study Openbox's behavior and tests, but new code is written
independently in Rust.
