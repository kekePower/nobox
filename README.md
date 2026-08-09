# nobox

**A small, predictable, Openbox-inspired X11 window manager written in Rust.**

nobox aims to be what Openbox has been for two decades: a lean, dependable
stacking window manager that stays out of your way — no desktop environment,
no compositor requirement, no toolkit in the core. Policy is protocol-neutral
by design, so a native Wayland compositor can follow later without making X11
the internal model.

## Highlights

- **Openbox-class window management** — reparenting frames, titlebars with
  minimize/maximize/close, Super/Alt + drag move and resize, edge snapping,
  and crash-safe client adoption and handoff.
- **Full ICCCM/EWMH interoperability** — pagers, taskbars, struts, urgency,
  window types, fullscreen, shading, and stacking behave the way existing
  tools expect. Openbox is the behavioral reference, with a
  [regression-by-regression compatibility matrix](docs/openbox-compatibility.md).
- **Named workspaces** with grids, per-workspace work areas, sticky clients,
  and independent focus history.
- **One strict TOML config file** plus an Openbox-style `autostart` script.
  Typos are errors, not surprises. Openbox 3 themes can be
  [imported directly](docs/configuration.md#importing-openbox-themes).
- **Menus without a toolkit** — nested popup menus defined in the same config
  file, XDG application discovery, live window/workspace menus, and
  command-generated menus with strict bounds.
- **Multi-monitor aware** — RandR output policy for placement, maximize,
  fullscreen, and struts, with safe recovery after disconnects.
- **Session support** — window-state save/restore, an optional XSMP
  companion, and Openbox-compatible remote exit for logout dialogs.
- **An agent seat, off by default** — nobox can offer an AI agent harness
  structured desktop state, a trustworthy event stream, window-addressed
  input, and consented capture through the
  [Agent Seat Protocol](docs/agent-protocol.md), with per-executable grants,
  windows you can hide from agents entirely, WM-drawn activity indicators, and
  a kill chord that always outranks agent traffic. Nobox Settings provides a
  searchable, deny-by-default picker for the installed applications a granted
  agent may launch. The person at the keyboard wins by construction.
- **Optional extras, never dependencies** — a GTK/libadwaita settings app, a
  Tint2-inspired panel, and the MCP companion run as separate processes; the
  window manager never links a UI toolkit.
- **No unsafe Rust.**

## Status

The first feature-complete X11 baseline is ready for deliberate daily-driver
dogfooding. The implementation is new, not historically proven: start in a
nested X server, then switch your real session once you trust it. The exact
scope, evidence, and intentional boundaries are recorded in
[docs/x11-acceptance.md](docs/x11-acceptance.md).

## Quick start

Try it safely inside a nested X server:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset test

Xnest :2 -geometry 1280x800 -ac &
DISPLAY=:2 ./build/dev/cargo/debug/nobox doctor
DISPLAY=:2 ./build/dev/cargo/debug/nobox
DISPLAY=:2 xterm &
```

`nobox doctor` is a read-only readiness check for the display, outputs, font,
and config. Nested servers often lack GLX, so use software-rendered clients
like `xterm` for smoke tests — see [docs/usage.md](docs/usage.md) for
details, default key bindings, and everyday controls.

A few defaults to get around:

| Input | Action |
|---|---|
| Super + left/right drag | Move / resize window |
| Alt+Tab | Cycle windows (overlay while held) |
| Super+Return | Terminal |
| Super+Left/Right | Switch workspace |
| Right-click desktop | Root menu |
| Double-click titlebar | Toggle maximize |

## Install

```sh
cmake --preset release
cmake --build --preset release
cmake --install build/release --prefix ~/.local
```

CMake with Ninja presets is the developer-facing build; Cargo remains the
Rust build and dependency layer underneath, and direct
`cargo install --path crates/nobox` works too. The install ships an
`xsessions` entry so display managers can offer a **nobox** session
(system-wide installs typically use `--prefix /usr`).

`cmake --install` only copies an existing build tree, so rebuild the release
preset after pulling changes before installing again. Build with the
**release** preset specifically: `--preset performance` builds only its
benchmark target, which is not everything an install needs. Installing an
incomplete tree stops with a message naming the missing binary rather than
copying half of one.

Optional components build automatically when their dependencies are present
and are omitted cleanly when they are not:

- `nobox-settings` — native settings app (GTK 4.10 + libadwaita 1.5); direct
  Cargo builds use `cargo build -p nobox-settings --features gui`.
- `nobox-xsmp` — XSMP session companion (`sm`/`ice` development files plus a
  C compiler); direct Cargo builds omit it, keeping `libSM`/`libICE` out of
  the Rust executable.
- `nobox-panel` — configurable Tint2-inspired EWMH panel with ordered
  components, application launchers, workspace/task controls, and a clock;
  disabled by default.
- `nobox-agent` — MCP companion for the [agent seat](docs/agent-protocol.md),
  installed to `bin` with its setup notes in `share/doc/nobox/nobox-agent.md`.
  Turn it off with `-DNOBOX_BUILD_AGENT=OFF`; the seat itself lives in the
  window manager either way and stays off until configuration enables it.
- `agent-semantic-helper` — disposable sandboxed AT-SPI translator, installed
  to `libexec/nobox`. Turn it off with
  `-DNOBOX_BUILD_SEMANTIC_HELPER=OFF`; semantic observation then fails closed
  without affecting the base agent seat.

An opt-in, reproducible performance comparison against the installed Openbox
is available via `cmake --build --preset performance`. It builds only what the
benchmark needs, so run it in addition to a release build rather than instead
of one; method and current numbers are in
[docs/performance.md](docs/performance.md).

## Configure

Everything lives in `~/.config/nobox/config.toml`:

```sh
nobox init                    # create a commented config
$EDITOR ~/.config/nobox/config.toml
nobox check                   # validate
```

Or use the optional `nobox-settings` GUI. Reload a running session with
`SIGHUP` or the session menu's **Reconfigure**. The complete reference —
themes, key/mouse bindings, menus, workspaces, application rules, and the
full action catalog — is in [docs/configuration.md](docs/configuration.md).

## Development

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check           # fmt + clippy -D warnings + tests
/usr/bin/ctest --preset dev --output-on-failure
```

The workspace is small and boundaries are deliberate:

- `nobox-core` — protocol-neutral policy: focus, stacking, workspaces,
  geometry, work areas
- `nobox-x11` — X11 ownership, events, client management, EWMH plumbing
- `nobox-config` — strict TOML model, validated format-preserving edits
- `nobox-desktop` — bounded XDG desktop-entry discovery and safe launching
- `nobox` — the thin CLI/session executable
- `nobox-settings`, `nobox-panel`, `nobox-xsmp` — optional separate processes

Contributions, bug reports, and dogfooding notes are welcome. Prefer small,
typed, testable changes; behavior changes come with focused unit or nested-X
regression coverage, and unsafe Rust is not used.

## Documentation

| Document | Contents |
|---|---|
| [docs/usage.md](docs/usage.md) | Everyday controls, menus, diagnostics, session control |
| [docs/configuration.md](docs/configuration.md) | Complete configuration and action reference |
| [docs/architecture.md](docs/architecture.md) | Design boundaries and crate responsibilities |
| [docs/agent-protocol.md](docs/agent-protocol.md) | The Agent Seat Protocol: what an agent may do, and why |
| [docs/agent-harness.md](docs/agent-harness.md) | Connecting an MCP agent harness, and keeping control of it |
| [docs/agent-seat-separation-roadmap.md](docs/agent-seat-separation-roadmap.md) | Nobox/product separation, Tier 0 readiness, and release gates |
| [docs/agent-seat-tier-complexity.md](docs/agent-seat-tier-complexity.md) | Why integrated Tier 1 is easier to secure than standalone Tier 0 |
| [docs/x11-behavior.md](docs/x11-behavior.md) | ICCCM/EWMH behavior details |
| [docs/x11-acceptance.md](docs/x11-acceptance.md) | Baseline scope and acceptance evidence |
| [docs/x11-roadmap.md](docs/x11-roadmap.md) | Staged compatibility plan |
| [docs/openbox-compatibility.md](docs/openbox-compatibility.md) | Per-fixture Openbox compatibility matrix |
| [docs/performance.md](docs/performance.md) | Reproducible Openbox comparison |
| [docs/client-side-decorations.md](docs/client-side-decorations.md) | GTK/Firefox CSD behavior under nobox |

## License

GPL-2.0-only. We study Openbox's behavior and tests, but new code is written
independently in Rust.
