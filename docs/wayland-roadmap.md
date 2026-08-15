# Wayland implementation roadmap

This document is the implementation plan for a native Nobox Wayland
compositor. It is not a promise to make Wayland look internally like X11, and
it is not permission to weaken the existing X11 baseline while the compositor
is built. [`architecture.md`](architecture.md) remains the ownership contract;
this roadmap defines the order of work, milestone exits, and final acceptance
boundary.

The planning baseline is Smithay 0.7 as published on 2026-08-14. Smithay is a
set of compositor building blocks, not a window manager, which is the right
division for Nobox: Smithay owns protocol and device mechanics while
`nobox-core` remains the source of window-management decisions. The initial
implementation pins a released Smithay version and enables only the features
used by the current milestone. It does not copy Anvil or Smallvil into this
repository.

## End result

This roadmap is complete only when all of the following are true:

1. The installed `nobox` binary can run either the existing X11 window manager
   or a native Wayland compositor. Backend selection is explicit and
   diagnosable; an X11 session never silently changes behavior because Wayland
   support was compiled in.
2. The Wayland backend runs both nested for safe development and directly on a
   logind/libseat-managed DRM/KMS session. It supports multiple connectors on
   one GPU, output hotplug, transforms, integer and fractional scaling,
   suspend/resume, VT switching, hardware or software cursors, shared-memory
   clients, and accelerated DMA-BUF clients.
3. Native `xdg_toplevel`, `xdg_popup`, layer-shell, and XWayland windows all
   enter the same protocol-neutral Nobox policy. Workspaces, placement, focus,
   modal/transient relationships, stacking layers, decorations, move/resize,
   minimize, maximize, fullscreen, show-desktop, menus, bindings, application
   rules, session restoration, and output fallback have the same documented
   user-visible meaning as on X11 wherever Wayland can represent it.
4. The Wayland session supplies the daily desktop protocols needed by ordinary
   applications: clipboard and drag-and-drop, primary selection, xdg
   activation, server-side decoration negotiation, layer shell, idle inhibit,
   relative and constrained pointers, pointer gestures, cursor shape,
   text-input/input-method, tablet and touch input, presentation feedback,
   viewporter, fractional scale, and session lock.
5. XWayland is optional at build and runtime but, when enabled, starts and dies
   with the compositor. X11 applications are managed beside native Wayland
   applications, use the same core identities and policy, and cannot become a
   second window-management authority.
6. `nobox-panel` remains an optional, separate process. Its X11 mode remains an
   EWMH client; its Wayland mode is a layer-shell client using advertised
   foreign-toplevel and workspace protocols. It keeps the existing replacement
   readiness handoff and never gains a dependency on compositor internals.
7. The Agent Seat wire protocol and grant model are unchanged. Under Wayland,
   compositor-owned capture and input make the existing visibility, consent,
   human-preemption, sensitive-client, and unforgeable-indicator promises
   enforceable. The companion is still a translator with no authority and can
   discover the seat only through an explicit socket or
   `AGENT_SEAT_SOCKET`; no fake X11 root advertisement is synthesized.
8. A failed panel, XWayland process, client, renderer import, configuration
   reload, output hotplug, or Agent Seat request cannot take down the
   compositor. Losing the session or graphics device pauses and recovers the
   direct backend cleanly. Protocol violations disconnect the offending client,
   not the desktop.
9. The CMake/Ninja workflow builds, checks, tests, installs, and diagnoses both
   backends. Deterministic nested-Wayland tests cover protocol behavior and
   shared policy outcomes; real-hardware dogfood covers DRM/KMS lifecycle. The
   X11 acceptance suite remains green throughout.

"A window appears in a nested compositor" is the exit for an early milestone,
not the end result.

## Goals

- Add a native compositor backend in a new `nobox-wayland` crate using
  Smithay's released APIs.
- Preserve `nobox-core` as a deterministic, display-server-neutral policy
  model. Smithay handles, Wayland resources, protocol serials, DRM nodes,
  buffers, and renderer types never enter it.
- Preserve the complete X11 baseline and use Openbox plus existing Nobox X11
  tests as behavioral oracles for shared window-management policy.
- Make nested bring-up the normal development path and direct DRM/KMS bring-up
  a separately testable realization of the same compositor state.
- Prefer official Wayland protocols. Use staging protocols where they are the
  current interoperability path and record their advertised versions. Use the
  established wlr protocols only for capabilities that have no suitable
  stable/staging equivalent, notably layer shell and interactive foreign
  toplevel management.
- Keep rendering, input, output, protocol, and policy state bounded and
  inspectable. Every asynchronous configure, import, frame, and device
  transition must have an explicit owner and failure path.
- Move configuration, Settings controls, panel behavior, documentation, tests,
  and installation together whenever a daily-use option becomes available on
  Wayland.
- Retain unsafe-free Nobox source. Smithay and its dependencies may contain
  audited unsafe code behind their safe public APIs; Nobox adds no `unsafe`.

## Non-goals for the first production Wayland baseline

- Replacing, deprecating, or feature-freezing `nobox-x11`.
- Turning `nobox-x11` into an in-process XWayland window manager. Smithay's
  XWayland/XWM integration is a separate protocol boundary inside
  `nobox-wayland`.
- Sharing one renderer, event loop, or display-server object model between X11
  and Wayland. The backends share policy results, not mechanics.
- A giant backend trait that pretends X11 reparenting and Wayland compositing
  are symmetric. Only the small process-control and session-handoff values
  required by the thin executable are shared.
- Forking Smithay, tracking its `master` branch, or copying Anvil as product
  code. A Smithay fork requires a separately approved blocker and an upstream
  issue.
- Implementing obsolete `wl_shell` or compositor-private protocols when a
  standard or established interoperable protocol exists.
- A built-in panel, desktop shell, notification daemon, launcher, lock-screen
  UI, settings daemon, portal implementation, display manager, or session
  manager. Nobox supplies the compositor side of the necessary protocols and
  keeps optional user interfaces out of its failure boundary.
- Remote desktop, network transport, VNC/RDP, a headless production backend,
  or nested compositor performance as a product feature. The nested backend is
  a development and test target.
- External screencopy, export-DMABUF, virtual-keyboard, virtual-pointer, or
  input-emulation globals in the initial baseline. Agent access remains behind
  the Agent Seat grant. Any later external capture/input protocol needs its own
  threat model and portal/session integration.
- HDR, wide-gamut color management, variable refresh rate, tearing control,
  direct scanout, DRM leasing, multi-GPU migration, explicit GPU reset
  recovery, or vendor-specific tuning in the first production baseline. The
  renderer design must not preclude them, but they land only after the ordinary
  composited path is correct and measured.
- Pixel-identical decorations between X11 core-font drawing and Wayland GPU
  rendering. Theme intent, extents, states, hit targets, and behavior must
  match; rasterization may differ by backend.
- Advertising protocol support before the corresponding lifecycle, security,
  and regression tests pass. Compiling Smithay support is not implementation.

## Architectural decisions

### Crate and process layout

The intended dependency direction is:

```text
                         ┌───────────────┐
                         │ nobox-config  │
                         └───────┬───────┘
                                 │ validated intent
┌───────────┐     ┌──────────────▼──────────────┐     ┌──────────────┐
│ nobox-x11 │────▶│ nobox-core + nobox-runtime │◀────│nobox-wayland │
└───────────┘     └──────────────┬──────────────┘     └──────┬───────┘
                                 │                           │ Smithay
                         ┌───────▼────────┐          ┌───────▼────────┐
                         │nobox-agent-wire│          │ Wayland clients│
                         └────────────────┘          └────────────────┘
```

- `nobox-wayland` owns the Wayland display, Smithay state/delegates, calloop
  loop, surface trees, scene elements, renderer, buffers, output devices,
  libinput devices, seats, protocol serials, XWayland process/XWM, and Wayland
  realization of the Agent Seat.
- `nobox-runtime` is a new, deliberately small crate. It owns only backend kind,
  typed run disposition, reload/shutdown/session-save control requests, a
  wakeable control handle contract, and neutral session handoff values. It owns
  no event loop, renderer, protocol resource, policy, or configuration parser.
  X11 adopts this boundary before Wayland depends on it.
- `nobox-core` continues to own clients, outputs, workspaces, focus, stacking,
  geometry, capabilities, presentation, reservations, application policy
  inputs, and Agent Seat authorization/state. Protocol-neutral additions are
  accepted only with pure tests and at least two real backend consumers.
- `nobox` selects a backend, loads configuration and saved state, runs
  autostart, forwards signals, supervises optional companions, and handles the
  backend's typed exit/restart result. It must not acquire Smithay state.
- `nobox-panel` gains separate X11 and Wayland client modules selected from its
  connection environment or an explicit backend argument. Wayland support may
  use Smithay Client Toolkit, but never `nobox-wayland`.

The runtime crate is not a general compositor framework. If a proposed type is
used only by Smithay callbacks it belongs in `nobox-wayland`; if it names an
X11 atom or request it belongs in `nobox-x11`.

### Backend selection and runtime control

`nobox run --backend x11` preserves today's behavior and accepts `--display`.
`nobox run --backend wayland` accepts `--nested-x11` or `--tty`; after the
direct backend is proven, plain Wayland startup selects `--tty`. During
development, omitting `--backend` continues to mean X11 so an upgrade cannot
unexpectedly claim a seat or DRM device. The installed Wayland session entry
always passes `--backend wayland --tty` explicitly.

The current X11 support-window wakeup is replaced at the process boundary by a
same-UID, mode-0600 UNIX control socket under `$XDG_RUNTIME_DIR/nobox/`. Each
backend may translate the typed request into its own event-loop wakeup. X11
retains EWMH verification for discovering its instance; Wayland uses a private
instance record created atomically beside the control socket. `nobox --exit`
must identify one backend/instance or fail rather than signal an arbitrary
process.

`nobox doctor --backend wayland` is read-only. Nested mode checks the parent
display and renderer; direct mode checks runtime-directory ownership,
logind/libseat availability, DRM render/card nodes, input discovery,
GBM/EGL/GLES, XWayland when enabled, and configuration/output-mode validity.
It never opens a Wayland listening socket, becomes DRM master, or takes a seat.

### Mapping Wayland objects into core policy

- One mapped `xdg_toplevel` or managed XWayland top-level corresponds to one
  `ClientId`. Popups and subsurfaces are backend-owned descendants, never
  independent core clients.
- A layer-shell surface with desktop or panel semantics contributes a typed
  desktop/dock role and an `EdgeReservation` where appropriate. It is absent
  from ordinary task and focus lists. Keyboard-interactive overlay surfaces
  are granted focus only according to layer-shell rules; they do not bypass
  focus policy by pretending to be ordinary clients.
- `xdg_toplevel.app_id`, title, parent, min/max size, decoration request, and
  state requests are translated into bounded application identity,
  capabilities, transient relationships, and policy transitions. The raw
  resources stay in the backend.
- XWayland `WM_CLASS`, role, transient, normal-hint, Motif, and EWMH inputs are
  translated independently by Smithay's XWM boundary. X11 resource IDs never
  become core or Agent Seat identities.
- A Wayland configure serial is backend state. Core chooses logical policy
  geometry and state; the backend records the pending configure, retains the
  last valid buffer until the client acknowledges and commits, and reports
  presented geometry accurately. A stalled or malformed client cannot stall
  other clients or cause speculative pixels to be reported as committed.
- Core geometry uses integer logical coordinates. Buffer scale, fractional
  scale, transform, viewport, physical pixels, damage, and rounding are
  renderer/output concerns. Every conversion has tested outward/inward rounding
  rules so work areas remain non-empty and adjacent output edges do not drift.
- Smithay's `Space` and scene helpers may index and render objects, but they do
  not become an independent focus or stacking policy. Core order is applied to
  the scene after every relevant transition and checked in debug/test builds.

### Protocol baseline

Globals are introduced in the milestone that owns their behavior; the complete
first-production set is:

| Area | Protocols/capability | Policy owner |
| --- | --- | --- |
| Surface foundation | `wl_compositor`, `wl_subcompositor`, `wl_shm`, damage and frame callbacks | Wayland backend lifecycle; core only after a shell role maps |
| Desktop shell | xdg-shell, xdg-decoration, xdg-dialog, layer-shell | Core role/state/geometry; backend configure and surface trees |
| Outputs | `wl_output`, xdg-output, viewporter, fractional-scale, presentation-time | Core topology/logical geometry; backend modes, scale, timing |
| Seat | `wl_seat` keyboard/pointer/touch, cursor-shape, relative-pointer, pointer-constraints, pointer-gestures, tablet-v2 | Core focus/actions; backend event delivery and grabs |
| Text and data | data-device/DND, primary-selection, text-input-v3, input-method-v2 | Backend protocol state with core focus authorization |
| Desktop integration | xdg-activation, idle-inhibit, ext-session-lock, ext-foreign-toplevel-list, ext-workspace-v1 | Core authorization/state; backend objects and atomic publication |
| Rendering | linux-dmabuf feedback plus DRM syncobj explicit synchronization where supported | Backend import, fences, damage, frame scheduling |
| Compatibility | XWayland shell, XWM, XWayland keyboard-grab | Backend translation into the same core clients |
| Panel compatibility | wlr-layer-shell and, until a standard interactive replacement is available, wlr-foreign-toplevel-management | Requests re-enter ordinary core action checks |

Protocol versions are pinned in code and listed by `nobox doctor`; requests
newer than the implemented version are never guessed. Staging protocol upgrades
receive the same review as a configuration schema change.

### Rendering and frame scheduling

The first renderer is Smithay's GLES2 path over EGL/GBM, with Pixman retained as
a deterministic software/test fallback. Rendering is damage-driven per output.
The scene is assembled from client surfaces, compositor decorations, menus,
switcher, consent UI, session-lock surfaces, cursor, and Agent Seat indicators
in an explicit order derived from core layers.

Only successfully imported buffers enter the scene. Buffer release, explicit
and implicit synchronization, frame callbacks, presentation feedback, output
enter/leave, and surface destruction are handled even when a frame is skipped.
Direct scanout is deliberately deferred; the ordinary composited path remains
the correctness oracle after later optimizations arrive.

Compositor-owned security UI is rendered last in a protected internal layer.
Ordinary Wayland and XWayland surfaces cannot receive its identifiers, cover
it, capture it through the Agent Seat, or be targeted while it owns a trusted
grab. Session-lock state has an equally explicit rendering and input boundary.

### Input and human precedence

libinput events and nested-backend events normalize into one backend input
path. Keymap compilation uses xkbcommon and the existing typed Nobox bindings;
the compositor consumes bindings before forwarding unhandled keys. Pointer
focus is derived from scene hit testing, while keyboard focus is the core's
selected client plus the active popup/grab rules.

Wayland serial provenance replaces X11 timestamp inference for activation,
move/resize, selections, and drag-and-drop. No client-supplied serial is trusted
without checking its seat, client, event class, and freshness. Pointer locks,
shortcut inhibition, popups, DND, compositor move/resize, menus, consent, and
session lock form an explicit priority order with cancellation tests.

The Agent Seat injects below the compositor's trusted-control layer and above
client delivery. Physical libinput events always preempt injected sequences;
the configured suppression window and kill chord remain authoritative even
under socket flood.

### Panel, session, and Agent Seat integration

The Wayland panel publishes a layer-shell exclusive zone and consumes
`ext-foreign-toplevel-list`, `ext-workspace-v1`, and the interactive
wlr-foreign-toplevel protocol where needed. Every requested activation, close,
minimize, maximize, fullscreen, or workspace transition is translated back
through normal core capability checks. The readiness pipe is signalled only
after the panel has connected, bound required globals, created its layer
surface, received its first configure, and committed a drawable buffer.

Saved session records remain protocol-neutral. Native matching prefers a
bounded `app_id` plus normalized title/role information and rejects ambiguous
duplicates; XWayland retains its existing stable X11 matching inputs behind the
backend boundary. Output persistence uses a stable connector/description key,
not a Smithay object or core session ID. XSMP remains X11/XWayland-specific;
native Wayland application relaunch is still the external session manager and
application's responsibility.

The Wayland compositor creates the same Agent Seat socket and uses the existing
wire crate/core grant state. It exports `AGENT_SEAT_SOCKET` to supervised
companions and autostart children. Capture reads the already composed scene with
scope filtering before encoding; hidden/redacted surfaces are omitted or
masked in the render pass rather than sampled and repaired afterward. Input is
surface-relative and verified against the current scene hit target/focus.
Consent, markers, highlights, capture, and mutations all have nested-Wayland
tests before the seat is advertised as supported.

## Local development baseline (2026-08-14)

The current Mageia development host has the required Smithay baseline:

| Component | Observed version/status |
| --- | --- |
| Rust | installed 1.95.0; Nobox declares and is checked with 1.87.0 |
| Smithay planning target | released 0.7.0; declares Rust 1.87 |
| Wayland client/server | 1.24.0 |
| xkbcommon | 1.13.1 |
| libinput / libudev | 1.30.3 / 258 |
| libseat | 0.9.2; active graphical logind seat |
| DRM / GBM | 2.4.133 / 26.0.8 |
| EGL / GLES | 1.5 / 3.2 |
| Pixman | 0.46.4 |
| XWayland | `/usr/bin/Xwayland`, package 24.1.13 |
| DRM nodes | card and render nodes present and ACL-accessible in the active session |

`libdisplay-info` runtime libraries are installed but no
`pkgconfig(libdisplay-info)` provider is present. It is not part of Smithay's
documented baseline and is not a bring-up blocker; if later output metadata or
color work requires it, the matching development package becomes a dependency
of that later milestone.

Smithay 0.7's Rust 1.87 minimum means W0 must raise Nobox's declared workspace
MSRV from 1.85 to 1.87 and prove the complete workspace with that exact
toolchain. The host's newer compiler is not sufficient evidence by itself.

Primary references for W0 are the released
[Smithay crate documentation](https://docs.rs/smithay/0.7.0/smithay/), the
[Smithay feature manifest](https://github.com/Smithay/smithay/blob/v0.7.0/Cargo.toml),
and the upstream [Anvil dependency/run notes](https://github.com/Smithay/smithay/blob/v0.7.0/anvil/README.md).

## Milestones

Milestones are intentionally vertical. Each ends in observable behavior and a
test gate, not merely new modules. A milestone may be split into smaller commits
while under development, but it is not accepted until its full exit is met.

### W0: dependency and event-loop proof

Status: complete (2026-08-14; `nobox-wayland` 0.2.1).

Deliverables:

- Pin Smithay 0.7 with `default-features = false`. W0 enables only
  `wayland_frontend`, `desktop`, `use_system_lib`, and `renderer_pixman`.
  EGL/GBM/GLES and Smithay's direct/nested device backends remain gated until
  the milestone that owns their complete lifecycle.
- Raise the workspace MSRV to 1.87 and verify with an actual 1.87 toolchain as
  well as the development toolchain.
- Add the new `nobox-wayland` crate, a private Wayland socket, calloop display
  dispatch, SHM, one synthetic output, and a deterministic clear-color frame in
  a window on a nested X server.
- Add `nobox doctor --backend wayland --nested-x11` dependency diagnostics and
  `NOBOX_BUILD_WAYLAND`, defaulting off until W9, so Wayland can be omitted
  without changing X11 artifacts. The Wayland crate is still checked
  explicitly while it is experimental.
- Record exact Smithay features and system package requirements. Keep a
  dependency license/audit report with the milestone.

Exit:

- A protocol probe connects, lists only the deliberately enabled globals, and
  disconnects cleanly; ten start/stop cycles leak no socket or child process.
- The proof runs under isolated Xvfb/Xephyr in CTest without touching the host
  Wayland session or DRM devices.
- The complete existing X11 suite passes at the raised MSRV.

This milestone manages no application window and installs no Wayland session
entry.

Acceptance evidence: Rust 1.87.0 and the development toolchain both check the
complete workspace; the W0 CTest protocol/lifecycle proof passes ten cycles on
isolated Xvfb; and the complete X11 CTest suite reports no failures when run on
Xvfb (four capability-dependent tests skip on that server). The default nested
server order also exposed an existing Xnest-only ParentRelative pixel failure
in `x11-frame-smoke`; it is not caused by the Wayland crate and remains visible
rather than being reclassified as a Wayland failure.

W0 deliberately uses Smithay's safe Pixman renderer and the existing x11rb
transport for its test window. Smithay 0.7's low-level X11 plus EGL/GLES path
requires caller-side `unsafe` during renderer initialization, which violates
Nobox's unsafe-free source rule. Smithay's safe winit wrapper also permits
selection of a host Wayland compositor and expands the dependency/license
surface for a proof that must be pinned to isolated X11. The W0 transport is
not product rendering architecture; accelerated renderer selection is an
explicit W4 gate. See [`wayland-dependencies.md`](wayland-dependencies.md).

### W1: neutral runtime and backend selection

Status: complete (2026-08-14; `nobox-runtime` 0.2.1, `nobox` 0.2.2,
`nobox-x11` 0.2.2, `nobox-wayland` 0.2.2, `nobox-settings` 0.2.1).

The process boundary is now protocol-neutral. X11 retains its verified EWMH
discovery chain but publishes only an opaque runtime identity; X11, Wayland,
signals, XSMP, and Settings all route typed requests through the private Unix
endpoint. The nested Wayland loop receives those requests through a calloop
channel and demonstrated a 3 ms remote-exit wake in local acceptance testing.
The `runtime-control` regression covers both backends, prompt reload/exit,
mode/cleanup, exact-instance selection, and ambiguity refusal; unit coverage
rejects wrong ownership attributes, symlinks, stale PIDs, and overlong paths.
The final W1 gate passed the CMake build/check workflow and all 52 runnable
nested-X tests; four extension-dependent Xvfb cases reported their established
capability skips.

Deliverables:

- Add `nobox-runtime` and move the minimal run disposition, control requests,
  control wakeup, and neutral session handoff out of `nobox-x11`.
- Make X11 use the new boundary without behavioral change. Replace
  X11-specific signal forwarding in the CLI with typed runtime requests while
  retaining X11's verified discovery behavior.
- Add explicit backend CLI selection, unambiguous remote exit, per-instance
  runtime files, backend-aware diagnostics, and backend-aware panel
  supervision. Default startup remains X11.
- Define backend capabilities as small typed data used by diagnostics,
  Settings, and Agent Seat status; do not add a broad window-manager trait.

Exit:

- Existing X11 restart, exit, reconfigure, XSMP, panel replacement, session
  restore, and Agent Seat tests pass unchanged in observable behavior.
- Wayland skeleton reload/exit wakes calloop immediately without polling.
- Runtime files reject wrong ownership, symlinks, stale PIDs, ambiguous
  instances, and overlong paths.

### W2: nested native shell

Status: complete in `nobox-wayland` 0.2.3, `nobox-runtime` 0.2.2, and `nobox`
0.2.3.

Deliverables:

- Implement compositor/subcompositor, SHM, xdg-shell toplevels and popups,
  xdg-decoration, output publication, seats, keyboard/pointer focus, cursor,
  frame callbacks, damage, and clean surface/client teardown.
- Map each xdg toplevel into `nobox-core`; implement initial placement,
  configure/ack/commit tracking, focus/raise, close, interactive move/resize,
  min/max constraints, maximize, fullscreen, minimize, and transient/modal
  popup behavior.
- Render client surfaces and minimal server-side decorations with GLES2 plus a
  Pixman/test fallback. Keep all Smithay/Wayland values in `nobox-wayland`.
- Add a deterministic in-tree Wayland test client rather than depending on a
  desktop toolkit's timing.

Exit:

- Two native test clients can map, redraw, create/destroy popups and
  subsurfaces, change constraints, move/resize, maximize/fullscreen/minimize,
  transfer focus, and exit without stale core or scene entries.
- Tests cover out-of-order/missing configure acknowledgements, buffer removal,
  invalid roles, disconnect during a grab, and one unresponsive client while a
  second remains interactive.
- Shared policy assertions match the X11 outcome for every implemented action.

Gate evidence (2026-08-14): `wayland-managed-shell` runs ten isolated nested
compositor lifecycles. It forces GLES2 once and Pixman once, exercises two
simultaneous native clients on every cycle, and covers xdg toplevels, popups,
subsurfaces, SHM redraw/frame callbacks, SSD, focus and keyboard delivery,
serial-authorized move/resize, min/max constraints, maximize, fullscreen,
minimize, close, client cursor surfaces, buffer removal, invalid configure
order, invalid roles, and disconnect during a popup grab. A deliberately
unresponsive client remains mapped while a second client takes focus, receives
pointer/keyboard input, and completes its shell actions. Every run removes its
Wayland socket and runtime record. The backend maps native toplevels to
`nobox-core::ClientSet` and uses the core resize constraint function; protocol
objects remain confined to `nobox-wayland`.

### W3: Nobox desktop policy and compositor UI

Status: complete (2026-08-15) in `nobox-wayland` 0.2.14, `nobox-core` 0.2.2,
`nobox-config` 0.2.1, `nobox-runtime` 0.2.4, `nobox-x11` 0.2.7, and
`nobox` 0.2.5.

Deliverables:

- Apply the complete core workspace, focus history, stacking layers,
  show-desktop, placement, snapping, directional actions, application rules,
  per-axis maximize restoration, shading, and output-selection behavior to
  native surfaces.
- Implement themed decorations and hit targets, menus, switcher, confirmation
  dialogs, key sequences, mouse bindings, urgency/attention, launch actions,
  xdg activation, configuration reload, and restart/session snapshot flow.
- Implement layer shell and translate exclusive zones into the existing core
  reservation/work-area calculation.
- Add `ext-foreign-toplevel-list` and `ext-workspace-v1` publication from core
  state. Requests are atomic where the protocol requires it.

Exit:

- The default configuration drives a complete nested policy/UI work session
  with deterministic native clients and baseline-compatible applications.
  Ordinary toolkit terminal/editor/browser acceptance belongs to W5 because
  those clients require its data-device and daily-application protocols.
- A backend-parity matrix maps every current action/config option to
  `native`, `XWayland-only`, `documented fallback`, or `intentionally
  unsupported`; no entry is silently ignored.
- Live reload preserves the last good config and reconfigures resources without
  dropping clients. Restart restores matched clients without rerunning
  autostart.

Foundation evidence (2026-08-14): the native backend now loads and live-reloads
the strict `nobox-config` model while retaining the last good configuration.
Mapped toplevel metadata is bounded and translated into core roles,
transient/modal relationships, focus, workspace, stacking, decoration,
minimize/shade, skip-list, maximize/fullscreen, size, and absolute-placement
policy. Theme border/titlebar dimensions and state colors drive the initial
server-side decoration pass. `xdg-activation` validates the requesting client,
seat, recent input serial, and a five-second freshness bound before restoring,
focusing, and raising through `ClientSet`.

The compositor also publishes `ext-foreign-toplevel-list` and
`ext-workspace-v1`; workspace activation requests are accumulated until the
manager's commit and applied atomically. Wlr layer-shell surfaces are
configured, rendered in Smithay's scene order, focus-filtered by keyboard
interactivity, and translated through `nobox-core::EdgeReservations` before
placement or maximize computes a work area. The nested regression probe checks
foreign-toplevel map/unmap, valid activation, atomic workspace switch/restore,
and a drawable 32-pixel exclusive-zone layer surface on the GLES2 and Pixman
paths. Later W3 tranches completed the action/binding, compositor UI, urgency,
restart, and parity work described below.

Keyboard/action evidence (2026-08-15): nested input now resolves the existing
typed key-binding model from XKB modifier state and both raw and modified
keysyms. Complete chords are consumed before client delivery; multi-chord
prefixes retain only viable bindings, honor the configured timeout and quit
chord, and reset on live reload. Matching key releases are consumed as well, so
clients never observe a release without its compositor-owned press. The nested
close probe now reaches `xdg_toplevel.close` through the default `Super-q`
binding instead of a backend-only escape-key shortcut.

The initial Wayland action executor covers shell launches, screenshots,
reconfigure/debug, close/kill, core focus and stacking, minimize, per-axis
maximize, fullscreen, layers, decorations, sticky/shade/show-desktop, relative
move/resize, MRU cycling, workspace navigation/movement/addition/removal, and
unprompted exit. It uses only `nobox-config` actions, `nobox-core` policy, and
Wayland protocol effects; no X11 action representation entered the backend or
core.

Action-policy evidence (2026-08-15): conditional and per-client iteration now
evaluate native metadata and state through `ActionQueryContext`, preserving
ordered nested actions and `stop` flow. Directional focus, edge movement and
growth/shrink/fill, relative and absolute move/resize, output selection for the
current single-output backend, centering, and directional cycling all delegate
to the existing core geometry/selection algorithms. Decoration extents are
translated only at the Wayland boundary. The nested probe exercises default
workspace switching and restoration through `Super-Right`/`Super-Left` and
observes the corresponding atomic workspace protocol state.

Interactive-action evidence (2026-08-15): `Move` and `Resize` enter a native,
cancellable keyboard operation. Arrow keys move or select/adjust a resize
edge, Control selects one-pixel steps, Shift moves directly to an output edge,
Return commits, and Escape restores the original geometry. Matching events are
held at the compositor boundary and resize state/configures follow xdg-shell.
The display-neutral keyboard movement calculation moved from `nobox-x11` into
`nobox-core`; the X11 adapter and Wayland compositor now share it. A configured
nested regression launches the real `nobox --backend wayland` path, binds
`Super-r` to `Resize`, and observes horizontal growth through xdg configure.

Pointer/decorations evidence (2026-08-15): `nobox-config` now owns the one
exact-to-general mouse-context chain consumed by both protocol backends.
Wayland scene hit testing resolves content, titlebar buttons, titlebar, and
individual frame edges/corners into those typed contexts. Press, release,
click, double-click, thresholded drag, exact modifiers, and discrete vertical
wheel bindings dispatch the existing ordered action trees; unclaimed input is
still delivered to the focused client. Pointer-originated move and resize keep
their invocation through nested conditionals, use explicit or hit-inferred
edges, and preserve xdg resize state. Titlebar button backgrounds and glyphs
are themed and share their geometry with input hit testing. The configured
nested regression proves a `Super`-drag resize, a root-wheel workspace action,
and a rendered close-button click through the real `nobox --backend wayland`
path.

Interactive-policy parity evidence (2026-08-15): pointer resize edge selection,
delta geometry, work-area resistance, and overflow handling now live in
`nobox-core`; X11 and Wayland translate their protocol edge enums into that
one typed policy. Wayland pointer moves snap decorated outer geometry to the
work area and, when configured, to visible peer frames using the same core
algorithms and stacking-order tie breaking as X11. Non-strict show-desktop now
ends when a placement-occupying native role maps, while strict mode keeps new
ordinary clients hidden; the shared role classification prevents backend
drift.

Compositor-UI evidence (2026-08-15): the Wayland backend loads the configured
font family from the system with bounded sans-serif fallbacks, rasterizes it in
safe Rust, and keeps a bounded glyph cache. Window titles honor theme text
color, padding, and left/center/right alignment while clipping before titlebar
buttons. Modifier-held forward and reverse focus cycles preview clients without
changing stacking; release commits with the configured raise policy and Escape
restores the original focus. A compositor-owned title list and selected-window
outline render in an explicit overlay pass above client surfaces. The nested
two-client regression proves focus delivery, Shift release handling,
cancellation, and overlay pixels through the GLES2 path; the ordinary suite
continues to exercise title rendering through both GLES2 and Pixman.

Menu evidence (2026-08-15): native compositor overlays now render configured
titles, items, separators, selection, and submenu affordances with the same
bounded theme and font path as titlebars. Keyboard navigation, accelerators,
pointer selection, outside-click dismissal, and wheel movement are retained at
the compositor boundary instead of leaking to clients. Static menus and the
generated client, workspace, and window sources dispatch ordinary typed
actions; prompted Execute and Exit actions reuse the same confirmation UI. A
nested client opens the default Alt-Space client menu, proves overlay pixels,
selects Close, and observes `xdg_toplevel.close`.

Dynamic-menu evidence (2026-08-15): command menus use one display-neutral
`nobox-runtime` runner shared by X11 and Wayland, with a private output file,
deadline, exit-status check, 64 KiB read bound, and strict UTF-8/TOML parsing.
The applications source consumes the bounded `nobox-desktop` catalog, builds
bounded inline category menus, and launches its already parsed argument vector
without a shell while exporting the compositor's actual `WAYLAND_DISPLAY`.
The nested suite generates a command menu that closes a native client and a
one-entry XDG catalog whose launch creates a deterministic marker.

Session-lifecycle evidence (2026-08-15): a native clean shutdown now returns
the same protocol-neutral snapshot and typed exit/restart disposition used by
the X11 process boundary. `nobox` owns persistence, runtime save requests,
single-shot autostart, in-process restart, and replacement-command handoff.
Native matching uses normalized bounded application id, title, role, and kind;
the shared restore layer discards ambiguous duplicates before a backend can
consume them. Restored clients recover workspace, geometry, presentation,
layer, decoration, maximize/fullscreen, focus, and relative stacking state.
The nested regression resizes and moves a real native client, restarts on the
same socket without rerunning autostart, verifies the saved client on remap,
checks clean command handoff/socket release, and exercises unprompted session
logout. Because Winit permits only one GLES event-loop initialization per
process on this nested path, an in-process restart hides that old host window
and uses the independent Pixman/X11 host; this is confined to the non-product
nested backend.

Final W3 evidence (2026-08-15): invalid or stale activation requests visibly
mark a nonfocused client urgent, and ordinary focus clears that state. The
`prevent_focus_stealing` option selects strict recent-seat/serial validation or
fresh known-token acceptance; `follow_mouse` focuses native content and frame
entries with the configured raise policy. Compositor-issued launch tokens are
five-second bounded, capped at 256, exported as `XDG_ACTIVATION_TOKEN` and
`DESKTOP_STARTUP_ID`, and distinguished from untrusted client tokens.

Directional cycling now uses the same modifier-held preview, release commit,
and Escape restoration lifecycle as linear cycling. Runtime menu trees paginate
recursively through bounded `_More...` submenus, including command-generated
and application-category content. The nested test observes all of these paths,
including actual launch environment values and visible urgent/overlay pixels.
The exhaustive [`wayland-parity.md`](wayland-parity.md) matrix accounts for
every action and public config leaf, naming the W4-W8 owner for each deliberate
fallback or unsupported row. A native Alacritty smoke also maps successfully;
GTK4 correctly declines W3 because its required data-device interfaces are not
advertised early, so real toolkit terminal/editor/browser acceptance remains
the explicit W5 exit rather than a false W3 claim.

### W4: real DRM/KMS and multi-output operation

Status: in progress in `nobox-wayland` 0.2.21, `nobox-runtime` 0.2.5,
`nobox-config` 0.2.2, and `nobox` 0.2.7.

Direct-foundation evidence (2026-08-15): the pinned Smithay build now enables
libseat session, udev, libinput, DRM, GBM, multi-renderer, and GLES features.
The default direct doctor validates the strict config and private runtime
directory, selects a bounded seat/session identity, enumerates seat-scoped DRM
cards plus render/input nodes, checks effective kernel access, and locates the
optional XWayland binary without opening or claiming any of them. On the
development host it reports `seat0`, one accessible card/render pair, 24 input
event nodes, and the installed XWayland. The nested doctor remains explicitly
selected with `--nested-x11`; the direct capability stays false until the
actual libseat/DRM run lifecycle passes.

The first topology-policy tranche adds a strict protocol-neutral `[outputs]`
model keyed by bounded connector names. Mode strings preserve optional
millihertz refresh, positions and all eight transforms are typed, fractional
scales are exact 1/120 units, and duplicate/disabled primary selections are
rejected before any backend mutation. Empty rules retain automatic preferred
mode and layout behavior. Hardware application and friendly Settings controls
remain part of the active W4 work rather than being claimed by this tranche.

The matching backend planner resolves those rules against a bounded DRM
connector/mode inventory before touching hardware. It selects exact requested
modes or deterministic preferred fallbacks, derives transformed fractional
logical sizes, normalizes a primary output, and refuses zero-output,
unavailable-mode, duplicate-connector, or overflowing candidates so the live
backend can keep its last working topology atomically.

The first executable direct tranche is now selected only by the explicit
`--backend wayland run --tty` combination. It acquires libseat, opens the DRM
device through the session, initializes GBM/GLES through Smithay's safe
`GbmGlesBackend`, scans connector/CRTC assignments, applies an exact
single-output candidate, registers libinput, and drives composited native
surfaces plus server UI from KMS/vblank. Session pause suspends libinput and
DRM; activation resumes them while retaining the existing `Compositor` and
core client policy. Direct autostart receives the private `WAYLAND_DISPLAY`
and has `DISPLAY` removed until W7. An isolated regression forces an invalid
libseat backend and proves startup refuses cleanly without opening devices.

The compositor scene tranche removes the former single synthetic-output
ownership from shared Wayland state. Outputs now carry independent logical
geometry and primary status, are mapped at their topology positions, and own
their layer-shell maps; layer hit testing, frame callbacks, arrangement, and
cleanup follow that association. Relative pointer motion is confined to the
nearest real output rectangle even across negative coordinates and layout
gaps, while absolute devices target the primary output including its logical
origin. Deterministic unit coverage constructs a two-output disjoint scene and
also proves primary normalization without opening DRM devices.

The matching direct-surface tranche now consumes every planned connector
instead of refusing multi-output candidates. Each connector owns an independent
Smithay output, CRTC, `DrmOutput`, damage history, frame-pending bit, and vblank
completion path; logical positions, transforms, fractional scales, and primary
selection are published before the scene maps them. Frames are assembled and
queued per output, and callbacks are completed only for the selected output's
windows and layer surfaces. Pause and resume cover the complete KMS set without
reconstructing compositor policy.

The first live-topology transaction is deliberately hardware-independent.
Replacing the scene topology withdraws removed output globals, emits Space
leave/enter updates, remaps layer surfaces to the surviving primary, cancels
stale interactive operations, confines the pointer, and reflows off-screen,
maximized, and fullscreen clients through `nobox-core` geometry policy. Direct
configuration reload applies position and primary changes only after planning
the complete candidate. Connector-set, mode, transform, or scale changes are
refused without disturbing the live topology until the matching KMS rollback
transaction exists. A disjoint two-output unit scenario removes the output
under the pointer and verifies scene unmapping and deterministic confinement.

Udev changes now drive the retained DRM scanner instead of ending at a log
message. Disconnected connector/CRTC pairs are pruned immediately, additions
are planned as one complete candidate and initialized before their output
globals become visible, and partial addition failure drops the provisional KMS
surfaces while keeping every surviving output usable. A successful candidate
is reordered to planner order and enters the scene through the same replacement
transaction. If the last physical output disappears Nobox exits the direct
session cleanly; if an addition or candidate fails while another output
survives, the compositor stays running on the survivor. Deterministic delta
coverage fixes removal/addition ordering without requiring a fake DRM device.

This is not the W4 exit: initial multi-connector KMS creation has compile-time
and emulated topology coverage but no disposable-VT hardware record yet, udev
application still needs the disposable-VT unplug/replug record, output
configuration reload does not yet mutate KMS mode/transform/scale state, and
an existing connector whose CRTC or scanout properties change is refused until
mode-change rollback exists. `direct_session` therefore remains false. The next
tranche owns that KMS mode/transform/scale rollback.

Deliverables:

- Add the libseat session, udev discovery, libinput, DRM/KMS, GBM/EGL/GLES,
  render-node, and multi-connector backend. Session pause releases devices;
  resume reconstructs them without reconstructing policy state.
- Implement connector add/remove, mode selection, transforms, layout, primary
  output, integer/fractional scale, output enter/leave, cursor fallback, damage
  tracking, vblank-driven frame scheduling, DMA-BUF feedback, and supported
  explicit synchronization.
- Add strict `[outputs]` configuration and friendly Settings controls for
  enabled state, mode, position, transform, scale, and primary output. Invalid
  configurations retain the last working topology and always preserve at least
  one usable output.
- Reflow clients through existing core output-removal and work-area policy.

Exit:

- On a disposable VT, Nobox starts unprivileged through logind/libseat, runs
  native accelerated and SHM clients, switches away/back, suspends/resumes, and
  exits restoring the seat and devices.
- Real-hardware tests cover two connectors, hot-unplug/replug, mixed scale,
  transform, mode failure rollback, renderer import failure, and a connector
  disappearing during interactive move/resize.
- The nested suite emulates topology changes deterministically; hardware-only
  checks have a documented manual/dogfood record rather than fake CI claims.

### W5: daily application protocols and secure lock

Status: planned.

Deliverables:

- Implement clipboard, primary selection, DND, data-offer cancellation,
  relative/constrained pointers, gestures, cursor shape, touch, tablet,
  text-input/input-method, shortcut inhibition, idle inhibit, presentation
  feedback, viewporter, and fractional scale completely.
- Implement `ext-session-lock` as a privileged compositor state: all outputs
  covered, no ordinary surface rendered or focused above it, output changes
  handled while locked, and failure leaves a secure blank/locked state rather
  than revealing the session.
- Add protocol-global/version reporting and resource bounds per client for
  surfaces, popups, SHM pools, offers, devices, callbacks, and pending
  configures.

Exit:

- GTK, Qt, SDL, Electron/Chromium, and a text-input client pass focused nested
  smoke scenarios without XWayland.
- Clipboard/DND owner death, popup storms, invalid serials, pointer-lock loss,
  IME death, and lock-client death have deterministic regressions.
- The compositor remains responsive under bounded hostile-client fixtures and
  disconnects only the offender.

### W6: Wayland panel mode

Status: planned.

Deliverables:

- Add a layer-shell Wayland frontend to `nobox-panel`, sharing its canonical
  `[panel]` model, XDG launcher parsing, component order, task filtering,
  interaction settings, and clock formatting with the X11 frontend.
- Consume `ext-foreign-toplevel-list` and `ext-workspace-v1`; implement the
  established wlr foreign-toplevel management requests required by the current
  task buttons. Advertise only the operations core says a client supports.
- Preserve readiness replacement across backend modes. Panel loss removes its
  layer surface/reservation and never affects compositor input or rendering.

Exit:

- All daily-use panel options have matching Settings controls, docs, unit tests,
  and nested-Wayland tests.
- Workspace buttons, task scope, activate/minimize/close, launchers, clock, live
  reconfigure, crash, and failed replacement behave like the X11 panel's
  documented contract.
- The panel has no dependency on `nobox-wayland`, `nobox-core`, or compositor
  private sockets.

### W7: XWayland compatibility

Status: planned.

Deliverables:

- Start XWayland on demand through Smithay, own its XWM, publish its environment
  only after readiness, and tear it down independently of native clients.
- Translate managed XWayland top levels into the same core roles, identities,
  capabilities, transients, size constraints, state, workspaces, stacking, and
  focus. Override-redirect surfaces remain backend-owned unmanaged scene
  elements with bounded stacking rules.
- Bridge clipboard/DND and activation/focus semantics without creating two
  authorities. Apply scale and coordinate conversion at the boundary.
- Select relevant existing Openbox/Nobox X11 fixtures and rerun their
  user-visible outcomes through XWayland; do not claim that an XWayland XWM is
  a full root-window WM.

Exit:

- `xterm` plus representative GTK/Qt X11 clients coexist with native clients
  across focus, workspaces, transients, menus, resize constraints, clipboard,
  restart, and XWayland crash/restart.
- XWayland crash removes only its clients and can be restarted; native clients,
  panel, compositor UI, control, and Agent Seat remain usable.
- X11 IDs/atoms are absent from core, session wire, and Agent Seat payloads.

### W8: Agent Seat realization under Wayland

Status: planned.

Deliverables:

- Reuse `nobox-agent-wire`, core grants/events/generations, config, and companion
  unchanged; implement Wayland peer identity, discovery environment, scene
  capture, surface-relative input, activation/management, indicators, consent,
  and human-preemption realization.
- Mask hidden/redacted content during scene rendering for output capture;
  authorize client capture against the exact committed surface tree. Never
  expose session-lock or compositor security UI.
- Correlate accessibility helper results to native clients through verified
  Wayland client credentials/process identity without exposing PIDs or
  Wayland object IDs on the wire.
- Report backend capabilities honestly. Features stay absent from the handshake
  until their end-to-end tests pass.

Exit:

- The existing harness flow—snapshot/subscribe, launch correlation, activate,
  stale-state rejection, pointer/key/type, capture, human interruption, freeze,
  revoke—passes in one nested-Wayland test.
- Hidden/redacted overlap, obscured capture, output masking, lock state, popup
  coverage, user input races, helper failure, disconnect, and renderer failure
  fail closed while the compositor stays responsive.
- The same companion operates against X11 and Wayland from the documented
  explicit/environment discovery paths with no backend-specific MCP surface.

### W9: hardening, dogfood, and release acceptance

Status: planned.

Deliverables:

- Run protocol/resource stress, repeated startup/shutdown, suspend/resume,
  output churn, XWayland churn, panel replacement, Agent Seat flood, memory and
  frame-latency profiling, and long-running dogfood on the development machine.
- Add an installed Wayland session entry only after `nobox doctor --backend
  wayland --tty` passes. Preserve the X11 session entry and an obvious fallback
  path.
- Complete usage, configuration, architecture, security, troubleshooting,
  protocol support, performance, and release documentation.
- Audit enabled Smithay features, dependency licenses/advisories, runtime file
  permissions, protocol globals, logging redaction, and absence of unsafe Nobox
  code.

Exit:

- All nine end-result statements at the top of this document are demonstrated
  by automated evidence or an explicitly named real-hardware dogfood record.
- `cmake --preset dev`, `cmake --build --preset dev`, `cmake --build --preset
  check`, and `/usr/bin/ctest --preset dev --output-on-failure` pass with both
  backends enabled; X11-only builds and Wayland-without-XWayland builds also
  pass.
- The source install contains the selected binaries, both session entries,
  example configuration, and documentation; a staged install passes nested
  X11 and nested Wayland smoke tests.
- A clean direct Wayland exit returns control of the seat, DRM devices, input
  devices, runtime sockets, and XWayland child with no manual recovery.

## Verification strategy

### Test layers

1. **Core unit tests:** display-neutral state transitions, geometry, focus,
   stacking, output topology, reservations, configure intent, application
   policy, session matching, and Agent Seat authorization.
2. **Wayland state/unit tests:** serial validation, resource bounds, role
   transitions, scale/coordinate rounding, configure queues, damage, output
   publication, protocol capability mapping, and renderer-independent scene
   order.
3. **Nested protocol tests:** an isolated Xvfb/Xephyr hosts Smithay's nested X11
   backend; a private `WAYLAND_DISPLAY` hosts deterministic in-tree clients and
   selected real toolkit clients. Tests observe Wayland events and rendered
   results, not implementation fields.
4. **Backend parity scenarios:** one declarative scenario expresses a policy
   outcome and has X11 and Wayland drivers. Protocol assertions remain separate
   so parity does not erase backend differences.
5. **XWayland tests:** a private XWayland launched by the test compositor hosts
   selected existing X11 fixtures and simple applications.
6. **Real hardware:** a disposable VT/logind session covers DRM master/session
   lifecycle, hotplug, mixed scale, suspend, and performance. It is never
   replaced by a nested test claim.
7. **Optional ecosystem suites:** add WLCS when its runner is available and its
   cases match advertised protocols. WLCS supplements, not replaces, Nobox
   policy and security regressions.

Every failure log records backend, renderer, Smithay version, advertised
globals, output topology, and test socket paths without leaking clipboard,
titles from hidden clients, Agent Seat payloads, or captured pixels.

### Standing gate for every milestone

- Update this roadmap's milestone status/evidence and amend architecture,
  configuration, usage, security, and acceptance docs in the same change as
  behavior.
- Add focused unit and integration coverage for every behavior change. Keep
  the full existing X11 gate green.
- Run the developer workflow exactly as documented, using `/usr/bin/ctest`.
- Increment the patch version of every crate changed by the accepted milestone;
  do not change major or minor versions without an explicit request.
- Preserve unrelated work and `tmp/`. Generated artifacts remain ignored.
- Commit and push the verified milestone to `origin/main`. Tags and source
  releases are created only for explicitly selected release milestones, not
  every patch increment.

## Risks and stop conditions

- **Policy leakage:** a Smithay/Wayland/DRM type reaching `nobox-core` stops the
  milestone until the translation boundary is repaired.
- **False parity:** if Wayland cannot implement an X11 behavior honestly, add a
  typed capability and documented fallback; do not emulate atoms or lie to
  config/Agent Seat consumers.
- **Configure races:** no milestone advances while core, rendered, and reported
  client geometry can disagree without a defined pending state.
- **Security UI:** Agent Seat consent/indicator or session-lock surfaces that
  ordinary clients can cover, target, or capture block dogfood.
- **Unbounded resources:** any client-controlled collection, buffer dimension,
  pending callback, protocol object, or message without a tested bound blocks
  protocol advertisement.
- **Device recovery:** direct mode does not become the default until VT switch,
  logind pause/resume, renderer loss, and clean exit are repeatable on the
  development host.
- **Smithay churn:** update only between accepted milestones. A release upgrade
  requires reading its changelog, rerunning all compositor gates, and recording
  changed protocol/backend behavior.
- **X11 regression:** a shared extraction that changes an X11 observable is
  either fixed or documented as a separately approved behavior change before
  Wayland work continues.

## Original implementation slice

The first code milestone was W0 only. It proved the pinned dependency, raised
MSRV, calloop integration, private socket, nested renderer, diagnostics, and
clean shutdown. It deliberately did not manage a client, advertise a session
entry, touch DRM/libinput, start XWayland, or move policy out of X11. That small
proof gave the next milestone a trustworthy foundation without allowing a
demo window to be mistaken for a compositor architecture.
