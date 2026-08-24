# Nobox 0.3.0

Nobox 0.3.0 adds a native Wayland compositor alongside the established X11
window manager. Both backends apply the same protocol-neutral desktop policy,
ship as independently linked executables, and are selected through the small
user-facing `nobox` command.

The X11 backend remains the mature daily-driver path. The Wayland backend is a
working baseline that passes nested release acceptance; its direct DRM/KMS path
remains pre-release until the guarded real-hardware acceptance record is
complete.

## Wayland highlights

- Native Wayland clients share Nobox's focus, stacking, workspace, placement,
  decoration, action, menu, session, and live-reconfiguration policy.
- The compositor implements the bounded protocols needed by current GTK, Qt,
  SDL, and Chromium/Ozone applications, including scaling, presentation,
  selections and drag-and-drop, advanced input, idle behavior, and session
  locking.
- The optional panel has a native layer-shell frontend with the same ordered
  components, workspace and task controls, launchers, clock, failure isolation,
  and readiness-safe replacement behavior as its X11 frontend.
- Optional XWayland clients are managed through the same core policy, with
  lifecycle recovery, selection and drag-and-drop bridging, activation, size
  hints, group and modal relationships, scaling, and toolkit regression
  coverage.
- Agent Seat support now covers native Wayland and XWayland clients with the
  existing grant model, observation stream, management, launching, capture,
  accessibility, input, consent, revocation, human preemption, and kill chord.

## Architecture and hardening

- `nobox-x11` and `nobox-wayland` are independently linked session executables
  under `libexec/nobox`; either backend and optional XWayland support can be
  omitted from a source build.
- Backend-neutral command handling, autostart, panel supervision, signals,
  session control, Agent Seat transport, and semantic translation now live
  behind shared display-server-neutral boundaries.
- Nested X11 and Wayland tests isolate their runtime, D-Bus, AT-SPI, and systemd
  activation environments from the login session.
- Direct-session startup and cleanup are hardened for LightDM, DRM handoff,
  output retention, XWayland readiness, and recovery on the initial hardware
  dogfood system.

## Versions and compatibility

- Every Rust workspace package is version 0.3.0.
- The on-wire `agent-seat` protocol remains revision 10; the package version
  change does not alter its neutral wire identity.
- Configuration remains in the strict canonical TOML model, and both display
  backends continue to use the protocol-neutral policy in `nobox-core`.

## Verification

The release passed the complete developer gate:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

The 68-test CTest suite covers isolated nested-X regressions, nested Wayland
acceptance, XWayland lifecycle and toolkit coverage, Agent Seat behavior,
staged installation, LightDM integration, backend dependency boundaries, and
the Openbox regression oracle. Formatting, Clippy with warnings denied, unit
and documentation tests, and native Settings checks run through the CMake
`check` preset.
