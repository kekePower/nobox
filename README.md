# nobox

`nobox` is an experimental, Openbox-inspired X11 window manager written in
Rust. The immediate goal is a small, dependable X11 daily driver. A native
Wayland compositor may follow only after the window-management policy and its
Openbox compatibility tests are mature.

The current vertical slice is real but intentionally small. It can own an X11
screen, adopt existing top-level windows, manage newly mapped clients, honor
configure requests, track focus and stacking, publish basic EWMH properties,
draw configurable borders, and move or resize a window with Super + mouse.

## Try it safely

The recommended first run is inside a nested X server:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset dev --target test

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

Key chords use `C`, `A`, `S`, and `W` for Control, Alt, Shift, and Super,
followed by an X11 keysym name. Caps Lock and Num Lock are ignored when matching
bindings. Available actions currently are `execute`, `close`, and `exit`.

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
cmake --build --preset dev --target test
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

- `nobox-core`: protocol-neutral client, focus, stacking, and geometry state
- `nobox-config`: strict TOML config, defaults, validation, and XDG paths
- `nobox-x11`: X11 ownership, events, client management, and EWMH plumbing
- `nobox`: the small CLI/session executable

See [docs/architecture.md](docs/architecture.md) for the design boundaries and
[docs/x11-roadmap.md](docs/x11-roadmap.md) for the staged compatibility plan.

## License

GPL-2.0-only. We study Openbox's behavior and tests, but new code is written
independently in Rust.
