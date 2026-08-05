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
- `nobox-desktop` owns bounded XDG application discovery and safe desktop-entry
  command parsing shared by menus and the optional panel.
- `nobox` stays a thin CLI/session executable. `nobox-settings` is an optional
  separate GTK/libadwaita application, never a toolkit dependency of the WM.
- `nobox-panel` is a separate optional EWMH process. Use `../tint2-17.1.3` as
  its behavioral reference without coupling panel failure to the WM.
- `agent-seat-proto` owns the Agent Seat Protocol wire format and nothing
  else. It depends on serde alone, never on a nobox crate, and stays
  extractable by `git mv`; "nobox" never appears in the protocol it defines.
  `nobox-core` may depend on it, since it is display-server-neutral.
- `nobox-agent` is the optional MCP companion. It is a translator with no
  authority: the manager re-validates every request against the session's
  grant, so nothing in the companion is a security boundary.
- Prefer small, typed, testable changes. Unsafe Rust is forbidden.

See `docs/architecture.md`, `docs/x11-acceptance.md`, and
`docs/openbox-compatibility.md` for detailed decisions and scope. The agent
protocol (WM-mediated AI agent access) is specified in
`docs/agent-protocol.md` and implemented per `docs/agent-roadmap.md`.

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
- The remote is GitHub. Commit and push `main` to
  `origin` after each successful, fully verified milestone.
- Binary distribution is out of scope; source builds and installation are the
  supported workflow.
- We push a git tag and then a proper source code release. I believe this
  can be done via a Github release config.

# Atlas Scout

## Code navigation

When Atlas Scout MCP tools are available, use them as the primary navigation path for repository
investigations that need the correct file, source range, or structural relationship. This includes
definitions, named symbols, unknown locations, callers, references, dependency paths, architecture,
and edit impact.

- If the file and range are already known, read that range directly.
- If a file is known but the relevant range is not, use `symbol_outline` before reading the file.
- If the location is unknown, use `symbol_search`, then `symbol_resolve` when exact metadata is
  needed.
- Use `symbol_references`, `symbol_graph`, `symbol_trace`, `symbol_path`, or `edit_impact` for the
  corresponding structural question.
- Read the exact source ranges returned by Atlas Scout before drawing conclusions or editing.
- Use `rg` or other raw-text search for literals, regexes, unmodeled text, unsupported/partial
  language coverage, an explicit user request, or after focused Atlas Scout queries miss.

Do not use shell text search as the default substitute for Atlas Scout when the task is structural
code navigation.
