# Architecture

The central design rule is that X11 is a backend, not the window manager's
policy model. Nobox preserves one user-visible window-management contract; it
does not pretend X11 window management and Wayland compositing are symmetric.

```text
X11 server <──> nobox-x11 ──────┐
                                ├──> nobox-core ──> policy decisions
Wayland clients <──> nobox-wayland┘

nobox CLI/session <──> nobox-runtime <──> selected backend event loop
          │
          └──> validated configuration
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

`nobox-runtime` owns the deliberately small process boundary shared by real
backends: backend identity and factual capabilities, typed reload/exit/session
requests, clean run disposition, and protocol-neutral session persistence. It
has no X11, Wayland, Smithay, configuration, or core-policy dependency. A live
backend creates one atomic instance record and mode-0600 same-UID Unix socket
under a private `$XDG_RUNTIME_DIR/nobox/` directory. X11 publishes only the
opaque instance identity after its EWMH supporting-window chain is established;
Wayland discovery requires either that identity or exactly one live instance.
Each backend translates accepted requests into its native event-loop wakeup,
so X11 ClientMessages and calloop sources remain backend details.

Every reparented client enters the X save set before leaving the root. If nobox
is killed without cleanup, the X server destroys manager-owned frames, reparents
those clients to the root, and maps them; connection destruction also releases
`WM_Sn`. A fresh nobox treats the recovered windows as ordinary startup clients
and rebuilds frames, EWMH lists, stacking, and focus. The nested-X gate exercises
that forced-death path rather than relying on graceful-shutdown behavior.

The backend owns exactly one manager selection, `WM_Sn`, on its dedicated
support window. It serves the ICCCM-required `TARGETS`, `MULTIPLE`, and
`TIMESTAMP` conversions but never claims `PRIMARY`, `SECONDARY`, or `CLIPBOARD`.
On replacement it releases root input ownership and other managed resources
before destroying the support window; window destruction relinquishes `WM_Sn`
without a disowning race or interference with an incoming manager.

`nobox-config` owns one strict, versionable TOML schema and the canonical
format-preserving document API. The manager, settings UI, and other tools share
its defaults, effective-value resolution, validation, loading, typed edits, and
atomic persistence contract. The autostart script is kept separate because its
executable shell format is already the clearest user interface for that job.

`nobox-desktop` owns bounded XDG desktop-entry discovery independently of X11.
It applies data-directory precedence, visibility and executable checks,
localization, stable categories, and desktop `Exec` field-code expansion into
direct argv. Menus, the Agent Seat launch-policy picker, and the future optional
panel can therefore share one catalog without treating installed desktop-file
content as shell code.

`nobox-settings` is a separate optional process, never a toolkit inside the
window manager. It is only a GTK/libadwaita presentation of
`nobox-config::ConfigDocument`; it does not maintain its own TOML paths,
defaults, validation, or save rules. The shared document retains comments,
ordering, bindings, menus, and application rules while typed controls replace
only their own scalar or workspace-list values. The binary is feature-gated and
CMake builds it only when local development metadata is present. This keeps GTK
out of the manager's dependency and failure boundary while permitting a native
modern UI.

The integrated Agent Seat follows the same policy/realization split.
`nobox-agent-wire` contains bounded framing and typed wire values, not policy
or X11 resources. It is Nobox's GPL-2.0-only implementation name; the neutral
`agent-seat` identity remains on the wire. `nobox-agent-seat` owns only the
bounded private UNIX-socket lifecycle, verified peer credentials, framing
queues, and backend-provided event-loop wakeup; it has neither grant authority
nor a display-server dependency. `nobox-core` owns session grants, client
scope, visibility, generations, freshness, and event policy. `nobox-config`
owns the strict persisted seat and launch policy. Each backend owns discovery,
capture/input realization, consent, and indicators. `nobox-wayland` projects
native and XWayland application identity, decorated frames, output names,
work areas, and workspace names through the same core `ClientDetails`
contract. It records application-rule visibility and scoped membership at the
shared manage/update/remove boundary; no Smithay handle or Wayland object ID
enters core or the wire. Its current W8 foundation attaches the same bounded
transport to nested and direct calloop loops, but exposes it only through an
explicitly configured or derived socket. Verified stored grants are narrowed
to the observation operations already realized; discovery environment,
mutations, capture, input, consent, and indicators remain unadvertised until
their acceptance gates pass. Atomic subscriptions use a settled,
display-neutral shadow diff and core's bounded per-session queues, so native
and XWayland client events obey the same redaction and scope rules as
snapshots. Before accepting peers the X11 backend claims the per-screen
`_AGENT_SEAT_S<screen>` selection on a dedicated window and publishes
identical bounded `_AGENT_SEAT` values on that window and the root.
Selection loss stops only the seat. `nobox-agent` remains an optional MCP
translator with no authority; its fallback discovery validates the current
selection owner and both properties rather than synthesizing a Nobox path.
Every request is revalidated inside the manager, and companion failure cannot
enter the WM failure boundary.

This GPL-2.0-only implementation stays in Nobox. It is not extracted into or
made source-compatible with the independent Apache-2.0 `agent-seat-proto`
product. The two products may implement compatible public wire revisions and
test them through process boundaries, but share no implementation source or
build dependency. Product separation, the Nobox launch-permission UI,
coordinate-efficiency rules, and the standalone Tier 0 X11 roadmap are defined
in [`agent-seat-separation-roadmap.md`](agent-seat-separation-roadmap.md).

`nobox-panel` follows the same process boundary. The thin session executable
starts it only when `[panel].enabled` is true, replaces it after a successful
Reconfigure, and stops it before manager exit or restart. A private readiness
pipe keeps the old process alive until its replacement has committed its first
drawable surface, preventing resource-ID reuse and retaining the working panel
when startup fails. A thin CLI selects peer `x11` and `wayland` frontend
modules. The X11 frontend publishes standard EWMH dock, state, desktop, and
strut properties and sends ordinary pager client messages. The Wayland frontend
publishes a layer-shell exclusive zone and consumes advertised standard
foreign-toplevel/workspace plus wlr interactive-toplevel protocols. Wlr output
membership supplies current-workspace task filtering without compositor-private
IPC. Every request returns through the backend's normal core action checks; the
panel has no authority over the manager and never depends on `nobox-core` or
`nobox-wayland`. Both frontends use the bounded `nobox-desktop` catalog and
direct parsed commands. GTK remains confined to the optional settings
application.

`agent-semantic-helper` is a separate optional, disposable accessibility
translator. It owns the pure-Rust AT-SPI/D-Bus dependencies and their small
async reactor; none enter `nobox-core`, `nobox-x11`, `nobox-agent`, or the main
executable. It receives one bounded root-correlation/projection request, has no
grant authority, and returns no raw D-Bus or X11 identity. Resource limits,
no-new-privileges, and a post-connect seccomp allowlist confine each invocation.
The X11 backend remains authoritative for X-Resource process identity,
authorization, generations, cancellation, and reply timing. A dedicated
worker owns the child and bounded pipes; completion wakes the existing X11
control channel, while a manager timer enforces the fixed public deadline. The
window-management event loop never waits for AT-SPI, D-Bus, or child exit. A
successful root request becomes a generation-scoped neutral semantic page. A
tree request performs a deterministic breadth-first traversal bounded to 4,096
inspected nodes, depth 16, and 128 returned nodes. The helper rechecks the D-Bus
owner PID at every visited object and emits only collision-checked internal
identities; the backend remaps them to monotonic session/client/tree-local node
handles. At most 16 opaque continuation cursors are retained per live tree.
Constrained search uses the same traversal and limits but returns only nodes
matching a bounded case-insensitive name substring, role OR-set, and state
AND-set; the manager re-evaluates the predicate before replying. Search
continuations retain the original predicate rather than trusting later caller
arguments.
Raw bus paths, process IDs, helper identities, and X11 resource IDs never cross
the manager-to-agent boundary.

Persistent window-session state is a separate strict, versioned, bounded TOML
document under the XDG state directory. `nobox` loads it before connecting and
atomically writes a user-only replacement after a clean event-loop exit. The
X11 backend translates `SM_CLIENT_ID` or legacy `WM_COMMAND` plus application
metadata into single-use restore candidates, rejects every ambiguous duplicate,
and captures protocol-neutral geometry, workspace, layer, presentation,
stacking, and focus values. X11 properties and window IDs are never persisted.
Native XSMP coordination is isolated in an optional `nobox-xsmp` companion
linked to libSM/libICE. It exists only when CMake discovers a C compiler and the
development packages, and it starts only when `SESSION_MANAGER` is present.
Bounded line messages translate `SaveYourself`, `SaveComplete`,
`ShutdownCancelled`, and `Die` callbacks into typed runtime control requests;
the X11 loop captures the snapshot at a coherent event boundary and acknowledges
success only after the process layer atomically persists it. The companion
also translates a confirmed backend-neutral logout intent into the XSMP
client's global interactive `SaveYourself` request. The window manager keeps
owning X11 while the external manager coordinates applications, stays alive on
`ShutdownCancelled`, and exits through its ordinary clean path only on `Die`.
Without a connected companion, the same intent becomes a clean local manager
exit after confirmation, matching Openbox's non-XSMP fallback. The companion
publishes program, user, process, clone/restart command, client identity,
restart-style, and desktop priority properties. This keeps unsafe FFI out of
Rust, libSM/libICE out of the default executable dependency graph, and session
protocol mechanics out of the policy core. Relaunching application processes
remains the external session manager and each application's responsibility.

`nobox` is deliberately thin: logging, CLI dispatch, config selection,
autostart, optional companion coordination, and backend startup.

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
X11-only top-level metadata stays at that realization boundary. In particular,
the backend mirrors client opacity onto the reparenting frame and removes the
mirror when the client property disappears; this compositor-facing mechanism
does not become part of shared window policy.

Pointer move/resize edge snapping is also pure geometry policy. Backends supply
the active work area and input delta, while the core deterministically resolves
the snapped result. X11 owns grabs and event cancellation; future compositor
input handling can reuse the geometry without inheriting X11 grab semantics.
The high-level `Move` and `Resize` actions are input-agnostic. A pointer
invocation carries its coordinates and optionally a configured edge; an action
without pointer context enters the backend's grabbed keyboard interaction.
Both paths share the same core geometry constraints and commit/cancel contract.

Configured relative geometry is typed before it reaches a backend. The shared
action model accepts signed pixels or rational amounts; the backend supplies the
relevant work-area or client dimension, and the core applies overflow-safe edge
resize arithmetic, size hints, and opposite-edge anchoring. X11 therefore does
not become the meaning of `MoveRelative` or `ResizeRelative`.
For reparented X11 clients, every realized geometry decision ends with an ICCCM
synthetic `ConfigureNotify` containing client content size and root-relative
coordinates. This includes denied fullscreen/maximized requests, so a client
never mistakes its frame-relative real notification for authoritative placement.

Directional edge movement is also shared geometry policy. The backend supplies
visible decorated rectangles and active work-area bounds; the core selects the
next near or far edge using only cardinal direction and rectangle overlap. This
keeps X11 client discovery separate from reusable obstacle behavior.

Spatial focus targeting follows the same division. Core scores candidate
centers in eight directions, prioritizes the requested 90-degree cone, and uses
caller order only for complete ties. X11 supplies visible decorated client
rectangles, then performs the backend-specific unshade, ICCCM focus, and layer
raise steps. A compositor can reuse the selector with scene rectangles.
Immediate and modifier-held spatial actions share the selector; spatial and MRU
cycles in turn share one candidate snapshot, grab, overlay, client-loss, commit,
and cancellation state machine. Preview focus suppresses permanent raising,
while commit applies unshade/focus/raise and Escape restores the original focus.

Focus history and fallback are core policy. `FocusToBottom` changes only the
active workspace's MRU ordering; `Unfocus`/`FocusFallback` exclude the old
target, shaded/iconic/hidden/non-focusable ordinary clients, resolve modal
redirects, and either select the next history entry or clear focus. X11 then
realizes that result through ICCCM focus methods, colormaps, and EWMH state.
Deliberate `Focus` is a higher-level activation contract: the shared target's
workspace and iconic state determine whether policy follows it or brings its
family to the active workspace, while the backend realizes visibility and its
native focus protocol. This avoids defining the action as an X11-only call to
`SetInputFocus` and gives a later compositor the same user-visible behavior.

Directional grow, shrink, and fill actions consume that same rectangle field.
The core performs the two-pass blocker search and half-size shrink bound; the
existing constrained relative-resize policy then maps the desired outer edges
back onto content geometry. Backends retain only visible-surface discovery and
the final configure operation.

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

Adaptive restacking is also policy-owned. Given visible same-layer rectangles
in bottom-to-top order, core raises a target obscured from above, lowers one
that obscures a peer below, and otherwise preserves order. X11 filters out
iconic, hidden, cross-layer, and specific-transient-family rectangles before
realizing the decision. `ShadeLower` and `UnshadeRaise` compose the existing
authoritative shade and layer operations rather than adding new state.

Desktop-showing mode is policy-owned visibility state rather than a batch of
minimize operations. The core temporarily excludes ordinary roles while
retaining desktop and dock surfaces, workspace membership, focus history, and
each client's genuine iconic state. X11 realizes this with frame mapping and
the `_NET_SHOWING_DESKTOP` root contract; a compositor can apply the same state
to its scene graph.

Shading is similarly retained as backend-neutral client state. It preserves
content geometry and iconic state while asking the backend to expose only the
server-side titlebar. X11 safely accounts for the intentional client unmap and
publishes EWMH shaded state; future scene-graph backends can collapse content
without inheriting X11 lifecycle rules.

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

Keyboard configuration layers user overrides and explicit omissions over one
standard keymap owned by `nobox-config`; opting out replaces that keymap
entirely. The resulting sequences and ordered action lists are validated before
reaching a backend. X11 resolves symbolic chords against the live
keyboard map and keeps only the currently valid sequence-prefix grabs active.
A single sleeping timer worker delivers generation-tagged X11 control events
for keyboard-chain, client-responsiveness, and synchronized-resize deadlines.
Incomplete chains expire without polling, protocol tracking adds no additional
threads, and stale timer events cannot cancel or mark newer state. Mapping
changes and configuration reloads rebuild the same typed key tree and cancel
any active sequence safely.

Close requests use the EWMH `_NET_WM_PING` protocol only when a client advertises
it. One timestamp/window-correlated ping is armed after `WM_DELETE_WINDOW`; a
pong removes the deadline immediately, while a timeout gives the frame an urgent
"Not Responding" title. Nobox never kills on timeout alone. Repeating close on
that visibly unresponsive client explicitly disconnects it from X11, and a late
pong restores normal presentation without polling or recurring traffic. The
separate typed `Kill` action bypasses ICCCM negotiation and disconnects the
owning X11 connection immediately, while sharing pending-ping cancellation with
the repeated-close path.

Interactive resize pacing is also an X11 backend concern. When a client opts in
to `_NET_WM_SYNC_REQUEST`, nobox initializes its X Sync counter and retains one
alarm plus the latest pending geometry only for the duration of the drag. Each
acknowledged sequence releases at most one coalesced configure. A generation-
tagged one-shot timeout destroys the alarm and resumes the ordinary direct path,
so protocol support improves visual pacing without entering the core geometry
model or trusting a stalled client with interaction progress.

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

Menus follow the same boundary. `nobox-config` owns the validated named graph of
titles, typed static or dynamic sources, items, separators, submenu references,
and ordered actions, including cycle and resource bounds. X11 resolves dynamic
client, workspace-destination, and combined window-list sources into bounded
runtime snapshots. It also executes command sources with a configured deadline
and bounded private output file, but sends the emitted UTF-8 TOML back through
the shared graph/action validator before constructing a snapshot. That keeps
shell lifecycle and temporary storage backend-owned without creating a second,
less strict menu language. Backend-only client identifiers never enter
persisted configuration or the shared action model. X11 also owns one persistent
override-redirect surface, temporary keyboard/pointer grabs, accelerator
translation, output clamping, expose redraws, and input translation. The
surface switches snapshots when traversing submenus instead of allocating a
window per level. A future Wayland renderer can populate the same typed sources
and consume the same configured graph without inheriting X11 windows or grabs.

Task-list visibility, pager visibility, and effective urgency are
protocol-neutral presentation state. X11 derives them from EWMH state and the
ICCCM urgency hint; a future Wayland backend can derive the same policy from
native toplevel metadata. The X11 backend keeps ownership rules intact: it may
clear EWMH demands-attention after activation, but only the client changes its
ICCCM urgency bit. Rendering an urgent frame is backend-owned realization of
the shared attention state.

Transient policy distinguishes a specific parent from an ICCCM application-
group transient. Specific descendants form one movable branch. A group
transient is ordered above the ordinary members of its group and follows an
ordinary member across workspaces together with its own specific descendants;
moving the group transient itself does not pull unrelated group members or
sibling group transients. Every ancestry, stacking, and workspace traversal is
visited-set bounded so mutually transient and historical circular-group hints
cannot recurse indefinitely. X11 only translates `WM_TRANSIENT_FOR` and
`WM_HINTS` window-group identifiers into this shared graph.

Minimization and decoration focus remain core lifecycle state. X11 publishes
that state through the window-manager-owned EWMH hidden and focused atoms,
repairing direct client mutations. A client hidden only because another
workspace is active is not minimized, even though ICCCM `WM_STATE` must be
Iconic while its frame is unmapped.

X input focus is observed as well as assigned. Stable `FocusIn` and `FocusOut`
transitions are translated back into core focus history, with a bounded parent
walk resolving toolkit child windows to their managed top-level. Grab/ungrab
events and inferior transitions within one client are filtered at the X11
boundary. Focus leaving the managed tree clears EWMH ownership without nobox
immediately stealing it back.

The core also resolves the user operations currently available for each
client from its role, validated capabilities, and runtime state. X11 publishes
that result as EWMH allowed actions; it does not infer policy from atom names.
Fixed-size hints remove resize/maximize capability at the boundary, while
fullscreen temporarily masks operations that cannot sensibly apply there.
ICCCM minimum, maximum, base, increment, and aspect hints are translated into
the shared `SizeHints` value on management and every live property change.
Invalid zero or below-minimum maxima normalize before policy use. Client and
user geometry requests then share the same overflow-safe constraint path;
oversized clients are not implicitly shrunk merely because they exceed an
output and remain movable with their content dimensions intact.

Pager requests reuse normal policy paths. EWMH close requests flow through the
same capability check and ICCCM protocol negotiation as titlebar actions.
EWMH moveresize requests are parsed into a small backend request value and then
use the same size constraints, maximize/fullscreen suppression, gravity
adjustment, frame configuration, and synthetic notification as ConfigureRequest.
Client-initiated `_NET_WM_MOVERESIZE` remains at the same boundary: X11 parses
the direction and owns its temporary pointer/keyboard grabs, while shared
geometry, work-area, capability, and size-hint rules determine every applied
step. Button release or Enter commits; Escape and the protocol cancel direction
restore the retained starting geometry. Keyboard and pointer resize also reuse
the existing synchronized-resize pacing rather than introducing another path.

Initial placement is pure geometry policy. The core scores outer rectangles on
an edge-derived grid using bounded integer arithmetic and can center a result
within a free field or relative to an anchor. X11 supplies decorated visible
obstacles and the destination workspace's work area, skips adopted and
ICCCM-positioned clients, and converts the selected outer position back to
client coordinates. A future compositor can supply scene rectangles to the
same policy without inheriting X11 position flags.

X11 burst handling preserves that policy without repeating global work for
every map. Initial client properties are requested as one bounded pipeline. A
new frame is inserted relative to the complete core stacking order when the
existing order is already correct, with full enforcement as the safe fallback.
Focus repaints only the old and new frames, and consecutive eligible new-client
focus requests collapse to the final request until the event queue drains. The
deferral is bounded to 256 events and direct user input cancels it. These are
backend scheduling optimizations; core placement, focus eligibility, stacking,
and focus-stealing decisions remain authoritative.

Configured absolute placement uses the same boundary. Strict config types
represent gravity-style axis anchors, positive relative dimensions, size bases,
and abstract output targets. X11 resolves the chosen output and its workspace
work area, constrains client content through ICCCM hints, and gives the resulting
decorated size plus source/target bounds to core placement. The core preserves
relative offsets across outputs, resolves start/center/end anchors, and keeps
the result on screen without knowing RandR identities.

Output identity, geometry, primary selection, overlap ownership, and nearest
fallback are protocol-neutral policy. X11 discovers that topology from RandR
1.5 monitors or RandR 1.2 CRTCs, subscribes to topology changes, and falls back
to the root rectangle when RandR is absent. Placement follows a parent or the
focused client onto its output; maximize and fullscreen use the output owning
the largest part of the client. A disconnected output causes ordinary clients
to move into the nearest surviving work area while maximized and fullscreen
clients reflow through their existing core state transitions.

Exact display-area coverage outside a managed fullscreen transition is also a
protocol-neutral client fact. The X11 boundary recognizes Openbox-style legacy
fullscreen only for undecorated, non-maximized clients whose content geometry
exactly equals an output or the root. The core conditionally promotes that
coverage to the fullscreen stacking layer while the client or a specific
transient is focused, while no client is focused, while it is on another
workspace, or while focus belongs to another output. A same-output competitor
demotes it to its requested layer. This never synthesizes EWMH fullscreen or a
restore rectangle; client geometry remains authoritative.

Edge reservations are protocol-neutral depth-and-span values. X11 struts are
translated into these values at the backend boundary; the core intersects them
with an output and derives a safe, non-empty work area. This same calculation
can later consume layer-shell exclusive zones without representing them as X11
properties.
Configured outer-screen margins enter the same reservation calculation as
backend-independent policy and apply to every workspace. X11 publishes the
result through `_NET_WORKAREA`; initial placement, maximize, absolute geometry,
and live reload consume that identical result.

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
The core also owns the last-active index and indexed workspace insertion/
removal. It shifts or merges membership and MRU histories without protocol
types; X11 updates its session-local name list, work areas, visibility, focus,
and EWMH properties from that one transition.
Application-rule and session identity settings are also protocol-neutral. The X11
backend translates client and group `WM_CLASS`, `WM_WINDOW_ROLE`, titles, and
EWMH window types into application identity, then applies the resolved initial
workspace, layer, decoration, focus, presentation, state, and typed geometry
policy. X11 position hints are preserved unless the rule explicitly forces
placement; session restoration retains final precedence. Session matching
additionally reads bounded `SM_CLIENT_ID` and `WM_COMMAND` data at the backend
boundary. A future Wayland backend can supply native application identifiers
and surface roles to the same policy without emulating those X11 properties.
EWMH restacking is applied by X11 and its observed result is synchronized into
core stacking state. A future Wayland backend should perform the equivalent
translation from xdg-shell and compositor state rather than emulating X11
properties.

Process lifecycle is kept outside the policy core. The X11 event loop returns
a typed exit or restart disposition together with its bounded session snapshot.
The CLI then either reconnects a fresh backend without rerunning autostart, or
replaces itself with an explicitly configured manager command only after X11
clients, grabs, root properties, and the manager selection have been released.

Conditional action structure and matching live in the protocol-neutral config
model. Backends supply a bounded query context containing core client state,
workspace history, output number, and normalized application identity. The X11
backend translates live properties into that context and executes the shared
`If`/`ForEach`/`Stop` flow over a stable management-order snapshot. A future
Wayland backend can provide the same facts without exposing surface protocols
to the action language.
The `Debug` action is likewise backend-neutral: configuration validates its
bounded message, and the runtime emits it through structured tracing without
introducing protocol-specific output paths.

Application launch intent follows the same split. The shared `Execute` action
contains a shell command, optional confirmation text, and optional name/icon/
application identity metadata. X11 resolves its client and pointer context,
expands the Openbox-compatible `$pid`, `$wid`, and `$pointer` variables, and
translates launch metadata into the bounded freedesktop startup-notification
stream. Sequence parsing, `_NET_STARTUP_ID`, X timestamps, workspace placement,
failure cleanup, and child reaping remain backend/runtime mechanics. A Wayland
backend can translate the same launch intent to activation tokens without
introducing X atoms into either config or core policy.

Focus-stealing prevention splits at the same boundary. The core answers whether
two clients share a specific-transient or application-group family. X11 owns
wrap-safe server timestamp ordering, `_NET_WM_USER_TIME` and its auxiliary
window, and EWMH activation source interpretation. A denied request is
translated into the existing protocol-neutral attention state instead of
changing focus or workspaces.

X Shape is deliberately backend-only. The X11 controller discovers the
optional extension, tracks client bounding and input regions, and composes
those regions with its frame decorations. Shape notifications and X11 regions
do not enter `nobox-core`: they alter server-side realization without changing
window-management policy, and servers without the extension remain supported.

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

Explicit state actions use typed protocol-neutral axes, decoration preferences,
and stacking layers. The core owns idempotence and geometry restoration; the
X11 controller realizes changed state through frames and mutually exclusive
EWMH atoms. This keeps `maximize`, `decorate`, `shade`, and layer intent usable
by a future backend without making X11 property names part of the policy API.

Abstractions are added from demonstrated policy needs, not speculative parity.
X11 work continues first, but new X11 code must keep raw protocol types and I/O
inside `nobox-x11`. When the Wayland backend begins, any boundary that does not
fit both real implementations should be revised rather than preserved for API
stability.

## Verification boundary

- Pure policy transitions and invariants belong in `nobox-core` unit tests.
- ICCCM/EWMH behavior, X server ordering, and protocol races belong in nested
  X11 integration tests.
- Configuration editing invariants belong in `nobox-config` unit tests; the
  optional GTK surface additionally gets a mapped nested-X save test.
- Openbox regression programs are behavioral evidence, not core APIs; tests
  assert the resulting user-visible contract.
- Future Wayland protocol tests must exercise the same policy outcomes while
  independently validating compositor responsibilities.

## Invariants

- Protocol errors from misbehaving clients must not crash the manager.
- A failed runtime reload preserves the last working configuration.
- X11 theme font resources and their server-reported metrics are backend-owned;
  the shared configuration stores presentation intent without exposing XIDs.
- Unknown config keys fail validation.
- A client occurs at most once in focus and stacking state.
- All external dimensions are clamped to at least one pixel.
- No `unsafe` Rust is allowed in this workspace.
- Starting beside another X11 window manager fails rather than replacing it.
- Read-only diagnostics never select root events or claim the ICCCM WM selection.
- Display-server handles and protocol messages never enter `nobox-core`.

## Wayland implementation

The hardened X11 baseline provides the behavioral oracle and policy seam for a
native compositor without making X11 the internal model. Smithay provides
protocol, rendering, input, and session/device building blocks while Nobox
retains its own policy. The neutral runtime extraction and explicit backend
selection are complete; managed Wayland shell behavior begins at W2. The phased
implementation, explicit
end result, protocol baseline, non-goals, and acceptance gates are defined in
[`wayland-roadmap.md`](wayland-roadmap.md).

Wayland work does not deprecate the X11 backend or justify speculative shared
machinery. Each new neutral abstraction must be demonstrated by both real
backends; compositor-only state remains in `nobox-wayland`.
