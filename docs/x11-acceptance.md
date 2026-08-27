# X11 baseline acceptance

The first nobox X11 baseline is feature-complete for its stated window-manager
scope: a dependable Openbox-class reparenting manager with a protocol-neutral
policy core, ICCCM/EWMH interoperability, modern typed configuration, and no
required desktop environment or compositor.

“Complete” here does not mean every historical X11 extension or every private
Openbox implementation detail. It means the behaviors advertised by nobox are
implemented, the current Openbox regression corpus has an explicit compatibility
decision, and the manager passes the acceptance gates below. The compatibility
inventory is in `openbox-compatibility.md`; the implementation checklist is in
`x11-roadmap.md`.

## Acceptance evidence

- The Rust crates have 390 tests covering shared geometry, client policy,
  configuration, session restore, settings transactions, and X11 translation.
- The CMake/Ninja gate has 53 tests. Thirty-one are labeled `openbox`; the rest
  cover CLI, settings, diagnostics, and additional X11 integration contracts.
- Nested-X tests exercise real client event streams and server-observed
  properties, parentage, focus, stacking, geometry, selections, shapes,
  colormaps, synchronized resize, RandR behavior, and crash recovery.
- The standalone screenshot regression exercises full-screen, active-window,
  pointer, drag-area, file, stdout, and minimal-session clipboard-refusal paths
  against real nested X pixels. Encoder fixtures and a visual text check cover
  JPEG qualities 60 and 80 alongside lossless PNG.
- Agent-seat integration tests exercise bounded accessibility discovery through
  real GTK and Qt bridges, normalize both screen-coordinate and positionless
  roots, and compare semantic bounds and payload bytes with a typed grounded
  capture. When Zen or Firefox is installed, an additional
  private-session regression finds a checked-in HTML video semantically and
  derives its content-relative center without reading pixels. At 150% CSS
  scaling it repeats wide, narrow, then wide, checks responsive bounds against
  each grounded capture, and proves a canvas-only target falls back to pixels.
  A separate Chromium fixture repeats the safe-unavailable and grounded-capture
  path three times.
- The live agent-seat test substitutes crashing, truncating, oversized, and
  valid disposable semantic helpers, then exercises human cancellation,
  disconnect, freeze, and revocation without losing manager availability.
- A clean release build installs `nobox`, the optional `nobox-panel`,
  `nobox-xsmp`, and `nobox-settings` helpers when enabled or when dependencies
  are present, the X session entry, the settings desktop entry, and the exact
  validated example configuration.
  The installed manager passes the nested-X smoke test from the staged prefix.
- Read-only dogfood on two 2560x1600 outputs repeats semantic-unavailable and
  grounded capture on visible windows at both root origins. An Electron window
  on a hidden workspace returns typed `unsupported` capture until restored.
- The 2026-08-05 five-run smart-placement comparison records lower first-client
  latency, 50-client latency, idle RSS, loaded RSS, and resolved dependency bytes
  than the installed Openbox 3.6.1 on the same host. The nobox executable itself
  is larger; `performance.md` retains both the favorable and unfavorable data.
- `main` is pushed only to the configured GitHub origin. Generated builds remain
  under ignored `build/` and `target/` directories; the source worktree is clean
  after each accepted milestone.

## Intentional boundaries

- Nobox is a window manager, not an X11 compositor. Client opacity is mirrored
  onto top-level frames for an external compositor.
- The automated Xnest gate validates window-management protocols rather than
  accelerated client rendering. Applications requiring GLX need a real Xorg
  session or a nested server independently confirmed to expose GLX.
- Openbox's private dockapp container and persistent keyboard chroots are not
  reproduced. Standards-based dock clients and finite typed key sequences cover
  the user-facing workflows without preserving obsolete internal machinery.
- Icon minimize animation and taskbar-provided icon geometry are not advertised;
  nobox does not render desktop icons or own a taskbar animation.
- Per-action XML-era switcher presentation flags are consolidated into one typed
  switcher policy. This keeps cycle behavior predictable and the configuration
  surface bounded.
- Wayland protocols, rendering, input seats, and outputs remain a later backend.
  They must translate into shared nobox policy rather than turn X11 into the
  internal model.
- Binary distribution is outside this baseline. Source installation through the
  CMake/Ninja workflow is the supported path.

## Stability statement

Automated completeness is not the same as years of desktop exposure. This
baseline is ready to install and dogfood, while `nobox doctor`, nested-X startup,
clean restart, crash-safe save-set recovery, and easy fallback to Openbox remain
the recommended safety net during real-session testing.
