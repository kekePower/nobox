# nobox

`nobox` is an experimental, Openbox-inspired X11 window manager written in
Rust. The immediate goal is a small, dependable X11 daily driver. A native
Wayland compositor may follow only after the window-management policy and its
Openbox compatibility tests are mature.

The current X11 implementation is real but still experimental. It can own an X11
screen, adopt existing top-level windows, manage newly mapped clients, honor
configure requests, track focus and stacking, publish basic EWMH properties,
own the ICCCM window-manager selection, answer its required conversion targets,
and hand it over without disturbing application clipboard selections. It draws
crash-safe reparenting frames with configurable titles,
minimize/maximize/close buttons, and can move or resize a
window with Super + mouse. It also provides named workspaces with EWMH pager
interoperability, sticky clients, window moves, and independent focus history.
New unpositioned clients use deterministic least-overlap smart placement;
explicit ICCCM positions are preserved, and dialogs center over their parents.
Specific and ICCCM group-transient families retain Openbox-compatible stacking,
workspace movement, modal focus, and cycle-safe behavior.
Taskbar/pager visibility and urgency hints update live; urgent clients use a
distinct theme palette, and taskbar-skipped clients stay out of Alt+Tab. A
lightweight, output-aware title list makes modifier-held focus cycling visible;
releasing the modifier commits the selection and Escape restores the original
window.
RandR monitors are selected through shared output policy for placement,
maximize, fullscreen, per-monitor struts, and safe recovery after disconnects;
validated EWMH fullscreen-monitor requests can span selected output edges.
Servers without RandR retain a single-root fallback.
When the X Shape extension is available, client bounding and input regions are
propagated to their frames and followed across live shape changes; ordinary
rectangular clients and servers without Shape keep the zero-overhead fallback.
Strict TOML menu definitions provide nested, action-backed popup menus without
a toolkit or required secondary config file. Dynamic menus expose live client
operations, workspace destinations, and a workspace-grouped window list while
keeping X11 identifiers out of the shared configuration model. Command-backed
menus can also generate the same typed entries on demand under explicit time,
size, and UTF-8 bounds.

## Try it safely

The recommended first run is inside a nested X server:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset test

Xnest :2 -geometry 1280x800 -ac &
DISPLAY=:2 ./build/dev/cargo/debug/nobox doctor
DISPLAY=:2 ./build/dev/cargo/debug/nobox
DISPLAY=:2 xterm &
```

`nobox doctor` is read-only: it validates the effective config and saved session,
then reports the X server, screen, outputs, configured font, RandR/Shape/Sync
availability, and any existing WM selection owner without claiming events or
changing the desktop. Missing optional extensions are warnings with explicit
fallbacks; an invalid config/session, unreachable display, or unavailable font
makes the command fail with `ready: no`.

For an honest local size, startup, RSS, and 50-client comparison with the
installed Openbox, build the opt-in performance target:

```sh
cmake --preset release
cmake --build --preset performance
```

The report runs both managers on isolated nested X servers and leaves no build
artifact in the source tree. It has no fragile timing gate and reports unfavorable
results too; the exact method and current local evidence are in
[`docs/performance.md`](docs/performance.md).

Drag a titlebar with the left mouse button to move a window, or drag a border
to resize from that edge or corner. The legacy Super + left/right gestures move
or resize from anywhere in the frame, and Escape cancels either operation.
Both operations snap to work-area edges using the configurable mouse
resistance. Double-clicking a titlebar toggles maximize, middle-clicking lowers
the window, the desktop wheel changes workspaces, and right-clicking the root
opens the configured `root` menu. Menus support pointer selection, wheel and
arrow-key navigation, Home/End, Enter, Left/Right submenu traversal, and Escape
or an outside click to dismiss. An underscore marks a keyboard accelerator;
use two underscores for a literal one. Right-clicking a titlebar opens its
client menu, middle-clicking the root opens the window list, and Alt+Space
opens the focused client's menu. The initial keyboard
actions include Alt+Tab/Alt+Shift+Tab to cycle windows, with an on-screen list
while Alt remains held and Escape to cancel. Super+Return starts `xterm`,
Super+Q closes the focused client, Super+D toggles EWMH show-desktop mode, and Super+Left/Right switches
workspaces, Super+Shift+Left/Right to move the focused window, and
Super+Shift+Escape to exit nobox. Start with a nested server and deliberate
dogfooding before replacing a daily Openbox session; the compatibility gate is
broad, but the project has not yet earned years of real-desktop exposure.

## Configure

`nobox` uses one strict TOML file plus the intentionally simple Openbox-style
autostart script:

```sh
nobox-settings
```

The optional native settings application exposes the daily-driver focus,
workspace, pointer, overlay, and appearance controls as validated forms. Its
window-chrome specimen follows the active theme values, and its Advanced TOML
page retains complete access to bindings, menus, and application rules. Every
friendly edit preserves comments and unrelated TOML, and **Save changes** parses
the entire canonical `nobox-config` model before an atomic user-only file
replacement. Invalid or oversized input remains on screen with an actionable
error and cannot replace the last valid file. Unsaved changes are confirmed on
close. Choose **Reconfigure** from the nobox session menu after saving to apply
the new file in place.

The same workflow remains available without GTK/libadwaita:

```sh
cargo run -p nobox -- init
$EDITOR ~/.config/nobox/config.toml
$EDITOR ~/.config/nobox/autostart
cargo run -p nobox -- check
```

Unknown configuration keys are errors instead of silently ignored typos. If no
file exists, the built-in defaults are used. `NOBOX_CONFIG_FILE` and `--config`
make isolated tests easy.

Existing Openbox 3 themes can seed the same single-file configuration:

```sh
nobox import-openbox-theme ~/.themes/Clearlooks \
  --output ~/.config/nobox/config.toml
nobox check
```

The source may be a `themerc`, an `openbox-3` directory, or its parent theme
directory. Without `--output`, the generated `[theme]` TOML is printed to
stdout. Output creation is non-destructive unless `--force` is explicit. The
importer handles Openbox/X11 hex, `rgb:`, named, and grey-percentage colors,
maps representable borders, padding, alignment, title/button colors, and emits
notes for gradients, separate inactive text colors, and legacy properties that
do not have an honest nobox equivalent. The generated minimal file is validated
by the normal config model and inherits defaults for all non-theme settings.

The `[mouse]` table keeps the backward-compatible Super-drag shorthand,
`edge_resistance`, `drag_threshold`, `double_click_ms`, and validated
`[[mouse.bindings]]` together. Bindings combine a context (`root`, `desktop`,
`client`, `frame`, `titlebar`, `border`, individual edges/corners, or a titlebar
button), a button chord, a `press`/`release`/`click`/`double_click`/`drag`
trigger, and one ordered `action` or `actions` list. Button chords use the same
`C`/`A`/`S`/`W` modifiers as keys plus `Left`, `Middle`, `Right`, `Up`, or
`Down`. Specific decoration contexts fall through to their useful aggregate
context. Resistance is measured in pixels and may be zero to disable magnetic
work-area edge snapping.

The `[placement]` table controls smart initial placement. Nobox scores
decorated outer rectangles on a grid formed by existing window and work-area
edges. `center_free_space = true` centers a window within the first completely
free field. ICCCM user/program positions are honored, existing windows adopted
at startup are not moved, and dialogs or splashes follow Openbox-style
parent/work-area centering.

The `[switcher]` table controls the focus-cycle list without introducing a UI
toolkit. `enabled` toggles it, while `width`, `row_height`, and `max_rows` set
bounded geometry. The list follows the selected window's output, stays inside
small outputs, scrolls around the current selection, and reuses the active and
inactive theme colors. These settings reload with the rest of the file.

The `[focus]` table controls initial focus, pointer-follow focus, and raising.
`follow_mouse = false` keeps the click-to-focus default; enabling it focuses a
client on normal pointer entry and follows the existing `raise_on_focus` policy.
Pointer entries caused by grabs, menus, drags, or the focus switcher are ignored.
Focus changes initiated outside nobox are reconciled through the X focus tree:
toolkit child windows resolve to their managed top-level, while temporary
keyboard/pointer grab events and ancestor/inferior transitions cannot corrupt
the active-window or focused-state properties.
With the default `prevent_focus_stealing = true`, nobox compares wrap-safe X11
user timestamps, honors `_NET_WM_USER_TIME_WINDOW`, and rejects stale application
activation requests. Explicit pager/taskbar requests and related transient
families remain eligible. A denied client receives demands-attention state and
the urgent theme instead of interrupting the active window.

The `[menu]` table keeps presentation bounds and all named menu definitions in
the same strict TOML file. Each definition has an `id`, title, and `source`.
The default `static` source uses ordered entries typed as `item`, `submenu`, or
`separator`; items accept the same singular `action` or ordered `actions` forms
as input bindings. The `client`, `client_workspaces`, and `windows` sources are
generated from live state and therefore reject configured entries. A `command`
source instead runs its required `command` whenever the menu opens. It must
finish within `command_timeout_ms` (1000 ms by default) and emit at most 64 KiB
of UTF-8 TOML containing `[[entries]]` in the same
`item`/`submenu`/`separator` schema. The process receives no stdin and stderr is
discarded; a timeout, failed exit, malformed output, unknown submenu, cycle, or
invalid action leaves the menu closed and writes a warning. For example:

```toml
[menu]
command_timeout_ms = 500

[[menu.definitions]]
id = "projects"
title = "Projects"
source = "command"
command = "nobox-project-menu"
```

The generator output can be as small as:

```toml
[[entries]]
type = "item"
label = "_Terminal"
action = { type = "execute", command = "xterm" }
```

`execute` can optionally show a native grabbed confirmation and carry portable
launch metadata:

```toml
action = { type = "execute", command = "xterm -e tool --window $wid --at $pointer", prompt = "Open a terminal?", startup_notify = { name = "Terminal", icon = "utilities-terminal", wm_class = "XTerm" } }
```

`$pid`, `$wid`, and `$pointer` expand case-insensitively from the action target
and triggering pointer location; unavailable client values become `0`. On X11,
startup metadata is translated into the freedesktop startup-notification
messages and `DESKTOP_STARTUP_ID`. Matching `_NET_STARTUP_ID`, `WM_CLASS`, or
binary identity supplies the launch timestamp and workspace without overriding
a client's explicit `_NET_WM_DESKTOP` request. Failed and stale launches are
completed automatically, and executed children are reaped without blocking the
window-manager loop.

Submenu references, duplicate IDs, empty static or generated menus, text and
geometry bounds, and cycles are rejected before display. `show_menu` actions
can open a named menu from any key or pointer binding. The built-in root menu
is bound to an unmodified root right-press and links the live window list and a
nested session menu. Client menus expose only operations allowed for their
target; window-list activation changes workspace, restores an iconic client,
and focuses it. Clients marked to skip taskbars are also excluded from that
list.

The `[workspaces]` `names` array is deliberately the only workspace-count
setting: four names mean four workspaces. Names and count reload in place, and
clients on removed workspaces move to the final survivor. `columns = 0` uses a
single row; a positive column count creates a rectangular grid, and `wrap`
controls navigation at its edges. A standards-compliant EWMH pager that owns
the desktop-layout selection may override the visible grid while it is active.
Runtime add/remove actions update the in-memory names and EWMH workspace set;
they are intentionally session-local, so reloading restores the configured
list.

Ordered `[[applications]]` rules match a newly managed client's X11 instance
name, class, role, title, and functional kind. Text patterns are
case-insensitive and support `*` and `?`; every field in `match` must match.
Later matching rules override only the settings they specify. Rules can select
an initial one-based workspace, `below`/`normal`/`above` layer, decorations,
and initial focus behavior. They affect initial management rather than pinning
the client against later user actions. The shipped config contains a commented
example.

Send `SIGHUP` to a running nobox process to validate and reload the effective
TOML file in place. Invalid replacements are diagnosed and the active config is
kept. The same operation is available as the typed `reconfigure` action and
from the default session menu. The same menu also exposes `restart`, which
captures session state, releases X11 ownership, rebuilds the backend in the
same process, and does not rerun autostart. An optional `command` replaces
nobox after clean release, allowing an intentional handoff to another window
manager. `SIGINT` and `SIGTERM` request a clean event-loop shutdown, including
releasing input grabs and manager-owned X11 properties and selections.
The default session menu also exposes `session_logout`. Its grabbed confirmation
starts on **Cancel** and supports the normal pointer, arrow, accelerator, Enter,
and Escape menu controls. After confirmation nobox asks the connected XSMP
manager for a global interactive logout and remains alive until that manager
cancels or sends `Die`. With no usable session manager it falls back to the same
clean local exit as Openbox. Bindings may set `prompt = false` when a separate
trusted confirmation layer already exists; the XSMP request itself remains
interactive so applications may still participate in shutdown.
The distinct `exit` action releases only nobox and leaves session coordination
alone. It uses the same cancel-first confirmation by default; `prompt = false`
is the explicit immediate form.

On a clean exit, nobox atomically saves bounded window-session state at
`$XDG_STATE_HOME/nobox/session.toml` (falling back to
`~/.local/state/nobox/session.toml`). `nobox paths` prints the effective path,
and `NOBOX_STATE_FILE` overrides it. When `NOBOX_CONFIG_FILE` is used for an
isolated environment, the default session file is placed beside that config.
The next run restores the current workspace plus matched clients' normal
geometry, workspace/sticky assignment, minimized/shaded/fullscreen/maximized
state, taskbar/pager visibility, layer, stacking order, and focus. Matching
prefers `SM_CLIENT_ID` and falls back to `WM_COMMAND`, combined with class,
instance, role, and type; ambiguous duplicate identities are deliberately not
restored. Clients without either stable identifier are omitted.

When `SESSION_MANAGER` is present, a CMake build with `libSM`/`libICE`
development files automatically starts the optional `nobox-xsmp` companion. It
registers the current/restart identity and process metadata, turns
`SaveYourself` into an in-place durable snapshot, honors save completion and
shutdown cancellation, and routes `Die` through the same clean X11 release path
as a signal or exit action. It also carries confirmed `session_logout` requests
out to the external manager rather than treating them as an alias for killing
the window manager. The companion is a separate process: ordinary X11
sessions neither start it nor add `libSM`/`libICE` to the Rust executable's
dependencies. Application relaunch remains the session manager and each
application's responsibility; the intentionally simple autostart script remains
nobox's startup mechanism.

The theme schema includes border width, titlebar height, a server-provided X11
core `font`, `title_alignment`, `title_padding`, focused/unfocused/urgent border
and titlebar colors, title text, and button colors. Font names may be short
aliases such as `fixed` or XLFDs; `xlsfonts` lists what the active X server
provides. Titlebars, menus, and the focus switcher share one loaded font and use
its real ascent, descent, and per-character advances for clipping and vertical
placement. All typography settings reload in place; an unavailable font rejects
the reload and preserves the last working theme. Minimize, maximize/restore, and
close controls use bounded vector glyphs in `button_glyph`; hover adds a compact
outline and an active left-button press offsets and thickens the glyph. These
states are rendered directly with core X11 requests and do not require a UI
toolkit. A titlebar height of zero explicitly disables the titlebar without
requiring a second theme file.

Key chords use `C`, `A`, `S`, and `W` for Control, Alt, Shift, and Super,
followed by an X11 keysym name. Space-separated chords form Openbox-style key
sequences such as `W-x W-t`; incomplete sequences time out and the configured
quit chord cancels them. Legacy singular `action` remains valid, while `actions`
runs an ordered list at a sequence leaf. Caps Lock and Num Lock are ignored when
matching bindings. Available actions include command execution, polite close,
explicit client kill, bounded structured debug logging, exit/restart,
confirmed session logout,
focus, raise/lower, minimize/full-axis/independent-axis maximize, fullscreen,
reversible decorations, explicit idempotent maximize/decoration/shade/layer
state, always-on-top/bottom stacking, adaptive `raise_lower`, and the composite
`shade_lower`/`unshade_raise` actions,
absolute, last-used, linear, or four-direction workspace switching, runtime
workspace insertion/removal, moving the action target with optional
`follow = true` behavior, shading, desktop-showing mode, validated in-place reconfiguration,
forward/reverse window cycling, eight-way spatial window focus/cycling, named
menu display, bounded `if`/`for_each`/`stop` control flow, focus-history
demotion/fallback, and non-interactive relative move/resize. `focus_to_bottom`
demotes without changing current focus;
`unfocus` and `focus_fallback` select the next valid MRU client or clear focus.
`maximize` and `unmaximize` accept an optional `direction` of `both` (the
default), `horizontal`, or `vertical`; `decorate`, `undecorate`, `shade`, and
`unshade` set rather than toggle state. `send_to_layer` requires `below`,
`normal`, or `above`. Repeating any of these explicit actions is a no-op.
`last_workspace` toggles to the previously active workspace, while
`move_to_last_workspace` uses the same destination. `add_workspace` and
`remove_workspace` accept `at = "current"` or the default `at = "last"`;
removal merges clients safely and never removes the final workspace.
`if` and `for_each` require a `query = [{ ... }]` array and `then = [...]`
actions; `else = [...]` is optional, and `for_each` additionally accepts
`none = [...]`. Queries can inspect the action or focused target's state,
workspace, output, case-insensitive wildcard application name/class/role/title,
functional type, plus the active workspace. Multiple queries are ANDed. `stop`
terminates the current nested list and the enclosing `for_each` loop; trees are
limited to eight nested levels and 128 actions per root to keep configuration
hostile-input safe. `debug` requires a `message` of at most 1024 bytes and
writes it through the same structured runtime logger as backend diagnostics.
`focus_direction` and
`cycle_direction` accept `left`, `right`, `up`, `down`, and their diagonal
combinations; they select by visible outer geometry and the cycle form previews
until modifier release. A committed result is unshaded, focused, and raised;
Escape restores focus without changing the previewed client's shade state.
Relative amounts accept signed pixels, percentages such as `10%`,
or fractions such as `1/4`; movement fractions use the active work area while
resize fractions use the client's current dimension. Pointer bindings additionally
provide interactive move and resize actions; menu actions can also toggle a
client's all-workspaces assignment. `move_to_edge` walks a client toward the
next overlapping window edge or the active work-area edge; invoking it again at
an obstacle steps across that obstacle. `grow_to_edge`, `shrink_to_edge`, and
`grow_to_fill` resize against the same visible obstacle field, preserve ICCCM
size constraints, and retain Openbox's blocked-growth fallback. `move_resize_to`
adds absolute placement with start, `center`, or negative end-edge coordinates,
positive pixel/fraction sizes, content/outer size bases, and typed
current/primary/next/previous/all/numbered output selection; `move_to_center`
keeps the current size. A focus cycle snapshots visible,
focusable clients in most-recently-used order while its modifier remains held;
modal families appear as their active focus target. Its backend-owned overlay
contains only core-selected client titles; modifier release commits the current
target and Escape restores the snapshot's original focus. Linear and spatial
cycles share one bounded snapshot, keyboard grab, overlay, and cleanup path.
Focus assignment respects the ICCCM `WM_HINTS` input model and
`WM_TAKE_FOCUS` protocol. Client-requested and Super+right-drag resizing honor
ICCCM minimum/maximum sizes, base sizes, and resize increments.
Client resize requests also preserve the anchor described by window gravity.
Clients with their own titlebars or resize grips can delegate pointer or
keyboard interaction through EWMH `_NET_WM_MOVERESIZE`. Nobox retains bounded
pointer and keyboard grabs, applies the same work-area resistance and size-hint
constraints as native frame drags, commits on the initiating button release or
Enter, and restores the exact starting geometry on Escape or an explicit
cancel request. Keyboard movement uses eight-pixel steps, Control for
single-pixel adjustment, and Shift to jump to a work-area edge.
Interactive resizes use EWMH `_NET_WM_SYNC_REQUEST` pacing when a client opts
in and the X Sync extension is available. Nobox initializes the advertised
counter, sends each sequence before its configure, and keeps only the latest
motion while waiting for the client to repaint. A one-second missed
acknowledgement disables pacing for that drag so an unresponsive client cannot
freeze the user's resize; clients and servers without the protocol keep the
direct path.
Modal transients, including ICCCM window groups, receive focus and are raised
when an application tries to activate a blocked parent or group member.
Specific transient families move between workspaces together, inherit higher
parent layers, and remain stacked above their parents even after restacking or
relationship changes.
ICCCM iconic initial state and `WM_CHANGE_STATE` requests keep clients managed
while unmapped; activating an iconified client restores it normally. Genuine
minimization publishes EWMH hidden state, while off-workspace windows are not
misreported as minimized. Read-only EWMH focused state tracks the decorations
across direct focus, Alt+Tab, minimization, and workspace changes.
Client and pager restacking requests support all X11 stack modes while keeping
the EWMH stacking list synchronized with the server's actual order.
Framed clients publish `_NET_FRAME_EXTENTS`, retain content-root geometry across
configure requests, and are protected by the X save set if nobox terminates.
EWMH window types and Motif hints select per-client roles, capabilities, and
decorations; live hint changes update frames without remanaging the client, and
pre-map `_NET_REQUEST_FRAME_EXTENTS` estimates use the same policy.
UTF-8 and legacy X11 titles are mirrored onto frames and refresh live. The
minimize button uses the same ICCCM iconic/restore lifecycle as client requests.
Initial and runtime EWMH maximize requests support independent axes and preserve
exact restore geometry; the maximize button toggles both axes together.
Initial and runtime EWMH shading keeps the titlebar active while client content
is unmapped, preserves geometry changes made while shaded, and unshades before
fullscreen. Client menus expose shade only when the current frame supports it.
Dock and panel struts update `_NET_WORKAREA` dynamically, reflow maximized
clients, and fall back from `_NET_WM_STRUT_PARTIAL` to legacy `_NET_WM_STRUT`.
Work areas are independent per workspace: sticky docks reserve every workspace,
while local docks affect only their assigned workspace.
Desktop and dock roles do not steal focus and occupy their default EWMH layers.
EWMH show-desktop mode keeps those surfaces mapped while temporarily hiding
ordinary clients without changing their genuine minimized state; pager or
Super+D requests toggle the mode, and explicit client activation restores it.
The typed action defaults to Openbox's non-strict behavior, so launching a new
ordinary window also restores the workspace. Set `strict = true` on that action
when show-desktop must remain active across new windows.
Fullscreen clients cover the complete output without decorations, stay above
docks, reject application geometry churn, and restore maximized or normal
geometry exactly. EWMH above/below requests are mutually exclusive and remain
within the core's deterministic desktop/below/normal/dock/above/fullscreen
stacking model.
Legacy clients that exactly cover the root or one output without decorations
receive Openbox-compatible conditional fullscreen stacking without being
misreported as EWMH fullscreen. Their geometry remains client-controlled:
resizing or managed maximization leaves compatibility coverage immediately,
and exact coverage can be re-entered without hidden restore state.
EWMH skip-taskbar and skip-pager hints are honored both initially and at
runtime. Skip-taskbar clients are omitted from the MRU focus cycle. ICCCM
urgency and EWMH demands-attention share the urgent theme state; activation
clears demands-attention while leaving the client-owned ICCCM hint untouched.
Taskbars and pagers receive live EWMH allowed actions derived from the same core
capabilities used by nobox. Fixed-size clients do not advertise resize or
maximize, and fullscreen clients temporarily expose only meaningful actions.
Pager close requests use normal ICCCM `WM_DELETE_WINDOW` negotiation and policy
checks. Clients advertising `_NET_WM_PING` are checked once after a close
request. A timeout marks the frame as "Not Responding" without killing it; close
the marked window again to explicitly force-disconnect it, or let a late reply
restore it normally. The typed `kill` action is intentionally stronger: it
immediately disconnects the X11 client without sending `WM_DELETE_WINDOW` and
cleans up any pending ping deadline. Pager moveresize requests share ordinary
client geometry handling, including field masks, gravity anchoring, size
constraints, and synthetic configure notifications.
Shaped clients retain both their visible bounding region and pointer input
region after reparenting. The X11 backend adds the configured titlebar to those
regions, tracks Shape notifications, and returns the frame to its native
rectangle when the client clears a custom shape.

Useful commands:

```sh
cargo run -p nobox -- paths
cargo run -p nobox -- print-default
RUST_LOG=nobox=debug cargo run -p nobox -- run --display :2 --no-autostart
```

## Build, test, and install

CMake is the developer-facing build layer and uses Ninja presets. Cargo remains
the source of truth for Rust compilation and dependencies.

```sh
cmake --preset release
cmake --build --preset release
cmake --preset dev
cmake --build --preset check
cmake --build --preset test
cmake --install build/release --prefix ~/.local
```

The install includes `share/xsessions/nobox.desktop`, so a display manager can
offer a **nobox** X11 session after `~/.local/share/xsessions` is in its session
search path. A system-wide install normally uses `--prefix /usr` and appropriate
privileges.

If `pkg-config`, a C compiler, and the `sm`/`ice` development packages are
available, CMake also builds and installs the optional `nobox-xsmp` companion.
Direct Cargo builds remain fully functional but omit that companion; this keeps
XSMP libraries out of the default Rust executable and makes the protocol
integration an explicit local build capability.

When GTK 4.10 and libadwaita 1.5 development metadata are available, CMake also
builds and installs `nobox-settings` plus its desktop entry. The GUI is a
separate optional executable: neither GTK nor libadwaita is linked into the
window manager. Directly build it with
`cargo build -p nobox-settings --features gui`; ordinary `cargo build -p nobox`
continues to build only the small manager/session executable.

Direct Cargo workflows remain available:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo install --path crates/nobox
```

## Workspace

- `nobox-core`: protocol-neutral roles, capabilities, focus, layers, work
  areas, workspaces, fullscreen state, stacking, and geometry
- `nobox-config`: strict TOML config, defaults, validation, and XDG paths
- `nobox-x11`: X11 ownership, events, client management, and EWMH plumbing
- `nobox`: the small CLI/session executable
- `nobox-settings`: format-preserving settings model and optional native
  GTK/libadwaita editor
- `nobox-xsmp`: optional libSM/libICE companion built only by capable CMake hosts

See [docs/architecture.md](docs/architecture.md) for the design boundaries and
[docs/x11-roadmap.md](docs/x11-roadmap.md) for the staged compatibility plan.
The [Openbox compatibility matrix](docs/openbox-compatibility.md) records every
upstream fixture as direct, equivalent, policy-only, pending, or deferred work.

## License

GPL-2.0-only. We study Openbox's behavior and tests, but new code is written
independently in Rust.
