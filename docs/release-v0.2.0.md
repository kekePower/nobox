# Nobox 0.2.0

Nobox 0.2.0 is a substantial X11 dogfooding release. It makes the optional
panel a first-class part of the desktop, improves everyday window and menu
interaction, and hardens the Agent Seat workflow around the real failure modes
found during long, complex GUI tasks.

## Desktop highlights

- `nobox-panel` now provides ordered launchers, workspace and task controls,
  configurable task scope and mouse actions, and a formatted clock. All
  daily-use options are available in Nobox Settings, and live reconfiguration
  keeps the working panel until its replacement is ready.
- The root menu has compact paged XDG application categories, more predictable
  submenu interaction, corrected application categorization, and a direct Exit
  action.
- Pointer moves gain configurable window-edge resistance, while focus cycling
  has a visible outline and autostart failures have clearer diagnostics.
- Saved Settings changes now apply to the running Nobox session.

## Agent Seat highlights

- Exact Unicode and long text entry use a bounded, target-scoped X11 selection
  transfer when the active keyboard layout cannot represent the requested
  text. Clipboard conversion remains alive through a short quiet period for
  clients that issue follow-up requests.
- Captures support bounded output crops and larger strictly limited payloads.
  MCP results keep image bytes out of duplicated text and clearly distinguish
  target-owned pixels from proof that a window is currently interactive.
- Window-addressed input now validates the live recipient immediately before
  injection. Pointer actions refuse destinations covered by another client's
  input region, and key actions refuse a named client that no longer owns
  focus. Both direct the caller to fresh structural observation without
  injecting into the wrong window.
- The Nobox GPL-2.0-only wire implementation remains source-separated from the
  independent Apache-2.0 Agent Seat product. This release incorporates only
  independently implemented behavior and protocol lessons.

## Versions and compatibility

- Every Rust workspace crate is version 0.2.0.
- The on-wire `agent-seat` protocol remains revision 10; the crate version bump
  does not change its neutral wire identity.
- Configuration remains in the strict canonical TOML model, with no X11 types
  introduced into `nobox-core`.

## Verification

The release passed the complete developer gate:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

The 54-test CTest suite includes isolated nested-X coverage for the window
manager, panel, Settings, Agent Seat, accessibility and browser integration,
window snapping, focus behavior, application menus, and the Openbox regression
oracle. Unit, lint, formatting, documentation, and native Settings checks run
through the CMake `check` preset.
