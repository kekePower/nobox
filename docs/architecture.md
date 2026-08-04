# Architecture

The central design rule is that X11 is a backend, not the window manager's
policy model.

```text
configuration ───────┐
                     v
X11 events ──> nobox-x11 ──> nobox-core ──> X11 requests
                     ^
                     └── focus, placement, stacking, actions
```

`nobox-core` owns display-server-independent identities, geometry, focus order,
and stacking order. It must not import X11 or future Wayland types.

`nobox-x11` owns the X connection and converts protocol events into policy
operations. It is responsible for ICCCM/EWMH interoperability, passive input
grabs, X error handling, and eventually frames/decorations.

`nobox-config` owns one strict, versionable TOML schema. The autostart script is
kept separate because its executable shell format is already the clearest user
interface for that job.

`nobox` is deliberately thin: logging, CLI dispatch, config selection,
autostart, and backend startup.

## Invariants

- Protocol errors from misbehaving clients must not crash the manager.
- Unknown config keys fail validation.
- A client occurs at most once in focus and stacking state.
- All external dimensions are clamped to at least one pixel.
- No `unsafe` Rust is allowed in this workspace.
- Starting beside another X11 window manager fails rather than replacing it.

## Why not Wayland yet?

The core boundary preserves the option, but feature work is intentionally
X11-first. A later Wayland backend can use Smithay for protocols, rendering,
input, and session/device integration while retaining nobox policy. It should
not influence milestone-one scope beyond keeping X11 types out of the core.
