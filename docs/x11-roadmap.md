# X11 roadmap

Openbox remains the behavioral reference. Its programs under `../openbox/tests`
are a valuable catalog of cases, but each behavior needs a deterministic nobox
test and an explicit compatibility decision.

## Milestone 0: running skeleton (current)

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
- [x] Iconic lifecycle and synthetic-unmap handling, verified with the upstream
  Openbox `mapiconic` and `fakeunmap` programs.
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
- [x] Mutually exclusive EWMH above/below state in a protocol-neutral ordered
  layer model, enforced against observed X11 stacking.
- Interactive move/resize cancellation, snapping, resistance, and constraints.
- Clean signal shutdown and in-place config reload.

Initial Openbox regression cases: `aspect`, `fakeunmap`, `focusout`, `grav`,
`groupmodal`, `mapiconic`, `mingrow`, `modal*`, `noresize`, `resize`, `stacking`,
`strut`, `urgent`, and `wmhints`.

## Milestone 2: Openbox-class workflows

- Multiple desktops, desktop layout, per-desktop focus history, and window moves.
- Remaining dock/desktop integration and transient-family layer edge cases.
- Application rules matched by class/name/role/type.
- Keyboard and mouse chording equivalent to Openbox's useful action model.
- Menus, focus cycling UI, placement policies, and multi-monitor behavior.

## Milestone 3: daily-driver polish

- A modern theme schema and renderer with compatibility import tooling.
- A graphical settings application that edits the same validated TOML model.
- Session packages, upgrades, diagnostics, crash recovery, and performance tests.
- A recorded compatibility matrix for the Openbox regression suite.

The roadmap is ordered by risk: protocol correctness and recoverability precede
visual polish. Each milestone should be usable in a nested server before it is
offered as a login session.
