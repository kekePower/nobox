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
order. It must not import X11 or future Wayland types. Workspaces are core
identifiers with per-workspace focus history; X11 desktop numbers and sticky
desktop values are translated only at the backend boundary.

`nobox-x11` owns the X connection and converts protocol events into policy
operations. It is responsible for ICCCM/EWMH interoperability, passive input
grabs, X error handling, save-set lifecycle recovery, and frame/decoration
resources.

`nobox-config` owns one strict, versionable TOML schema. The autostart script is
kept separate because its executable shell format is already the clearest user
interface for that job.

`nobox` is deliberately thin: logging, CLI dispatch, config selection,
autostart, and backend startup.

Unix signal handling also stays in the executable. A dedicated signal thread
translates `SIGHUP`, `SIGINT`, and `SIGTERM` into typed control events delivered
to the backend's manager-owned X11 support window. This wakes the blocking event loop
without polling. The CLI reloads and validates TOML; only a valid `Config`
crosses into `nobox-x11`, which applies it to existing resources in place.

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

Pointer move/resize edge snapping is also pure geometry policy. Backends supply
the active work area and input delta, while the core deterministically resolves
the snapped result. X11 owns grabs and event cancellation; future compositor
input handling can reuse the geometry without inheriting X11 grab semantics.

Maximize state is likewise policy-owned and retains per-axis restore geometry.
The backend supplies the currently available area and realizes the resulting
content geometry, so future outputs or work-area changes do not require X11
state in the core.

Fullscreen and stacking layers are policy-owned as well. The core retains
fullscreen restore geometry and resolves each role plus the user's requested
below/normal/above preference into an ordered desktop, below, normal, dock,
above, or fullscreen layer. X11 realizes that order with frame windows and EWMH
atoms; a Wayland compositor can realize the same contract with its scene graph.
Fullscreen uses the complete output rather than its reserved work area.

Specific transient relationships form policy families. Core state resolves
their cycle-safe ancestry, moves the family as one workspace unit, inherits a
higher parent layer, and emits a parent-before-child stacking order. X11 only
realizes that order with frame requests; it does not define family semantics.

Focus-cycle candidates are also policy-owned: the core returns a de-duplicated
most-recently-used list after workspace visibility, iconic state, focus
capability, task-list presentation, and modal redirection are applied. X11
retains a keyboard grab only for the modifier-held cycle and realizes each
selected core identifier through ICCCM focus negotiation. The X11 backend owns
one persistent override-redirect list window and maps it only during a retained
grab. It places that window through shared output selection, draws bounded title
rows with existing theme resources, commits on modifier release, and restores
the original core focus target on Escape. This keeps popup mechanics out of the
policy model while leaving candidate and cancellation semantics reusable by a
future compositor.

Keyboard configuration is parsed into validated sequences and ordered action
lists before reaching a backend. X11 resolves symbolic chords against the live
keyboard map and keeps only the currently valid sequence-prefix grabs active.
A single sleeping timer worker delivers generation-tagged X11 control events,
so incomplete chains expire without polling and stale timer events cannot
cancel a newer chain. Mapping changes and configuration reloads rebuild the
same typed tree and cancel any active sequence safely.

Mouse configuration is likewise parsed into validated context, chord, trigger,
and ordered-action types before reaching X11. The backend compiles those into a
bounded lookup map and installs one passive grab per unique modified button;
unmodified frame and client gestures use their existing event selections.
Press state retains only one small gesture record until release or the drag
threshold, while double-click history is one fixed-size record. Specific
buttons and resize edges fall through to titlebar, border, frame, or desktop
policy in the same useful order as Openbox. Only X11 target discovery and grabs
remain backend-specific; action targeting, stacking order, geometry constraints,
and workspace policy stay in the shared model.

Task-list visibility, pager visibility, and effective urgency are
protocol-neutral presentation state. X11 derives them from EWMH state and the
ICCCM urgency hint; a future Wayland backend can derive the same policy from
native toplevel metadata. The X11 backend keeps ownership rules intact: it may
clear EWMH demands-attention after activation, but only the client changes its
ICCCM urgency bit. Rendering an urgent frame is backend-owned realization of
the shared attention state.

Minimization and decoration focus remain core lifecycle state. X11 publishes
that state through the window-manager-owned EWMH hidden and focused atoms,
repairing direct client mutations. A client hidden only because another
workspace is active is not minimized, even though ICCCM `WM_STATE` must be
Iconic while its frame is unmapped.

The core also resolves the user operations currently available for each
client from its role, validated capabilities, and runtime state. X11 publishes
that result as EWMH allowed actions; it does not infer policy from atom names.
Fixed-size hints remove resize/maximize capability at the boundary, while
fullscreen temporarily masks operations that cannot sensibly apply there.

Pager requests reuse normal policy paths. EWMH close requests flow through the
same capability check and ICCCM protocol negotiation as titlebar actions.
EWMH moveresize requests are parsed into a small backend request value and then
use the same size constraints, maximize/fullscreen suppression, gravity
adjustment, frame configuration, and synthetic notification as ConfigureRequest.

Initial placement is pure geometry policy. The core scores outer rectangles on
an edge-derived grid using bounded integer arithmetic and can center a result
within a free field or relative to an anchor. X11 supplies decorated visible
obstacles and the destination workspace's work area, skips adopted and
ICCCM-positioned clients, and converts the selected outer position back to
client coordinates. A future compositor can supply scene rectangles to the
same policy without inheriting X11 position flags.

Output identity, geometry, primary selection, overlap ownership, and nearest
fallback are protocol-neutral policy. X11 discovers that topology from RandR
1.5 monitors or RandR 1.2 CRTCs, subscribes to topology changes, and falls back
to the root rectangle when RandR is absent. Placement follows a parent or the
focused client onto its output; maximize and fullscreen use the output owning
the largest part of the client. A disconnected output causes ordinary clients
to move into the nearest surviving work area while maximized and fullscreen
clients reflow through their existing core state transitions.

Edge reservations are protocol-neutral depth-and-span values. X11 struts are
translated into these values at the backend boundary; the core intersects them
with an output and derives a safe, non-empty work area. This same calculation
can later consume layer-shell exclusive zones without representing them as X11
properties.

The X11 runtime caches one derived work area per output and policy workspace,
while publishing the EWMH root-wide work area required by pagers. X11 strut
depths are translated from root edges to each affected output before entering
core geometry. Reservation membership is selected from each client's core
workspace assignment, so sticky docks apply everywhere and local docks do not
shrink unrelated workspaces. Maximized geometry always queries the client's
own output and workspace; sticky clients query the currently visible workspace.

The core should remain a deterministic state machine. Backends translate
external events into validated state transitions, ask the core for policy, and
apply the resulting decisions using their own protocol. The core does not open
devices, own event loops, allocate display-server resources, render pixels, or
send protocol messages. This keeps policy tests fast and lets both backends
share answers to user-visible questions without sharing protocol machinery.

Protocol hints are translated at the boundary. For example, ICCCM size hints
become protocol-neutral size constraints before entering the core. EWMH
window types and Motif hints become client roles, capabilities, and decoration
choices. EWMH fullscreen and above/below atoms become core state transitions.
EWMH skip-taskbar, skip-pager, and demands-attention atoms become presentation
state rather than X11-specific focus policy.
EWMH desktop indexes and the all-desktops sentinel become core workspace
assignments; the same policy can later be driven by compositor workspace
actions without emulating root-window properties. Rectangular workspace
geometry is policy-owned as typed orientation and corner values. X11 accepts
`_NET_DESKTOP_LAYOUT` only while a pager owns its required manager selection;
otherwise validated TOML supplies the fallback layout.
Application-rule identity and settings are also protocol-neutral. The X11
backend translates `WM_CLASS`, `WM_WINDOW_ROLE`, titles, and EWMH window types
into that identity only when rules exist, then applies the resolved initial
workspace, layer, decoration, and focus policy. A future Wayland backend can
supply native application identifiers and surface roles to the same matcher.
EWMH restacking is applied by X11 and its observed result is synchronized into
core stacking state. A future Wayland backend should perform the equivalent
translation from xdg-shell and compositor state rather than emulating X11
properties.

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
- A failed runtime reload preserves the last working configuration.
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
