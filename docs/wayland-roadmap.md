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

Status: in progress through `nobox-wayland` 0.2.65, `nobox-runtime` 0.2.7,
`nobox-config` 0.2.5, `nobox-settings` 0.2.2, and `nobox` 0.2.37.

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
mode and layout behavior. The direct backend consumes this model through the
transactional paths described below.

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
core client policy. Direct autostart receives the private `WAYLAND_DISPLAY`;
when XWayland is enabled, startup waits at most five seconds for its readiness
handoff and supplies the resulting real `DISPLAY`, otherwise `DISPLAY` remains
absent. An isolated regression forces an invalid libseat backend and proves
startup refuses cleanly without opening devices.

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
Scanner CRTC reassignment is validated only after connector presence pruning:
a still-connected output therefore remains live until the unsupported
assignment change is rejected, rather than being silently discarded and
misclassified as a new connector.

Direct reload now waits until every in-flight output frame has completed, then
tests each requested mode through its `DrmOutput`. If any CRTC rejects the
candidate, already changed outputs are restored in reverse order and no
Wayland output or configuration state is published. Once all KMS mode tests
succeed, mode, transform, fractional scale, position, primary selection, and
client reflow become visible together. Connector enable/disable reload reuses
the provisional-addition hotplug transaction, so failed additions retain the
old connector set. Each `wl_output` also advertises the connector's complete
mode list and its actual DRM-preferred mode rather than incorrectly marking a
configured current mode preferred.

The friendly Settings editor now owns the complete dynamic output-rule
workflow instead of treating `[outputs]` as Advanced-TOML-only state. It can
add and remove exact connector rules and edit enabled state, preferred or exact
mode, automatic or signed logical position, every transform, exact fractional
scale, and one atomic primary selection. Each edit passes through a typed,
format-preserving `nobox-config` transaction; invalid or duplicate rules leave
the prior document intact. Save-and-apply discovers a Wayland runtime control
endpoint when launched inside a Wayland session, while retaining the existing
X11 path and refusing ambiguous instances.

The direct renderer now publishes `zwp_linux_dmabuf_v1` v5 default feedback
from the actual render node and its supported formats. DMA-BUF creation imports
are bounded and completed by the backend renderer; a rejected import fails only
that client buffer. Committed surface trees are also early-imported through the
multi-renderer path, while a failed surface import is omitted until a later
valid commit instead of terminating the compositor. When the DRM device
supports syncobj eventfd waits, Nobox additionally publishes
`wp_linux_drm_syncobj_manager_v1` v1: acquire points block the Smithay surface
transaction through bounded calloop sources and release points follow renderer
buffer lifetime. Unsupported devices simply omit the global. The compositor
loads a bounded system XCursor theme with a small server-owned solid fallback,
while each `DrmOutput` retains its own damage history and vblank-driven frame
state. Direct Ctrl-Alt-F1 through F12 requests are forwarded through libseat
rather than relying on an X server or desktop environment.

Preliminary physical evidence (2026-08-18): LightDM started the direct session
on an NVIDIA GeForce GTX 1660 SUPER with `DVI-D-1` and `HDMI-A-1`, and the
compositor remained alive until an intentional remote LightDM restart. That
run exposed incorrect menu output placement, an empty menu, menu/cursor layer
ordering, cursor-theme, VT-switch, and XWayland-readiness defects. The fixes
are covered by the automated suite and installed in `nobox-wayland` 0.2.64 and
`nobox` 0.2.37, but require the reduced follow-up LightDM run. This preliminary
dogfood evidence is not the guarded W4 hardware record.

This is not the W4 exit: initial multi-connector KMS creation has compile-time
and emulated topology coverage but no disposable-VT hardware record yet, udev
application still needs the disposable-VT unplug/replug record, output
configuration reload rollback still needs its forced real-hardware failure
record, and a udev event that changes an existing connector's CRTC assignment
is refused rather than guessed. `direct_session` therefore remains false. The
remaining W4 work is the disposable-VT hardware acceptance record.

The guarded recorder and exact evidence contract are documented in
[`wayland-hardware-acceptance.md`](wayland-hardware-acceptance.md). The in-tree
client now inventories output publication and deliberately exercises renderer
import rejection before checking that a subsequent SHM surface still renders.
The recorder also retains the exact DRM GPU and initially connected connector
identities in both its machine inventory and human-readable acceptance record;
a synthetic sysfs fixture keeps that evidence path deterministic before the
physical run.
The recorder refuses graphical or non-logind TTY sessions and cannot turn
human hardware actions into automatic claims.

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

Status: complete in `nobox-wayland` 0.2.40, `nobox-config` 0.2.4, and
`nobox` 0.2.23. Direct physical delivery remains part of the separate W4
hardware gate.

The first W5 protocol tranche publishes `wp_viewporter` v1 and
`wp_fractional_scale_manager_v1` v1 in both nested and direct sessions. The
renderer consumes viewport source/destination state through Smithay's committed
surface state. Preferred fractional scale follows the output selected by the
existing protocol-neutral client geometry, layer-output association, or cursor
location, and is refreshed with output enter/leave processing after topology,
scale, or window-placement changes. Unassociated surfaces receive the primary
output scale until they enter the managed scene.

Every client now has an atomic 256-`wl_surface` limit covering toplevels,
popups, subsurfaces, layer surfaces, cursors, and the one-per-surface viewport
and fractional-scale objects. Reservations are released on surface destruction;
the 257th surface disconnects only the offender. The nested protocol fixture
maps and renders a viewport surface, requires the exact preferred scale event,
checks both duplicate-object protocol errors, floods the surface limit, and
then maps a healthy client. Doctor output reports both exact global versions.
This is a completed W5 slice, not the W5 exit: the remaining data-transfer
edge cases and bounds, advanced input, text input, inhibition, presentation
feedback, other resource classes, and representative
toolkit acceptance below remain.

The second W5 tranche publishes `wl_data_device_manager` v3 and
`zwp_primary_selection_device_manager_v1` v1. Clipboard and primary-selection
focus follow the keyboard-focused Wayland client; typed selection offers pass
their file descriptors directly between source and recipient, replacement cancels the old
owner, and client DND uses Smithay's serial-validated pointer/touch grabs,
negotiated actions, cancellation, and drop lifecycle. DND icons are rendered at
the pointer in nested GLES/Pixman and direct multi-output paths, receive frame
callbacks, and disappear when the grab ends. The nested fixture round-trips
exact clipboard and primary bytes and requires both replacement cancellations;
an installed GTK4 demo also remains alive as a native Wayland client. Exact
protocol versions are reported by both doctors.

The bounded-selection follow-up records the exact client that owns clipboard
and primary selection, clears both selections when that connection dies, and
proves that a separately connected observer receives both withdrawals.
Per-connection creation budgets jointly cover both protocols: 64 sources and
16 devices. Each source may advertise at most 32 MIME types of at most 256
bytes. These cumulative budgets also bound the offers Smithay can derive from
one client even if it repeatedly destroys and recreates resources. Four
hostile fixtures exceed each independent boundary across both protocols;
every offender is disconnected and a fresh client then completes both byte
transfers and replacement cancellation. Both doctors publish the exact
limits.

The interactive follow-up uses XTest only to create real pointer input; the
client must start each drag from the actual implicit-grab serial delivered by
Nobox. The successful path negotiates `Copy`, requires a rendered DND-icon
frame callback, transfers exact bytes through the target offer, and observes
target drop plus source `dnd_drop_performed` and `dnd_finished`. The
cancellation path first enters a valid target, then leaves every client
surface and releases; it requires source cancellation and forbids a target
drop. Together with owner death and resource exhaustion, this completes the
clipboard/DND data-transfer slice. W5 remains in progress for the advanced
input, text, inhibition, presentation, resource-bound, and
toolkit exits below.

The first advanced-pointer tranche publishes
`zwp_relative_pointer_manager_v1` v1 and `zwp_pointer_constraints_v1` v1 in
both nested and direct sessions. Native libinput preserves separate accelerated
and unaccelerated deltas; nested X11 derives raw deltas from the host pointer
while retaining absolute resynchronization between clients. A focused lock
holds `wl_pointer` coordinates while relative motion continues, confinement
honors the committed client region or surface boundary, persistent constraints
reactivate on focus return, and a committed cursor-position hint is applied on
unlock. Smithay protocol state and resources remain wholly inside
`nobox-wayland`; `nobox-core` receives only ordinary pointer locations and
display-neutral policy events.

The nested fixture requires real relative events under both lock and
confinement, stable `wl_pointer` coordinates while constrained, cursor-hint
restoration, release from confinement, and fatal rejection of two constraints
for one surface/seat pair. A cumulative per-connection ceiling of 64 created
relative-pointer or pointer-constraint objects disconnects only the offender;
a fresh constrained-pointer client then proves compositor health. Both doctors
publish the two exact protocol versions and the shared limit. W5 remains in
progress for gestures, cursor shape, touch/tablet, text input, inhibition,
presentation, remaining resource classes, and the toolkit
exit matrix.

The timing tranche publishes `wp_presentation` v2 with `CLOCK_MONOTONIC`.
Feedback is double-buffered with the surface commit and completed only after
the matching nested renderer submission or direct KMS vblank. It reports the
selected output, fixed mode refresh, a monotonic timestamp, and a nonzero
compositor sequence; direct completions additionally carry the vblank flag.
The complete committed surface tree is drained alongside frame callbacks, so
superseded or destroyed feedback retains Smithay's discard lifecycle. A
cumulative 256-feedback connection budget prevents callback floods. The nested
fixture requires a presented (not discarded) result with exact clock, refresh,
and sequence fields, then exceeds the budget and proves a fresh client still
receives valid feedback. Both doctors report the global version and limit.

The first inhibition tranche publishes
`zwp_keyboard_shortcuts_inhibit_manager_v1` v1. An inhibitor becomes active
only while its surface has keyboard focus; focus changes atomically inactivate
the old inhibitor and activate the new focused surface's request. While active,
ordinary Nobox bindings are forwarded through the seat instead of entering the
policy action dispatcher. Release bookkeeping still clears any chord that was
intercepted before inhibition, and destruction immediately restores normal
bindings. The nested fixture proves an active inhibitor receives all of
Super-q without triggering close, then destroys it and requires the same chord
to trigger `xdg_toplevel.close`. A cumulative 64-inhibitor connection budget
disconnects only the offender and the functional probe runs afterward. Both
doctors report the global version and limit. Idle inhibition remains separate
because it must be coupled to the compositor idle lifecycle rather than merely
advertised.

The gesture tranche publishes `zwp_pointer_gestures_v1` v3. The direct
libinput path forwards swipe, pinch, and hold begin/update/end events through
the focused Smithay pointer, preserving finger counts, deltas, pinch scale and
rotation, cancellation, input timestamps, and fresh compositor serials.
Smithay cancels an in-progress stream if pointer focus leaves its surface.
Nested X11 publishes and validates the protocol objects but does not invent
touchpad gestures from ordinary X pointer motion. Its fixture creates swipe,
pinch, and hold objects, then exceeds a cumulative connection-lifetime limit
of 64 gesture objects and proves a fresh client can create all three. Both
doctors publish version 3 and the exact bound. Direct gesture delivery remains
part of the guarded input-device hardware record because XTest has no genuine
libinput gesture source.

The cursor tranche publishes `wp_cursor_shape_manager_v1` v2. Pointer clients
may select the complete standard shape set only with their current focus-enter
serial; Smithay rejects stale or cross-client cursor authority. Nobox maps the
named shapes into a small, consistent compositor-rendered glyph theme shared by
nested GLES, nested Pixman, and direct multi-output rendering, while retaining
client-provided `wl_surface` cursors and hotspots. Text, vertical text,
crosshair, busy, hand, forbidden, directional resize, move, and zoom families
are visually distinct; all protocol shapes have bounded nonempty fallback
geometry. A cumulative connection-lifetime ceiling of 64 cursor-shape devices
disconnects only the offender. The nested fixture creates cursor-shape objects
with a real `wl_pointer`, uses an actual enter serial to select text and
horizontal-resize shapes, exhausts the limit first, and then proves a healthy
focused client remains usable. Unit coverage proves every standardized shape
has bounded geometry and the representative families do not collapse to the
old single-arrow fallback. Both doctors report version 2 and the exact limit.

The touch tranche adds the `wl_touch` capability to the version-9 `wl_seat`.
Nested winit and direct libinput input paths forward down, motion, up, frame,
and cancellation events through Smithay's per-slot touch grab, using compositor
space coordinates and the existing surface hit test. The Pixman X11 host still
advertises the protocol but cannot synthesize touch from ordinary X pointer
events. A cumulative connection-lifetime ceiling of 16 `wl_touch` objects
disconnects only the offender. The nested fixture requires the advertised seat
capability, creates a healthy device, exhausts the limit first, and then proves
a fresh client can create one. Both doctors report the seat version and bound.
Actual nested-winit or direct-libinput event delivery remains in the guarded
input-device hardware record; the fixture does not mislabel XTest pointer
events as touch.

The tablet implementation publishes `zwp_tablet_manager_v2` v1. Direct libinput
hotplug registers at most 16 tablet-tool devices, 64 distinct tools, and 16
tablet pads with the native Nobox adapter. Proximity, absolute motion, pressure,
distance, tilt, rotation, slider, wheel, tip, and tool-button state are forwarded
with compositor serials, timestamps, surface-local focus, and client-selected
tool cursor images. Pads are paired to their tablet through libinput device
groups and publish bounded mode groups, buttons, rings, and strips; pad focus
follows the paired tool's focused surface. A
cumulative connection-lifetime ceiling of 16 tablet-seat objects disconnects
only the offender. The nested fixture requires the exact manager version,
exhausts that limit, and then proves a fresh client can create a tablet seat;
both doctors report the complete object bounds. Device removal sends
client-visible `removed` events for tablets, tools, and pads after cancelling
active tip/button/focus state; a tablet is removed with every known tool that
last belonged to it, including tools currently outside proximity. Pad removal
also terminates its focus before the object is removed.

The software tablet-v2 deliverable is complete. Nested X11 has no tablet event
source, so real tool and pad event delivery remains an explicit guarded
input-device hardware record and is not inferred from the nested object fixture.

The secure text-input tranche conditionally publishes
`zwp_text_input_manager_v3` v1 when `[wayland].input_method` contains a
validated absolute argv. Nobox creates a private socket pair, inserts one end
as an explicitly authorized Wayland client, and launches the configured
process with the other end as `WAYLAND_SOCKET`; no public socket race, PID,
executable-name assertion, or client metadata grants authority. The filtered
`zwp_input_method_manager_v2` v1 global is visible only on that inherited
connection. Ordinary clients receive focus-scoped text-input enter/leave and
the full enable, surrounding-text, change-cause, content-type, cursor-rectangle,
preedit/delete/commit, and serial lifecycle through Smithay.

Connection-lifetime budgets allow 32 text-input objects per ordinary client,
one input-method object on the authorized connection, and eight popup and
eight keyboard-grab objects. Input-method popups use the existing popup scene
and output geometry. A configured command change is restart-only so reload
cannot silently diverge from the live privileged process. The nested fixture
proves the privileged global is absent from an ordinary registry, exhausts the
text-input budget without harming the compositor, round-trips focused text
state and the exact `nobox-ime` commit, then requires text-input leave, child
reaping, and a healthy shell after deliberate IME death. Both doctors publish
the conditional versions, authorization boundary, and bounds.

The idle-lifecycle tranche publishes `zwp_idle_inhibit_manager_v1` v1 and
`ext_idle_notifier_v1` v2. Ordinary idle notifications stop only while an
inhibitor belongs to a currently buffered, visible native toplevel or mapped
layer surface; hidden, iconic, unmapped, destroyed, and disconnected surfaces
cannot keep the session awake. Input-idle notifications deliberately ignore
those inhibitors. Nested and direct pointer, keyboard, gesture, touch, and
tablet-tool activity resume idle clients and restart their deadlines, while the
compositor-owned clock delivers deterministic idle transitions without an
external daemon. Stored protocol and surface references are weak so hostile
disconnects cannot retain a client or poison later input dispatch.

Both object classes have cumulative 64-object connection-lifetime budgets.
The nested fixture exhausts each budget, proves inhibitor suppression and
input-only bypass, destroys the inhibitor to restart the ordinary deadline,
injects real nested input to resume both notification classes, and then maps a
fresh healthy shell. Doctors report the exact versions and bounds.

The secure-lock tranche publishes `ext_session_lock_manager_v1` v1. Accepting
a lock request immediately removes ordinary focus, pointer constraints, cursor,
drag icons, compositor menus, move/resize operations, and binding dispatch from
the visible/input scene. Nested GLES, nested Pixman, and direct DRM render only
the matching lock surface over pure black; ordinary toplevels, layers, popups,
decorations, overlays, and their frame/presentation callbacks remain suppressed.
The compositor sends `locked` only after every current output has submitted a
secure frame. Output additions join that barrier until confirmation, and lock
surfaces receive exact output-sized configures after topology changes.

Only a confirmed lock owned by the requesting connection may unlock. A
pre-confirmation unlock is a fatal client error that retains the secure state.
Locker death drops every retained lock-surface reference and presents black,
but deliberately keeps the session locked; competing lockers receive
`finished`. Connection-lifetime budgets allow eight lock objects and sixteen
lock surfaces per client. The nested fixture proves keyboard delivery to the
lock surface, clean unlock and fresh-shell recovery, ordinary callback
suppression, a real black pixel after locker death, competing-lock refusal,
invalid-unlock secure retention, and hostile lock-object exhaustion. Both
doctors report the exact version and bounds. Smithay types and lock resources
remain confined to `nobox-wayland`; no lock protocol object enters core policy.

The core-resource hardening tranche adds concurrent per-client ceilings for 64
SHM pools, 4096 SHM buffers, 1024 frame callbacks, 256 XDG positioners, and 128
XDG popups. One SHM pool is limited to 64 MiB and one SHM buffer axis to 16384
pixels. Destruction releases each concurrent reservation, so long-running
healthy clients do not consume a connection-lifetime budget for ordinary frame
or buffer reuse. XDG toplevel and popup state retains at most 64 unacknowledged
configures per surface; a client that keeps provoking configures without
acknowledging them is disconnected before the Smithay queue can grow without
bound. The nested hostile fixture independently exceeds every count and size
boundary, then continues through the healthy-client suite.

The 2026-08-15 developer acceptance run exercised GTK 4.20, Qt 6.10, SDL 2.32,
and Chromium/Ozone as native clients with `DISPLAY` removed, plus the dedicated
text-input/input-method round trip. Each remained live long enough to map and
render under the managed shell, and the complete hostile-client suite remained
responsive afterward. The portable test discovers these external toolkits when
installed (including an existing Puppeteer Chromium cache) without making them
runtime or build dependencies. This satisfies the W5 toolkit and resilience
exit; W5 is complete.

Deliverables:

- Implement clipboard, primary selection, DND, data-offer cancellation,
  relative/constrained pointers, gestures, cursor shape, touch, tablet,
  text-input/input-method, shortcut inhibition, idle inhibit, presentation
  feedback, viewporter, and fractional scale completely.
- Maintain `ext-session-lock` as a privileged compositor state: all outputs
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

Status: complete (2026-08-15) in `nobox-panel` 0.2.1, `nobox-wayland`
0.2.41, `nobox-runtime` 0.2.6, and `nobox` 0.2.24.

`nobox-panel` now has peer X11 and Wayland frontend modules behind a thin
backend-selecting CLI. The native frontend is an independent SHM layer-shell
client: it reads the same strict `[panel]` model and bounded desktop catalog,
reserves the configured edge, draws the ordered component model with a bounded
system-font renderer, and never links `nobox-core` or `nobox-wayland`.

The panel observes the standard `ext-foreign-toplevel-list` and
`ext-workspace-v1` globals and uses wlr foreign-toplevel v3 for the actionable
task handles that the standard list deliberately does not provide. Wlr
`output_enter`/`output_leave` events carry current-workspace visibility, so
current/all task scope is exact without a private protocol or socket. Activate,
minimize, maximize, fullscreen, close, and workspace requests all re-enter the
ordinary core capability and action paths. The wlr protocol has no
per-operation capability event, so unsupported requests are ignored by those
checks rather than falsely advertised as supported. Manager bindings are
limited to 16 per client and protocol stop/destruction ends future publication.

The session supervisor starts either explicit panel backend only after its
display endpoint exists. Replacement is asynchronous: the working panel stays
alive until its candidate has committed a drawable buffer and written the
readiness token. Candidate failure, panel death, and compositor restart remain
separate failure domains. Nested acceptance injects real pointer input through
the parent X server to prove workspace selection, exact current/all task scope,
activate/minimize/close, launcher execution, reconfigure replacement, failed
replacement retention, crash isolation, and recovery. Doctors list the four
panel-facing protocol versions and manager bound.

There is one explicit protocol-level non-goal. Neither
`ext-foreign-toplevel-list` v1 nor wlr foreign-toplevel v3 publishes a
toplevel urgency/attention state. `urgent_background` remains in the canonical
model and works in the X11 frontend, but native Wayland cannot apply it without
inventing a Nobox-private extension. Nobox does not invent that extension; the
setting becomes effective when a standard actionable toplevel protocol exposes
attention state.

Deliverables:

- Add a layer-shell Wayland frontend to `nobox-panel`, sharing its canonical
  `[panel]` model, XDG launcher parsing, component order, task filtering,
  interaction settings, and clock formatting with the X11 frontend.
- Consume `ext-foreign-toplevel-list` and `ext-workspace-v1`; implement the
  established wlr foreign-toplevel management requests required by the current
  task buttons. Honor only operations core says a client supports.
- Preserve readiness replacement across backend modes. Panel loss removes its
  layer surface/reservation and never affects compositor input or rendering.

Exit:

- All panel options representable by the advertised protocols have matching
  Settings controls, docs, unit tests, and nested-Wayland tests; the missing
  urgency signal is documented rather than privately extended.
- Workspace buttons, task scope, activate/minimize/close, launchers, clock, live
  reconfigure, crash, and failed replacement behave like the X11 panel's
  documented contract.
- The panel has no dependency on `nobox-wayland`, `nobox-core`, or compositor
  private sockets.

### W7: XWayland compatibility

Status: complete (2026-08-16) in `nobox-wayland` 0.2.62, `nobox-core`
0.2.3, and `nobox` 0.2.35.

Lifecycle and managed-scene foundation evidence (2026-08-15): XWayland is an independent Cargo
feature (`nobox/xwayland` -> `nobox-wayland/xwayland`) and CMake option
(`NOBOX_BUILD_XWAYLAND`). Runtime enablement is a strict
`[wayland].xwayland` boolean that remains opt-in after W7 completion. When
enabled, both nested and direct loops spawn XWayland
through Smithay, wait for its readiness event before retaining a `DISPLAY`,
own the Smithay XWM, and remove the process/XWM without stopping native
clients. Startup failure and XWM disconnect schedule a bounded one-second
retry. Runtime disable/re-enable, forced XWayland death, replacement readiness,
and a native client before/during/after that cycle are covered by
`wayland-xwayland-lifecycle`. Managed windows now enter Smithay's shared
`Window`/`Space` scene only after X11 metadata has been translated to a
protocol-neutral core client. The boundary maps supported X11 roles,
class/instance/title identity, transient/modal relationships, min/max/base size
hints, application workspace/layer/decoration/focus settings, placement,
minimize/maximize/fullscreen requests, stacking, and keyboard focus. A bounded
set of 128 override-redirect surfaces stays outside core policy and above the
managed stack. The lifecycle regression launches real X11 surfaces and proves
managed rendering through the nested host pixel, override-redirect separation,
X input focus, cleanup, restart, and native-client survival. The ordinary build
without the feature remains checked.

The selection-bridge follow-up keeps the Wayland seat as the single clipboard
and primary-selection authority. Native client sources are advertised through
the current Smithay XWM and republished after runtime XWayland replacement;
X-owned sources become generation-tagged compositor offers on that same seat,
with transfer file descriptors routed back through the owning XWM. MIME lists
are deduplicated and retain the existing 32-entry/256-byte bounds. Native owner
death makes its data unreadable from X immediately, and XWayland death revokes
both X-owned offers without affecting native clients. The lifecycle fixture
proves exact bytes in both directions for clipboard and primary selection,
republish across disable/re-enable, and withdrawal on a forced XWayland crash.
Stale callbacks are rejected by XWM generation. The normal-hint follow-up now
translates the complete core size-constraint model: minimum, maximum, base,
resize increments, and ordered aspect ranges. A real X client requests an
off-lattice size and the lifecycle fixture proves that it is configured to the
core-selected increment- and aspect-constrained geometry. Client-initiated
pointer move and resize requests now re-enter the same capability, snapping,
work-area, and size-constraint path as native windows, with the resulting core
geometry configured back to X. The XWM boundary accepts a request only while a
real Smithay implicit grab has the requested physical button and belongs to the
target surface; the fixture proves an ungrabbed spoof is inert before proving
real move and resize gestures. The relationship follow-up assigns bounded,
backend-local X group windows to neutral core group identities, resolves
specific and group transients again when late parents appear or disappear, and
uses core policy stacking for both scene hit-testing and the XWM stack. A real
X client maps a group transient before its main peer and the fixture proves the
core-selected parent-before-transient order is reflected in
`_NET_CLIENT_LIST_STACKING`. Representative toolkit coverage now builds real
GTK 3/X11 and Qt 6/xcb clients, proves that both enter the XWM client list, and
switches focus from one toolkit to the other through ordinary core pointer
policy. The fixtures remain conditional on their development packages so a
minimal build host retains the rest of the XWayland lifecycle coverage.
Compositor-issued launch tokens now cross the boundary through the standard
`DESKTOP_STARTUP_ID`/`_NET_STARTUP_ID` path, are bounded and consumed exactly
once, and activate the resulting X client through the same workspace, focus,
raise, and unminimize policy as native activation. A real XDG desktop launch
proves the token reaches and focuses an application whose ordinary application
rule disables focus-on-map; unit coverage rejects forged, empty, oversized,
and replayed values. The XWayland generation now receives the primary output's
integral ceiling scale before its XWM starts, preserving logical core geometry
while Smithay converts X coordinates at the boundary. Minimum, maximum, base,
and resize-increment hints use the same conversion, GTK/Qt receive matching
`Gdk/WindowScalingFactor`, `Gdk/UnscaledDPI`, and `Xft/DPI` XSETTINGS, and an
output-scale change reconstrains and reconfigures managed X windows. This is
necessarily one process-wide scale: mixed-scale layouts follow the primary
output because XWayland is one Wayland client and cannot assign an independent
client coordinate space per X window.

The interoperability follow-up pins Smithay to revision
`ba0063fbebb6f8c2905c61d74292f213973580e0`, the first reviewed upstream
revision carrying the required bidirectional XDND, X activation, and live
`_NET_WM_STATE_MODAL` request APIs. Nobox crossed Smithay's Dispatch2
transition without weakening its hostile-client bounds: exact protocol
resource pairs still pass through Nobox validation wrappers before delegating
to Smithay's user-data dispatch, and the isolated XWM event queue remains in
place. The XWM generation and its
selection/DND transfer sources are retired as one unit on runtime disable,
startup failure, or crash, preventing stale callbacks from addressing a
replacement XWM. Client-ID keyed resource accounting preserves SHM-pool,
SHM-buffer, and xdg-positioner limits across the Smithay API change and removes
registrations synchronously on disconnect so reused IDs cannot inherit stale
counters.

Pointer and touch focus now retain whether a target is native Wayland or an
X11 surface, while the shared seat remains the only input and DND authority.
Every pointer event closes its protocol frame, including motion, buttons,
axes, and gestures; this is required for ordinary GTK threshold drags and also
fixes native toolkit input batching. Smithay's serial-validated DND grab then
bridges offers in both directions, retains action/cancellation/drop semantics,
and uses the compositor's last policy-configured X geometry for hit testing at
the XWayland scale boundary. The lifecycle regression drives real GTK 3
X11 and Wayland clients from an implicit pointer grab and verifies the exact
`nobox-cross-dnd` payload Wayland-to-XWayland and XWayland-to-Wayland.

Standard X client activation requests now pass through the same recent-user-
time, related-handoff, workspace, focus, raise, unminimize, and attention
policy as native activation. `xdg-dialog-v1` updates native modal relationships
live, initial X transient/dialog state enters the same neutral core group and
modal model, and application position/size rules are applied to X clients with
the same bounded geometry policy. Live X `_NET_WM_STATE_MODAL` add/remove
requests now update both the X property and neutral core relationship state;
the nested lifecycle fixture proves parent focus redirects to the modal group
transient and returns to the parent after removal. `ClientSet::focus` enforces
that neutral redirect for every backend focus path. XWayland `wl_surface`
commits are explicitly kept out of the xdg-toplevel metadata path, so XWM-owned
size hints, groups, transients, and modal state cannot be overwritten by
Wayland-side defaults. The same regression retains X normal hints through
commits and applies core-selected stacking to the XWM in top-to-bottom walker
order.

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

Status: complete in `nobox-wayland` 0.2.60, `nobox-agent-seat` 0.1.1, and
`nobox-agent-semantic` 0.1.0. The first boundary milestone extracted the
existing, fully bounded UNIX-socket
listener, peer-credential collection, frame queues, and teardown into the
display-neutral `nobox-agent-seat` crate. Its wakeup is now supplied by the
owning backend, so X11 retains selection-based discovery
and its native control event while Wayland can add environment discovery and a
calloop wake source without depending on `nobox-x11`. The unchanged
`x11-agent-seat` integration regression proves the extraction preserved the
existing handshake, grants, traffic bounds, and cleanup. Wayland kept Agent
Seat unavailable throughout those internal milestones until accessibility and
public discovery completed the gate.

The client-projection milestone now gives the Wayland compositor its own
display-neutral `AgentState` and `ClientDetails` adapter. Native and XWayland
clients enter the same application-rule visibility and scoped-membership path
when they are managed or their identity changes, and are forgotten on unmap or
destruction. Snapshots obtain protocol-neutral application type, decorated
frame, workspace name, output name, and layer-adjusted work area without
placing Smithay objects or Wayland IDs in core or on the wire. Focused unit
coverage proves the privacy/type translations and a core-built snapshot on a
non-zero-origin output; that test also fixed the Wayland boundary's conversion
of Smithay output-local layer zones into Nobox global policy coordinates. This
is internal foundation only: no listener or capability is advertised yet. The
milestone passes the developer build, workspace check, and all 59 CTest entries
(with the four environment-dependent X11 cases reported as skips).

The explicit transport milestone attaches that reusable listener to both the
nested and direct calloop loops. Verified peer executable and user identity
select the stored grant, which is narrowed to the two realized observation
atoms; `desktop.snapshot` and `client.get` use core policy, while every other
otherwise-authorized call receives a structured `unsupported` result. The
welcome advertises no optional features, the socket is not exported into the
environment, and `BackendCapabilities::WAYLAND_NESTED.agent_seat` remains
false. Configuration reload starts, stops, or replaces the listener without
making the compositor depend on it. A dedicated nested-X regression maps a
real native client, proves the executable-bound narrowed grant and snapshot,
checks private listener directory/socket modes, exits through runtime control,
and proves socket cleanup. Unit coverage drives the same framed greeting,
snapshot, and refusal directly. Listener hardening also stopped configured
socket paths from changing permissions on an existing caller-owned parent;
only Nobox's derived parent or a newly created leaf is tightened to `0700`.
The developer build, workspace check, and all 60 CTest entries pass, with the
four environment-dependent X11 cases reported as skips.

The subscription milestone adds atomic `subscribe_and_snapshot` and the
bounded event stream without advertising a new backend feature. A
display-neutral shadow is reconciled only at coherent nested/direct loop
boundaries; it coalesces interactive geometry and derives native and XWayland
map, close, title, state, geometry, focus, and workspace changes through the
same core visibility, scope, generation, sequence, and backlog policy used by
X11. Closing a client publishes its final identity before forgetting its
privacy/scope projection. The nested regression now subscribes before mapping
a second real native client and proves strictly advancing mapped/focus/closed
events through that client's complete lifetime. Focused unit coverage also
proves ordered map/geometry/close projection and absence after retirement.
The developer build, workspace check, and all 60 CTest entries pass, with the
four environment-dependent X11 cases reported as skips.

The management milestone grants only the realized `manage.activate`,
`manage.geometry`, `manage.close`, and `manage.workspace` atoms. Client calls
first apply core perception and freshness checks, then route through the
existing Wayland activation, constrained configure, workspace, and negotiated
close paths; unsupported client operations return a structured refusal and no
kill path is exposed. The existing backend-neutral management probe now drives
a real native client across a workspace boundary, proves focus and activation,
forces and recovers from a stale generation, verifies the committed geometry,
and observes the client exit in response to `xdg_toplevel.close`. Unit coverage
also proves stale geometry refusal; configured but unrealized capabilities
remain absent from the grant.
The developer build, workspace check, and all 60 CTest entries pass, with the
four environment-dependent X11 cases reported as skips.

The state-management milestone adds the realized `manage.state` atom without
making Agent Seat public under Wayland. A client state request first applies
the same perception and freshness checks as every other client-addressed call,
then validates the complete multi-field change before mutating anything. Valid
requests reuse core's minimize/restore, per-axis maximize, fullscreen restore,
shade, sticky-workspace, and above/below layer policy plus the existing native
and XWayland configure paths. Fullscreen exit is applied before dependent
states and fullscreen entry last, so combined requests retain a coherent
restore geometry. Focused unit coverage proves an unsupported later field
cannot leave an earlier minimize partially committed. The nested regression
now minimizes and restores a real native client through the backend-neutral
probe before exercising the existing workspace/activation/geometry/close
sequence. Transport coverage proves `manage.state` is granted while the next
configured but unrealized atom remains masked and denied.
The developer build, workspace check, and all 60 CTest entries pass, with the
four environment-dependent X11 cases reported as skips.

The launch milestone realizes `launch.desktop` through the existing bounded
`nobox-desktop` catalog and the independent `[agent.launch]` policy. Requests
can name only a catalog desktop ID; an installed entry outside the allow policy
returns `launch_denied`, URI arguments remain unsupported rather than being
misrouted, and spawn failures consume their reserved token. Every authorized
launch allocates a bounded one-shot compositor token and passes the same opaque
value through native `XDG_ACTIVATION_TOKEN` and XWayland
`DESKTOP_STARTUP_ID`. Only tokens reserved for Agent Seat launches can be
attached to an event; ordinary menu and binding activation tokens never become
correlation claims. Native XDG activation or XWayland startup handling consumes
the token and records it on exactly one protocol-neutral client, whose first
`client_mapped` event carries the value returned by `launch` without exposing
PIDs or Wayland/X11 object IDs. The nested regression builds a disposable XDG
catalog, refuses a present but non-allowlisted entry, launches an approved real
native client, and correlates its mapped event without titles, pixels, or timing
heuristics. Unit coverage proves one-shot event attachment, while transport
coverage grants launch and keeps the configured but unrealized
`capture.output` atom masked and denied. The developer build, workspace check,
and all 60 CTest entries pass, with the four environment-dependent X11 cases
reported as skips.

The scene-capture milestone realizes `capture.client_visible`,
`capture.client_obscured`, and `capture.output` on the still-explicit Wayland
Agent Seat socket. Requests are validated and authorized before entering a
bounded eight-item queue, then revalidated by the renderer-owning loop so a
grant, client, generation, or lock-state change cannot race pixel access.
Client content/frame capture renders only the selected committed surface tree,
its subsurfaces and popups, plus that client's server decoration when requested;
it never samples whatever happens to cover the window. Output capture builds a
logical-coordinate offscreen scene including layer-shell surfaces, then places
opaque masks over every hidden/redacted client frame and popup bound before
readback. Session-lock and compositor security UI are never part of that scene,
and capture is refused while locked or while a direct seat is inactive. GLES,
Pixman, and direct multi-GPU renderers share the same deferred service, bounded
pixel limit, RGB PNG encoder, crop stamps, and signed content-coordinate grid.
The nested regression exercises real client, output, and output-crop pixels;
focused tests prove obscured-capability separation, crop/grid coordinates, PNG
encoding, and pre-encoding privacy masks. At that milestone the next configured
but unrealized `input.pointer` atom remained masked and denied, and input,
consent, indicators, and human-preemption still awaited their own gates.

The input-and-consent milestone realizes `input.pointer` and `input.keyboard`
without routing Agent Seat events through compositor shortcuts, mouse bindings,
or pointer constraints. Pointer calls resolve a live content-relative target
and refuse covered or out-of-bounds destinations; key calls compile named keys
and neutral modifiers through XKB; text is completely validated before paced
injection, with a request-local two-second selection fallback for exact UTF-8
and long text. Human pointer, keyboard, touch, gesture, and tablet activity wins
before and during injection, is coalesced into privacy-preserving activity
events, and clears temporary agent clipboard ownership. Bounded post-action
observation settles on quiet/minimum/maximum deadlines and can enter the same
deferred capture queue. The configured kill chord is intercepted before lock,
inhibition, menu, and ordinary binding handling; it freezes or resumes every
session and drives an always-compositor-owned visible/frozen indicator that is
excluded from capture. An unconfigured peer under `policy = "ask"` receives a
compositor-owned consent menu showing its verified executable, uid, pid,
purpose, and requested bundles, with deny, allow-once, and persisted choices.
Grant-list reloads now re-evaluate live non-consented sessions in place and
deliver `session_control: revoked` before refusing further calls. The nested
regression proves real pointer/key/text delivery, exact Unicode, invalid and
interrupted requests, paced-prefix interruption, observation settlement,
freeze/resume, consent allow/deny, capture and management continuity, and live
revocation in one session.

The accessibility-and-discovery milestone completes W8. The disposable AT-SPI
helper runner moved out of `nobox-x11` into the display-neutral
`nobox-agent-semantic` crate with a backend-provided wakeup; X11 retains its
existing correlation and failure behavior while Wayland correlates native
clients from the owning `wl_client` credentials, a complete bounded process
scan, the exact authorized content/frame rectangles, and a fixed 1.2-second
reply boundary. Backend object IDs and PIDs remain private; the neutral helper
state remaps nodes and continuations into session-local handles and rechecks
grant, redaction, generation, and client PID before releasing a result. A
private-bus GTK 4 regression exercises root, paged tree, refreshed generation,
search, and matching capture against a real native Wayland client. Successful
final acceptance also makes the seat public: compositor-launched commands and
desktop applications receive the exact live `AGENT_SEAT_SOCKET`, the stock MCP
companion completes discovery and a real call through that environment path,
and `BackendCapabilities::WAYLAND_NESTED.agent_seat` now reports true. No X11
property or synthesized filesystem fallback is introduced. The foundation
regression covers the complete snapshot/subscribe, launch correlation,
management, stale-state, input, capture, human interruption, freeze, consent,
environment discovery, and live-revoke flow; helper parsing/failure remains in
the shared process-boundary tests, and renderer/lock/privacy failures retain
their focused fail-closed coverage.

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

Status: automated acceptance complete; the 2026-08-18 LightDM dogfood fixes are
in `nobox-wayland` 0.2.64 and `nobox` 0.2.37. The guarded direct real-hardware
record remains the sole open release gate. See
[`wayland-release-acceptance.md`](wayland-release-acceptance.md).

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
