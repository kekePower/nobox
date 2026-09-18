# Changelog

All notable changes to nobox are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- Authenticate X11 event-loop wakeups with a per-process random cookie and
  remove the obsolete client-message shutdown opcode. Unrelated or forged X11
  traffic can no longer make Nobox exit cleanly and take a display-manager
  session down with it; the private runtime socket remains the only remote
  shutdown path.
- Advance the `nobox-x11` crate patch version to 0.3.4. Other crate versions
  are unchanged.

## [0.3.4] - 2026-09-12

### Added

- `nobox-screenshot -i` opens an optional native GTK chooser for screen,
  window, or selection captures, pointer inclusion, delay, and independent
  sound/outline controls. It closes before capture and shows save errors.
  CMake detects GTK independently of Settings; direct capture remains usable
  without the GUI feature.
- Successful file and clipboard captures request the themed shutter sound
  through optional `canberra-gtk-play`. `--no-sound` disables it; missing,
  failing, or stuck playback never fails the screenshot. Stdout stays silent.

### Changed

- Hold the input-transparent window capture outline for 400 ms instead of
  180 ms, preserving focus, live content, and saved pixels. `--no-flash`
  continues to suppress the outline independently of sound.
- Escape during area selection erases the drag outline and exits quietly
  without saving or showing success feedback or an error dialog.
- Advance the `nobox-screenshot` crate patch version to 0.3.3. Other crate
  versions are unchanged.

## [0.3.3] - 2026-09-11

### Fixed

- Avoid unnecessary quoting of the installed X11 session command. Mageia's
  Xsession treats a quoted single executable as an unknown session name and
  silently starts IceWM instead of Nobox. Absolute installation paths remain
  in use, and prefixes that require quoting retain it.
- Exercise desktop-entry command dispatch and nested-X startup during staged
  installation tests, including Mageia's executable lookup before shell parsing.
- Isolate saved state in nested tests, including runtime-control tests that
  use `--config` without relocating the state file. Clear inherited Nobox path
  overrides so the tests cannot overwrite the user's saved window layout.

## [0.3.2] - 2026-09-11

### Added

- Active-window screenshots give a brief black-and-white perimeter indication
  after delivery, without covering the center or changing client opacity.
  `--no-flash` disables feedback; stdout remains visually silent.

### Fixed

- Repaint frame borders as well as title text after an X11 expose event, so
  dismissing a screenshot outline or another overlay restores the decoration.
- Installed X11 login entries select their own absolute launcher path, avoiding
  older Nobox installations earlier on PATH. An older manager could otherwise
  keep launching `gnome-screenshot` for Alt+Print after a newer source install.
- Added live screenshot-content, visibility, focus, and input-shape regression
  checks for direct and Alt+Print captures against Nobox and Openbox.
- Wait for previous fixture clients to withdraw before reusing their X11 IDs,
  preventing intermittent Openbox workspace-comparison failures under Xnest.

## [0.3.1] - 2026-09-07

### Added

- Added `nobox-screenshot`, an independently failing X11 screenshot utility
  with the classic `gnome-screenshot` command surface plus explicit PNG/JPEG
  selection and bounded JPEG quality control. It covers full-screen,
  active-window, drag-area, pointer, delay, file, stdout, and persistent X11
  clipboard paths, with nested-X and encoder-size regression tests.

### Changed

- Made `nobox-screenshot` the default screenshot command. JPEG quality 60 and
  80 both retained readable UI text in the nested fixture, while measurements
  also document that lossless PNG can remain smaller for sparse, flat UI and
  that encoded byte size does not determine vision-model image-token use.

### Fixed

- Prevented covered application frames from flashing during X11 workspace
  switches. Incoming windows map from top to bottom before outgoing windows
  hide, and layer enforcement keeps lower windows below their final siblings.
  A 38-client visibility-event regression covers ordinary, maximized, and
  fullscreen windows against Nobox and Openbox.
- Limited source installs to end-user documentation instead of packaging the
  complete internal roadmap, acceptance, governance, dogfood, and historical
  release-note archive.
- Fixed nested-X extension probes that could falsely skip available XTest and
  Sync checks when `grep -q` closed the `xdpyinfo` pipe early.

## [0.3.0] - 2026-08-24

### Added

- Added an explicit native Wayland backend alongside the established X11
  session. It supports safe nested use and a direct DRM/KMS path with
  multi-output rendering, transactional mode changes and hotplug handling,
  output controls, and read-only startup diagnostics. The direct path remains
  pre-release until its guarded real-hardware acceptance record is complete.
- Brought the existing desktop policy to native Wayland clients, including
  decorations, focus and stacking, workspaces, application rules, placement,
  keyboard and mouse actions, menus, focus switching, session restoration,
  live reconfiguration, and restart.
- Added the bounded Wayland protocols needed by current desktop applications,
  including scaling and presentation feedback, clipboard and drag-and-drop,
  advanced pointer, touch and tablet input, text input, idle notification and
  inhibition, and secure session locking. GTK, Qt, SDL, and Chromium/Ozone
  clients are covered by the nested acceptance suite.
- Added a native layer-shell frontend for the optional `nobox-panel`, retaining
  its ordered components, workspace and task controls, launchers, clock,
  failure isolation, and readiness-safe replacement behavior.
- Added optional XWayland management through the same core policy as native
  clients, with lifecycle recovery, selections and drag-and-drop, activation,
  size hints, group and modal relationships, scaling, and toolkit coverage.
- Extended the Agent Seat to Wayland with the existing wire format and grant
  model, including scoped observation and events, management, correlated
  launches, privacy-aware capture, accessibility, input, consent, revocation,
  human preemption, and the kill chord.

### Changed

- Split the window manager and compositor into independently linked
  `nobox-x11` and `nobox-wayland` executables under `libexec/nobox`. The
  user-facing `nobox` command is now a tiny compatibility selector, and CMake
  can build/install either backend without linking in the other.
- Extracted backend-neutral CLI, autostart, panel, signal, and session
  supervision into `nobox-common`; dependency-boundary regression coverage
  prevents the X11 artifact from reaching Smithay and the Wayland artifact
  from reaching the X11 manager crate.
- The default source build now includes separate X11 and Wayland sessions and
  optional XWayland support; each component can still be omitted explicitly.
- Moved session control, process lifecycle, Agent Seat transport, and semantic
  translation behind display-server-neutral boundaries shared by both
  backends.

### Fixed

- Isolated nested X11 and Wayland tests from the login session's runtime,
  session D-Bus, AT-SPI bus, and systemd activation environment so a test
  accessibility service cannot replace the live desktop's socket.
- Made native Wayland absolute geometry actions and application placement
  rules honor the complete live multi-output selector model, including primary,
  pointer, wrapping next/previous, all-output bounds, and one-based indexes.
- Hardened direct-session startup and cleanup for LightDM, DRM handoff, output
  retention during CRTC validation, XWayland readiness, and display-manager
  recovery on the initial hardware dogfood system.
- Fixed direct-session redraw pacing, pointer damage, completed presentation
  feedback recycling, per-window decoration stacking, configured XKB layouts,
  and themed cursor fallback found during hardware dogfooding.

## [0.2.0] - 2026-08-14

### Added

- Promoted `nobox-panel` into a first-class optional desktop panel with ordered
  launchers, workspace and task controls, configurable task scope and mouse
  actions, a formatted clock, complete Settings controls, and readiness-safe
  live replacement.
- Added compact, paged XDG application menus with corrected category handling,
  improved submenu interaction, and a conventional root-level Exit action.
- Added a visible focus-cycle outline, clearer autostart diagnostics, and
  configurable resistance between decorated window edges during pointer moves.
- Added exact Unicode Agent Seat text entry, including bounded target-scoped
  selection transfer for characters outside the active keyboard layout and for
  long passages.

### Changed

- Settings now applies saved changes to the running Nobox session, including
  safe replacement of the optional panel.
- Reduced avoidable X11 round trips on command paths and kept the CMake and
  Cargo Rust toolchains coherent.
- Hardened Agent Seat capture and text transfers with bounded output crops,
  larger but strictly limited payloads, and a 250 ms post-conversion quiet
  period for clients that perform follow-up clipboard requests.
- Documented the independent Agent Seat product boundary and retained Nobox's
  GPL-2.0-only wire implementation without sharing source across licenses.

### Fixed

- Kept window-addressed Agent Seat input bound to the live interactive owner:
  pointer actions now fail before injection when another client covers the
  destination, and key actions fail when the named client no longer owns
  focus. Both return the structured re-observation retry path. Client-owned
  MCP captures now carry a compact warning that obscured pixels do not prove
  visibility, focus, or interactivity, with fresh-snapshot recovery guidance.
- Fixed focus-cycle, submenu, desktop-entry categorization, live Settings, and
  Agent Seat text/capture edge cases found through nested-X dogfooding.

## [0.1.3] - 2026-08-08

### Changed

- Bound the integrated seat atomically to `_AGENT_SEAT_S<screen>` on a
  dedicated owner window, with matching owner/root advertisements, duplicate
  refusal, seat-only selection-loss handling, and ownership-safe cleanup.
- Made `nobox-agent` discover a live selection-bound X11 advertisement after
  explicit and environment overrides, removing its Nobox-specific synthesized
  socket fallback.

### Fixed

- Preserved unrelated X11 events encountered while obtaining a fresh server
  timestamp during an in-place Agent Seat restart.

## [0.1.2] - 2026-08-08

### Changed

- Renamed Nobox's internal GPL-2.0-only Agent Seat wire crate and test probe to
  `nobox-agent-wire` without changing serialized names, framing, behavior, or
  wire revision.

## [0.1.1] - 2026-08-08

### Added

- Introduced the deny-by-default Agent Seat Protocol, with a bounded,
  versioned wire format, verified peer identity, per-executable capability
  grants, application scoping, sensitive-window privacy, and runtime grant
  revocation.
- Added structured desktop snapshots and a gap-detecting event stream,
  generation-based freshness checks, window and workspace management, safe
  desktop-entry launching with correlation tokens, and privacy-aware client
  and output capture.
- Added human-first agent input with window-relative pointer and keyboard
  actions, configurable preemption, atomic text validation, a kill chord, and
  window-manager-owned activity indicators.
- Added the optional `nobox-agent` MCP companion, installed by default, with
  stock-harness compatibility, lazy seat connection, setup output, diagnostics,
  native image responses, and cropped captures with exact coordinate metadata.
- Added machine-correctable agent errors and bounded post-action observations,
  including correlated events and optional captures that remain useful when an
  action closes a transient window.
- Added capability-separated accessibility observation through an isolated
  `agent-semantic-helper`, with generation-stamped semantic roots, bounded tree
  paging and search, toolkit and browser geometry, and fail-closed lifecycle
  handling.
- Added an Agent seat page to `nobox-settings` for enabling the seat, choosing
  consent behavior, configuring human-input priority and the kill chord, and
  reviewing or revoking stored grants. Its launch-policy editor provides
  deny-all, selected-only, and all-installed-except-selected modes over the
  bounded XDG catalog, with a separate user-entry switch.
- Added end-to-end, adversarial, accessibility, browser, and MCP regression
  coverage, plus protocol, harness, interface, threat-model, dogfooding, and
  troubleshooting documentation.
- Added repeated GTK, Qt, Firefox-family, Chromium, responsive-reflow, scaled
  browser, and canvas-fallback measurements with explicit call, payload, image,
  and elapsed costs.

### Changed

- Evolved the agent interface to machine-native MCP results while retaining
  compatibility with deployed MCP initialization revisions; discovery no
  longer connects to the seat or waits for consent.
- Made agent text entry paced, preemptible, focus-aware, and all-or-nothing for
  invalid input; clarified pointer button defaults and serialized semantic
  helper requests to avoid resource contention.
- Made observable sequence numbers session-local and advanced them for every
  visible desktop change, avoiding both missed changes and cross-scope
  information leaks.
- Strengthened capture grounding and privacy: image data is returned as native
  content, non-zero origins and crops preserve exact input coordinates, and
  covered or sensitive content is composited safely or refused.
- Unified CMake's shipped Rust binaries into one Cargo build so builds cannot
  partially succeed, added independent build toggles for the MCP and semantic
  companions, and improved incomplete-install diagnostics.
- Recorded desktop-entry provenance for launch policy and expanded the project
  overview, configuration, usage, architecture, acceptance, and performance
  documentation for the new agent surface.

### Fixed

- Fixed Agent seat startup, consent, remembered-grant, legacy MCP, response
  framing, image delivery, and input-delivery reporting issues found during
  real harness dogfooding.
- Fixed agent captures on outputs with non-zero origins and scheduled input
  highlight cleanup correctly.
- Fixed visible client capture when part of the requested window lies outside
  the X11 root by using the already-authorized Composite path.
- Fixed semantic helper crash, timeout, browser-targeting, toolkit-geometry,
  and query-schema edge cases without coupling helper failure to the window
  manager.
- Fixed work-area-aware X11 placement and pseudo-transparent frame updates.
- Fixed resize handles so they no longer overlap or consume client content.
- Fixed panel reload handling when tracked clients disappear mid-update.

## [0.1.0] - 2026-08-05

- Initial public release with the feature-complete X11 baseline.

[Unreleased]: https://github.com/kekePower/nobox/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/kekePower/nobox/releases/tag/v0.3.1
[0.3.0]: https://github.com/kekePower/nobox/releases/tag/v0.3.0
[0.2.0]: https://github.com/kekePower/nobox/releases/tag/v0.2.0
[0.1.3]: https://github.com/kekePower/nobox/releases/tag/v0.1.3
[0.1.2]: https://github.com/kekePower/nobox/releases/tag/v0.1.2
[0.1.1]: https://github.com/kekePower/nobox/releases/tag/v0.1.1
[0.1.0]: https://github.com/kekePower/nobox/releases/tag/v0.1.0
