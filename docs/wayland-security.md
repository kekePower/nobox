# Wayland security boundary

Nobox's Wayland compositor is the display authority, not a security sandbox for
arbitrary same-user processes. It nevertheless treats every Wayland client,
XWayland client, helper process, and Agent Seat peer as untrusted input and
keeps failure local to the narrowest owning boundary.

## Ownership and isolation

- Smithay objects, DRM handles, Wayland serials, X11 resources, and renderer
  buffers stay in `nobox-wayland`. `nobox-core` receives only typed policy
  identities, geometry, state, and outcomes.
- Direct device ownership comes only from libseat. Pause drops input/DRM use;
  resume reconstructs device mechanics around retained policy state. A direct
  run requires explicit `--backend wayland run --tty`.
- XWayland is optional at build and runtime. Smithay's XWM is its sole window
  manager; a crash removes XWayland clients and schedules an isolated restart
  without replacing native policy authority.
- The panel, settings application, MCP companion, input method, and
  accessibility helper are separate processes. Their exit cannot unwind the
  compositor.

## Bounded protocol surface

Advertised globals have explicit object, queue, byte, geometry, and configure
limits exercised by hostile-client fixtures. Invalid roles, serials, resource
exhaustion, imports, lock transitions, and XWayland messages disconnect or
refuse the offending client. They do not become best-effort state mutations.

Clipboard and primary-selection bytes travel directly between endpoints under
bounded MIME metadata; they are neither logged nor retained by policy. Session
lock excludes ordinary surfaces and compositor UI from both display and Agent
Seat capture. Failed DMA-BUF imports never expose stale renderer contents.

## Agent Seat

The wire/grant model is identical on X11 and Wayland, but Wayland enforcement
is compositor-owned. Peer credentials precede grants; every request is checked
again at execution/readback time. Capture masks hidden/redacted output regions,
input is client-relative, human input preempts injection, and consent plus the
unforgeable indicator are compositor surfaces excluded from capture.

Accessibility runs in a disposable helper with a hard deadline. Native client
credentials, process correlation, and exact frame rectangles are verified
before invocation and rechecked before session-local semantic handles are
released. PIDs and backend object IDs never enter the wire.

## Logs and files

Runtime directories are owner-only and manager sockets are mode 0600. Tests
verify cleanup after normal exit and crash residue. Logs may contain backend,
renderer, output topology, numeric policy identities, process IDs, and socket
paths needed for diagnosis. They do not record client titles/classes, command
strings, desktop-entry requests, activation tokens, clipboard data, Agent Seat
payloads, semantic content, captures, or pixels. The static W9 audit rejects
those tracing fields and any `unsafe` Nobox Rust declaration.

The exact dependency/license/advisory record is in
[wayland-dependencies.md](wayland-dependencies.md). Source builds are supported;
binary distribution remains outside the project scope.
