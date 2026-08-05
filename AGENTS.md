# nobox project guidance

`nobox` is a small, predictable Openbox-inspired window manager written in
Rust. The current priority is hardening and dogfooding the feature-complete X11
baseline. Wayland may follow later as a native backend/compositor, but must use
the same protocol-neutral policy rather than making X11 the internal model.

Use `../openbox` as the primary behavioral reference and regression oracle.
Honor its user-visible behavior and accumulated edge cases where they remain
useful; do not preserve obsolete internals merely because Openbox uses them.
Nobox code is independently implemented in Rust.

## Boundaries

- `nobox-core` owns display-server-neutral policy, geometry, focus, stacking,
  workspaces, outputs, and client state. X11 types must not enter it.
- `nobox-x11` owns ICCCM/EWMH translation, X resources, input, and decorations.
- `nobox-config` owns the strict TOML model. Keep one main config file and the
  intentionally simple Openbox-style `autostart` script.
- `nobox` stays a thin CLI/session executable. `nobox-settings` is an optional
  separate GTK/libadwaita application, never a toolkit dependency of the WM.
- Prefer small, typed, testable changes. Unsafe Rust is forbidden.

See `docs/architecture.md`, `docs/x11-acceptance.md`, and
`docs/openbox-compatibility.md` for detailed decisions and scope.

## Build and test

CMake with Ninja presets is the developer-facing workflow; Cargo remains the
Rust build and dependency layer.

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

Use `/usr/bin/ctest` explicitly. On this development system, the earlier
`ctest` on `PATH` is a broken user-local Python wrapper, not CMake's binary.
The development executable is `build/dev/cargo/debug/nobox`.

X11 integration tests use isolated Xnest, Xephyr, or Xvfb displays. Xnest does
not normally provide GLX here, so use simple clients such as `xterm` for smoke
tests; GLX failures are not automatically nobox failures. Add focused unit and
nested-X regression coverage for behavior changes, including relevant Openbox
comparisons.

## Repository discipline

- Keep source and documentation concise, modern, and free of generated files.
- Preserve unrelated user changes. `tmp/` contains local observations and must
  not be staged, modified, or removed unless explicitly requested.
- The remote is the user's Gitea server, not GitHub. Commit and push `main` to
  `origin` after each successful, fully verified milestone.
- Binary distribution is out of scope; source builds and installation are the
  supported workflow.
