# X11 roadmap

Openbox remains the behavioral reference. Its programs under `../openbox/tests`
are a valuable catalog of cases, but each behavior needs a deterministic nobox
test and an explicit compatibility decision.

## Milestone 0: running skeleton

- Own `SubstructureRedirect` and fail safely if another manager is present.
- Own the ICCCM `WM_Sn` selection and publish the `MANAGER` announcement.
- Adopt existing windows and handle map, unmap, destroy, and configure events.
- Publish WM identity, active window, client lists, and one desktop via EWMH.
- Click focus and configurable focused/unfocused borders.
- Super + left/right drag for move/resize.
- Nested `Xnest` smoke test with a real `xterm`.

## Milestone 1: usable single-desktop session

- [x] Layout-aware key grabs refreshed after X11 keyboard mapping changes.
- [x] Typed execute, close, and exit actions, including `WM_DELETE_WINDOW`.
- [x] ICCCM input models (`WM_HINTS`, `WM_TAKE_FOCUS`) with event timestamps.
- [x] Minimum/maximum sizes, base sizes, and resize increments.
- [x] Aspect-ratio constraints, including the upstream Openbox `aspect` test.
- [x] Window gravity, including the upstream Openbox `grav` test.
- [x] Transient relationships, modal groups, and focus fallback, verified with
  the upstream Openbox `modal`, `modal2`, and `groupmodal` programs.
- [x] Iconic lifecycle, synthetic-unmap handling, and WM-owned EWMH hidden and
  focused state, verified with the upstream Openbox `mapiconic` and
  `fakeunmap` programs plus nested taskbar/pager regressions.
- [x] ConfigureRequest and EWMH restacking synchronized from server-observed
  order, including the upstream Openbox `stacking` program.
- [x] Crash-safe reparenting frames, `_NET_FRAME_EXTENTS`, themed titlebars, and
  a working close button.
- [x] EWMH window roles, Motif hints, pre-map frame-extents estimates, and
  dynamic per-client decoration capability rules.
- [x] Live UTF-8/legacy window titles and capability-aware minimize buttons.
- [x] Initial/runtime EWMH maximize state, exact axis-aware restore geometry,
  and capability-aware maximize buttons.
- [x] Dynamic `_NET_WORKAREA`, partial/legacy struts, default desktop/dock
  layering, non-focusable docks, and maximized-client reflow.
- [x] Initial/runtime EWMH fullscreen with undecorated output geometry, exact
  maximize-aware restoration, and geometry-request suppression.
- [x] Openbox-compatible legacy fullscreen detection for exact undecorated
  root/output coverage, with focus/output-aware stacking and no synthetic state.
- [x] Mutually exclusive EWMH above/below state in a protocol-neutral ordered
  layer model, enforced against observed X11 stacking.
- [x] Interactive move/resize cancellation, configurable work-area edge
  resistance, and size-hint-constrained resizing.
- [x] Clean SIGINT/SIGTERM shutdown and validated in-place SIGHUP config reload.
- [x] Optional X Shape bounding/input propagation through reparenting frames,
  including live region changes and rectangular fallback.
- [x] User-time focus-stealing prevention for new maps and EWMH activation,
  including auxiliary timestamp windows, related-client exceptions, and
  demands-attention fallback.
- [x] External X input-focus reconciliation through managed child trees, with
  grab/ungrab and ancestor/inferior noise filtering plus exact EWMH ownership.
- [x] ICCCM colormap focus with bounded `WM_COLORMAP_WINDOWS` priority lists,
  implicit top-level handling, live property/attribute tracking, shared child
  watches, and default-colormap restoration.
- [x] Input-only descendant cursor preservation through top-level reparenting,
  verified from the server-selected XFixes cursor image.
- [x] EWMH `_NET_WM_PING` close responsiveness with exact timestamp/window
  correlation, one-shot deadlines, visible unresponsive state, late recovery,
  and an explicit repeated-close force-disconnect policy.
- [x] EWMH `_NET_WM_SYNC_REQUEST` pacing for interactive resize, including
  manager initialization, 64-bit sequencing, X Sync alarms, latest-motion
  coalescing, and a bounded fallback for stalled clients.
- [x] ICCCM manager-selection conversions and ordered replacement handover,
  with `PRIMARY` and `CLIPBOARD` ownership and transfers preserved end to end.
- [x] Strict, bounded local session snapshots with atomic XDG-state persistence,
  duplicate-safe `SM_CLIENT_ID`/`WM_COMMAND` matching, and restoration of
  workspace, normal geometry, state, stacking, and focus after a clean restart.

Initial Openbox regression cases: `aspect`, `fakeunmap`, `focusout`, `grav`,
`groupmodal`, `mapiconic`, `mingrow`, `modal*`, `noresize`, `resize`, `stacking`,
`shape`, `strut`, `urgent`, and `wmhints`.

## Milestone 2: Openbox-class workflows

- [x] Named multiple desktops with EWMH switching, sticky clients, window
  moves, runtime count changes, and per-desktop focus history.
- [x] Desktop grid layout, directional switching/moves, wrap policy, and
  selection-validated `_NET_DESKTOP_LAYOUT` pager interoperability.
- [x] Cycle-safe specific-transient workspace families, parent layer
  inheritance, and parent-before-child stacking enforcement.
- [x] Per-desktop dock/strut work areas with sticky reservations and
  workspace-aware maximized-client reflow.
- [x] Ordered initial application rules matched by class/name/role/title/type,
  with workspace, layer, decoration, and focus policy.
- [x] Modifier-held forward/reverse MRU focus cycling with modal redirection and
  hidden, iconic, and non-focusable client filtering.
- [x] Work-area-aware least-overlap smart placement, free-field centering,
  ICCCM explicit-position preservation, and parent-relative dialog placement.
- [x] Initial/live EWMH skip-taskbar and skip-pager state, task-switcher
  filtering, and themed attention from both client-owned ICCCM urgency and
  EWMH demands-attention with activation clearing.
- [x] Live `_NET_WM_ALLOWED_ACTIONS` derived from core roles, capabilities,
  fullscreen state, Motif functions, and fixed-size ICCCM constraints.
- [x] Pager `_NET_CLOSE_WINDOW` and `_NET_MOVERESIZE_WINDOW` requests through
  shared close/configure paths with timestamps, masks, gravity, constraints,
  and synthetic `ConfigureNotify` delivery.
- [x] Client-initiated EWMH `_NET_WM_MOVERESIZE` with all pointer edges,
  keyboard move/resize, retained grabs, capability checks, button/Enter commit,
  and exact Escape or protocol-cancel rollback.
- [x] Openbox-style keyboard chains with configurable cancellation/timeout,
  mapping-aware prefix grabs, and ordered action lists while retaining legacy
  single-chord/single-action configuration.
- [x] Context-aware mouse bindings with modifier/button chords, ordered actions,
  press/release/click/double-click/drag recognition, useful context fall-through,
  titlebar/button/desktop defaults, and edge/corner-aware interactive resizing.
- [x] Dynamic RandR monitor/CRTC discovery with a root fallback, shared output
  selection, per-output/workspace struts, output-aware placement/maximize/
  fullscreen, and safe reflow after topology changes.
- [x] Output-aware on-screen focus-cycle list with bounded scrolling, live
  titles, modifier-release commit, Escape restoration, and clean client-loss
  teardown.
- [x] Strict single-file configured menus with bounded rendering, nested menu
  graphs, ordered actions, root-pointer placement, keyboard/pointer navigation,
  and deterministic grab/dismissal lifecycle.
- [x] Dynamic capability-aware client and workspace-destination menus, combined
  workspace-grouped window lists, and Openbox-style menu keyboard accelerators.
- [x] Bounded initial/live `_NET_WM_ICON` metadata parsing with deterministic
  replacement, deletion, and hostile-property regressions.
- [x] Reloadable focus-follows-mouse policy with grab-safe pointer entry and
  independently configurable raise behavior.
- [x] EWMH desktop-showing mode with desktop/dock preservation, non-iconifying
  policy state, pager requests, activation restore, and a typed toggle action.
- [x] Initial/live EWMH shading with titlebar-only frames, safe client unmaps,
  hidden geometry updates, fullscreen interlock, and capability publication.
- [x] Capability-aware fullscreen and always-on-top/bottom user actions exposed
  through typed bindings and dynamic client menus.
- [x] Validated `_NET_WM_FULLSCREEN_MONITORS` requests with authoritative
  property publication, output-spanning geometry, and topology-safe fallback.
- [x] Openbox-style `Reconfigure` as a typed binding/menu action routed through
  the same validated, rollback-safe in-place reload path as SIGHUP.
- [x] Reversible `ToggleDecorations` policy that preserves live client/rule
  preferences underneath the user override and updates frames and menus in place.
- [x] Independent horizontal and vertical maximize toggle actions that preserve
  the untouched axis and share the constrained EWMH geometry path.

## Milestone 3: daily-driver polish

- A modern theme schema and renderer with compatibility import tooling.
- A graphical settings application that edits the same validated TOML model.
- XSMP coordination, session packages, upgrades, diagnostics, crash recovery,
  and performance tests.
- [x] A recorded compatibility matrix for the complete current Openbox fixture
  corpus, including explicit pending and deferred contracts.

The roadmap is ordered by risk: protocol correctness and recoverability precede
visual polish. Each milestone should be usable in a nested server before it is
offered as a login session.
