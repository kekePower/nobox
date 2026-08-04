# Architecture

The central design rule is that X11 is a backend, not the window manager's
policy model. Nobox preserves one user-visible window-management contract; it
does not pretend X11 window management and Wayland compositing are symmetric.

```text
X11 server <──> nobox-x11 ──────┐
                                ├──> nobox-core ──> policy decisions
Wayland clients <──> compositor ┘          ^
                                           │
                                   validated configuration
```

`nobox-core` owns display-server-independent identities, functional client
roles, capabilities, decoration policy, geometry, focus order, and stacking
order. It must not import X11 or future Wayland types.

`nobox-x11` owns the X connection and converts protocol events into policy
operations. It is responsible for ICCCM/EWMH interoperability, passive input
grabs, X error handling, save-set lifecycle recovery, and frame/decoration
resources.

`nobox-config` owns one strict, versionable TOML schema. The autostart script is
kept separate because its executable shell format is already the clearest user
interface for that job.

`nobox` is deliberately thin: logging, CLI dispatch, config selection,
autostart, and backend startup.

## Shared policy contract

The core models nobox concepts, not protocol objects. A managed client may be
an X11 top-level window or a Wayland toplevel, but it is never an X atom,
`xcb_window_t`, `wl_surface`, or Smithay handle. The same rule applies to future
outputs, workspaces, seats, placement decisions, application rules, menus, and
session state.

Decoration extents are shared policy geometry, while the resources used to
realize them are backend-owned. The core can therefore reason about content and
outer geometry without knowing whether X11 frame windows or Wayland compositor
rendering produced those pixels.

Maximize state is likewise policy-owned and retains per-axis restore geometry.
The backend supplies the currently available area and realizes the resulting
content geometry, so future outputs or work-area changes do not require X11
state in the core.

The core should remain a deterministic state machine. Backends translate
external events into validated state transitions, ask the core for policy, and
apply the resulting decisions using their own protocol. The core does not open
devices, own event loops, allocate display-server resources, render pixels, or
send protocol messages. This keeps policy tests fast and lets both backends
share answers to user-visible questions without sharing protocol machinery.

Protocol hints are translated at the boundary. For example, ICCCM size hints
become protocol-neutral size constraints before entering the core. EWMH
window types and Motif hints become client roles, capabilities, and decoration
choices. EWMH restacking is applied by X11 and its observed result is
synchronized into core stacking state. A future Wayland backend should perform
the equivalent translation from xdg-shell and compositor state rather than
emulating X11 properties.

## Backend asymmetry and capabilities

`nobox-x11` remains a comparatively small controller for an existing X server.
A Wayland backend will also be the compositor and therefore owns outputs,
seats, input dispatch, rendering, surface lifecycles, and Wayland protocol
advertisement. Those responsibilities stay outside `nobox-core` even when they
produce state that policy consumes.

We do not force every backend feature into a lowest-common-denominator API.
Shared actions describe user intent; backends expose whether they can realize
that intent and may have protocol-specific implementation paths. When one
backend cannot provide a meaningful equivalent, the behavior is explicitly
unsupported or given a documented user-visible fallback instead of leaking a
fake X11 abstraction into Wayland.

Abstractions are added from demonstrated policy needs, not speculative parity.
X11 work continues first, but new X11 code must keep raw protocol types and I/O
inside `nobox-x11`. When the Wayland backend begins, any boundary that does not
fit both real implementations should be revised rather than preserved for API
stability.

## Verification boundary

- Pure policy transitions and invariants belong in `nobox-core` unit tests.
- ICCCM/EWMH behavior, X server ordering, and protocol races belong in nested
  X11 integration tests.
- Openbox regression programs are behavioral evidence, not core APIs; tests
  assert the resulting user-visible contract.
- Future Wayland protocol tests must exercise the same policy outcomes while
  independently validating compositor responsibilities.

## Invariants

- Protocol errors from misbehaving clients must not crash the manager.
- Unknown config keys fail validation.
- A client occurs at most once in focus and stacking state.
- All external dimensions are clamped to at least one pixel.
- No `unsafe` Rust is allowed in this workspace.
- Starting beside another X11 window manager fails rather than replacing it.
- Display-server handles and protocol messages never enter `nobox-core`.

## Why not Wayland yet?

Feature work is intentionally X11-first because it can be dogfooded and checked
directly against Openbox. A later Wayland backend can use Smithay for protocols,
rendering, input, and session/device integration while retaining nobox policy.
Wayland does influence the architectural boundary now, but it does not delay
X11 features or justify speculative compositor abstractions before they are
needed.
