# Configuration

nobox uses one strict TOML file, `~/.config/nobox/config.toml`, plus the
intentionally simple Openbox-style `~/.config/nobox/autostart` shell script.
Unknown configuration keys are errors instead of silently ignored typos. If no
file exists, the built-in defaults are used. `NOBOX_CONFIG_FILE` and `--config`
make isolated tests easy.

```sh
cargo run -p nobox -- init          # create a commented config with defaults
$EDITOR ~/.config/nobox/config.toml
$EDITOR ~/.config/nobox/autostart
cargo run -p nobox -- check         # validate without running
```

The autostart script runs once after Nobox claims the display. Its standard
output and error remain attached to the session log, matching Openbox, so a
failed command leaves an actionable diagnostic instead of disappearing.

Useful related commands:

```sh
cargo run -p nobox -- paths          # print effective config/autostart/session paths
cargo run -p nobox -- print-default  # print the built-in default configuration
RUST_LOG=nobox=debug cargo run -p nobox -- run --display :2 --no-autostart
```

Send `SIGHUP` to a running nobox process to validate and reload the effective
TOML file in place. Invalid replacements are diagnosed and the active config is
kept. The same operation is available as the typed `reconfigure` action and
from the default session menu.

## The settings application

```sh
nobox-settings
```

The optional native settings application exposes the daily-driver focus,
workspace/startup, reserved-edge, pointer, overlay, and appearance controls as
validated forms. Its Desktops page has an explicit desktop count and one
ordered name field per desktop; the Openbox-compatible default is four. Its
window-chrome specimen follows the active theme values, and its Advanced TOML
page retains complete access to bindings, menus, and application rules. Every
friendly edit preserves comments and unrelated TOML, and **Save and apply**
parses and saves through the same canonical `nobox-config::ConfigDocument` API
used by the rest of the project before an atomic user-only file replacement.
Invalid or oversized input remains on screen with an actionable error and
cannot replace the last valid file. Unsaved changes are confirmed on close.
When a Nobox session is running, Settings asks it to apply the saved file in
place. If no session accepts the reload, the file remains saved and Settings
reports that it will take effect when Nobox starts or is reconfigured.

The Panel page configures the separate optional `nobox-panel` process. It is
disabled by default. Enabling it creates an EWMH dock in an X11 session or an
independent layer-shell surface in a Wayland session, with a configurable
top/bottom position, height, padding, spacing, colors, and matching work-area
reservation. `items` orders `launchers`, `workspaces`, `tasks`, `spacer`, and
`clock` from left to right; `spacer` consumes the unused width. Each item may
appear at most once, while the existing `show_workspaces`, `show_tasks`, and
`show_clock` switches provide quick visibility controls.

`launchers` is an ordered list of desktop-entry IDs selected from the bounded
XDG application catalog in Settings. Task buttons can cover the current or all
workspaces and have a configurable maximum width. Left click activates a task
or minimizes the active task, right click requests a normal EWMH close, and the
wheel cycles tasks. Urgent and iconified windows are visually distinct. The
clock accepts a validated, single-line `strftime` format such as `%a %H:%M`.
Reconfigure replaces only the panel after its replacement has committed a
drawable surface, and panel failure never terminates the window manager. The
Wayland frontend uses standard workspace state and wlr output membership for
exact current/all task filtering; it uses no Nobox-private socket. Current
foreign-toplevel protocols do not publish urgency, so `urgent_background` is
effective on X11 but cannot yet be selected by a native Wayland task.

## `[wayland]`

`xwayland` enables the separately built XWayland compatibility server. It
defaults to `false` while W7 integration is being completed; configure with
`-DNOBOX_BUILD_XWAYLAND=OFF` to omit the implementation entirely. XWayland
failure never terminates the native compositor.

`input_method` is an optional argv for a native Wayland input-method process.
The first item must be an absolute executable path; Nobox passes later items
verbatim and never invokes a shell. The vector is limited to 32 items and
16 KiB in total. The empty default disables both text-input-v3 and
input-method-v2, so a session that does not use an IME exposes neither global.

When configured, Nobox starts the process with a private inherited
`WAYLAND_SOCKET`. Only that capability-bearing connection can enumerate
`zwp_input_method_manager_v2`; ordinary clients can enumerate
`zwp_text_input_manager_v3` but cannot race to claim input-method authority.
The compositor revalidates the connection rather than trusting process names,
PIDs, or client-supplied metadata. IME exit deactivates the active text input
without terminating or exposing the session. Changing `input_method` during a
live reload is retained as a validated file change but requires a compositor
restart before the process or privileged globals change.

## Importing Openbox themes

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
do not have an honest nobox equivalent. The generated minimal file is
validated by the normal config model and inherits defaults for all non-theme
settings.

## `[theme]`

The theme schema includes border width, titlebar height, a server-provided X11
core `font`, `title_alignment`, `title_padding`, focused/unfocused/urgent
border and titlebar colors, title text, and button colors. Font names may be
short aliases such as `fixed` or XLFDs; `xlsfonts` lists what the active X
server provides. Startup falls back to `fixed` when the configured font is
unavailable. Titlebars, menus, and the focus switcher share one loaded font
and use its real ascent, descent, and per-character advances for clipping and
vertical placement. All typography settings reload in place; an unavailable
font rejects the reload and preserves the last working theme. Minimize,
maximize/restore, and close controls use bounded vector glyphs in
`button_glyph`; hover adds a compact outline and an active left-button press
offsets and thickens the glyph. These states are rendered directly with core
X11 requests and do not require a UI toolkit. A titlebar height of zero
explicitly disables the titlebar without requiring a second theme file.

## `[focus]`

The `[focus]` table controls initial focus, pointer-follow focus, and raising.
`follow_mouse = false` keeps the click-to-focus default; enabling it focuses a
client on normal pointer entry and follows the existing `raise_on_focus`
policy. Pointer entries caused by grabs, menus, drags, or the focus switcher
are ignored. Focus changes initiated outside nobox are reconciled through the
X focus tree: toolkit child windows resolve to their managed top-level, while
temporary keyboard/pointer grab events and ancestor/inferior transitions
cannot corrupt the active-window or focused-state properties.

With the default `prevent_focus_stealing = true`, nobox compares wrap-safe X11
user timestamps, honors `_NET_WM_USER_TIME_WINDOW`, and rejects stale
application activation requests. Explicit pager/taskbar requests and related
transient families remain eligible. A denied client receives
demands-attention state and the urgent theme instead of interrupting the
active window.

## `[placement]`

The `[placement]` table controls smart initial placement. Nobox scores
decorated outer rectangles on a grid formed by existing window and work-area
edges. `center_free_space = true` centers a window within the first completely
free field. ICCCM user/program positions are honored, existing windows adopted
at startup are not moved, and dialogs or splashes follow Openbox-style
parent/work-area centering.

## `[switcher]`

The `[switcher]` table controls the focus-cycle list without introducing a UI
toolkit. `enabled` toggles it, while `width`, `row_height`, and `max_rows` set
bounded geometry. The list follows the selected window's output, stays inside
small outputs, scrolls around the current selection, and reuses the active and
inactive theme colors. These settings reload with the rest of the file.

## `[workspaces]` and `[margins]`

The `[workspaces]` `names` array is deliberately the only workspace-count
setting: four names mean four workspaces. Names and count reload in place, and
clients on removed workspaces move to the final survivor. `columns = 0` uses a
single row; a positive column count creates a rectangular grid, and `wrap` is
the default edge policy for custom directional actions. The shipped keyboard
bindings explicitly stop at grid edges, while workspace scrolling continues
around the ordered workspace list. A standards-compliant EWMH pager that owns
the desktop-layout selection may override the visible grid while it is active.
`initial` selects the one-based startup workspace unless a saved session
restores another. Runtime add/remove actions update the in-memory names and
EWMH workspace set; they are intentionally session-local, so reloading
restores the configured list.

The `[margins]` table reserves pixels at the outer screen edges on every
workspace independently of application-owned panel struts; work-area
publication, placement, and maximize policy all use the result. Settings
presents the names array as a count plus individual name fields while
preserving this single-source configuration model.

## `[outputs]`

Direct Wayland sessions use `[outputs]` for persistent connector preferences.
An empty `entries` array is the safe default: every connected desktop output
uses its preferred mode and is laid out from left to right. Rules use the exact
bounded connector name reported by the backend and may override enabled state,
mode, logical position, transform, scale, and primary selection:

```toml
[outputs]

[[outputs.entries]]
name = "eDP-1"
enabled = true
mode = "1920x1080@60"
position = { x = 0, y = 0 }
transform = "normal"
scale = 1.25
primary = true

[[outputs.entries]]
name = "DP-1"
enabled = true
mode = "2560x1440@143.973"
position = { x = 1536, y = 0 }
transform = "rotate90"
scale = 1
```

Omit `mode` to select the connector's preferred mode and omit `position` for
automatic placement. Refresh is optional in a mode string; when present it is
stored exactly in millihertz. Transform accepts `normal`, `rotate90`,
`rotate180`, `rotate270`, `flipped`, and the corresponding `flipped90`,
`flipped180`, or `flipped270` forms. Scale is bounded to 0.5x through 8x and
must be exactly representable in Wayland's 1/120 units. Connector names are
unique and at most one enabled entry may be primary.

The Displays page in `nobox-settings` provides the same validated controls,
including adding or removing connector rules. An empty rule list restores
automatic preferred-mode, left-to-right layout. Save and apply targets the
current Nobox Wayland session when Settings is launched as a Wayland client,
and otherwise uses the running X11 manager.

The direct backend applies the complete topology transactionally. A missing
connector does not invalidate the file, while an unavailable mode or a change
that would leave no usable output rejects that topology and retains the last
working one. X11 ignores these hardware preferences; window-management policy
continues to consume only the resulting protocol-neutral output geometry.

## `[commands]` and `[shortcuts]`

The `[commands]` table is the single source for standard terminal, screenshot,
active-window screenshot, and optional session-dialog commands. Typed
`launch_terminal` and `screenshot` actions resolve through it, so the root
menu, standard bindings, custom bindings, and the Commands page in Settings
cannot drift onto different executables. `[shortcuts]` exposes the common
Ctrl+Alt+T, Print, and Alt+Print aliases; the complete layered
`[[keyboard.bindings]]` model remains available for arbitrary key sequences
and actions. Configurations from older nobox releases transparently promote
the shipped `_Terminal`/`xterm` menu item to the semantic terminal action.

## `[mouse]`

The `[mouse]` table keeps the primary and compatibility drag modifiers,
`snap_to_windows`, `edge_resistance`, `drag_threshold`, `double_click_ms`, and
validated `[[mouse.bindings]]` together. Standard bindings are inherited by
default; matching configured identities override them, while
`disabled_bindings` and `inherit_defaults = false` express intentional
omissions. Bindings combine a context (`root`, `desktop`, `client`, `frame`,
`titlebar`, `border`, individual edges/corners, or a titlebar button), a button
chord, a `press`/`release`/`click`/`double_click`/`drag` trigger, and one
ordered `action` or `actions` list. Button chords use the same
`C`/`A`/`S`/`W` modifiers as keys plus `Left`, `Middle`, `Right`, `Up`, or
`Down`. Specific decoration contexts fall through to their useful aggregate
context. Resistance is measured in pixels and may be zero to disable all
magnetic edge snapping.
`snap_to_windows` is enabled by default and applies that distance to visible
decorated peers as well as the work-area boundary; Settings exposes it as a
simple switch.

## `[menu]`

The `[menu]` table keeps presentation bounds and all named menu definitions in
the same strict TOML file. Each definition has an `id`, title, and `source`.
`max_rows` bounds each page, including its `More...` continuation entry; menus
do not automatically scroll around the selected entry.

The default root menu includes an `applications` source that discovers visible
XDG desktop entries, applies user-over-system precedence, and sorts them into
stable FreeDesktop categories. The Applications submenu contains only those
categories, with the applications one level deeper, keeping the first two menu
levels compact. Applications launch their parsed `Exec` arguments directly
without treating desktop-file content as shell code. When an entry declares
several main categories, the first specific category wins; the generic
`Utility` category is used only when no more specific main category is present.
An empty `XDG_DATA_HOME` is treated as unset and uses the standard
`$HOME/.local/share` fallback.

The default `static` source uses ordered entries typed as `item`, `submenu`,
or `separator`; items accept the same singular `action` or ordered `actions`
forms as input bindings. The `client`, `client_workspaces`, and `windows`
sources are generated from live state and therefore reject configured entries.

A `command` source instead runs its required `command` whenever the menu
opens. It must finish within `command_timeout_ms` (1000 ms by default) and
emit at most 64 KiB of UTF-8 TOML containing `[[entries]]` in the same
`item`/`submenu`/`separator` schema. The process receives no stdin and stderr
is discarded; a timeout, failed exit, malformed output, unknown submenu,
cycle, or invalid action leaves the menu closed and writes a warning. For
example:

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

Submenu references, duplicate IDs, empty static or generated menus, text and
geometry bounds, and cycles are rejected before display. `show_menu` actions
can open a named menu from any key or pointer binding. The built-in root menu
is bound to an unmodified root right-press and links the live window list and
a nested session menu. Its final, separated entry exits nobox after
confirmation, while session-wide logout remains in the Session submenu. Client
menus expose only operations allowed for their target; window-list activation
changes workspace, restores an iconic client, and focuses it. Clients marked
to skip taskbars are also excluded from that list.

### `execute` actions and launch metadata

`execute` can optionally show a native grabbed confirmation and carry portable
launch metadata:

```toml
action = { type = "execute", command = "xterm -e tool --window $wid --at $pointer", prompt = "Open a terminal?", startup_notify = { name = "Terminal", icon = "utilities-terminal", wm_class = "XTerm" } }
```

`$pid`, `$wid`, and `$pointer` expand case-insensitively from the action
target and triggering pointer location; unavailable client values become `0`.
On X11, startup metadata is translated into the freedesktop
startup-notification messages and `DESKTOP_STARTUP_ID`. Matching
`_NET_STARTUP_ID`, `WM_CLASS`, or binary identity supplies the launch
timestamp and workspace without overriding a client's explicit
`_NET_WM_DESKTOP` request. Failed and stale launches are completed
automatically, and executed children are reaped without blocking the
window-manager loop.

## Keyboard bindings

Key chords use `C`, `A`, `S`, and `W` for Control, Alt, Shift, and Super,
followed by an X11 keysym name. Space-separated chords form Openbox-style key
sequences such as `W-x W-t`; incomplete sequences time out and the configured
quit chord cancels them. Legacy singular `action` remains valid, while
`actions` runs an ordered list at a sequence leaf. Caps Lock and Num Lock are
ignored when matching bindings.

The shipped Openbox-style defaults use `C-A-Left/Right` to switch desktops
without wrapping and `A-S-Left/Right` to move the active window and follow it;
the existing Super-arrow alternatives use the same boundary and follow policy.
Mouse-wheel workspace switching continues to wrap. Standard bindings are
inherited by default, configured bindings override the same sequence, and
`disabled_bindings` or `inherit_defaults = false` express intentional
omissions without copying the built-in keymap.

## Action reference

Available actions include command execution, polite close, explicit client
kill, bounded structured debug logging, exit/restart, confirmed session
logout, focus, raise/lower, minimize/full-axis/independent-axis maximize,
fullscreen, reversible decorations, explicit idempotent
maximize/decoration/shade/layer state, always-on-top/bottom stacking, adaptive
`raise_lower`, the composite `shade_lower`/`unshade_raise` actions, absolute,
last-used, linear, or four-direction workspace switching, runtime workspace
insertion/removal, moving the action target and following it by default
(`follow = false` opts out), shading, desktop-showing mode, validated in-place
reconfiguration, forward/reverse window cycling, eight-way spatial window
focus/cycling, named menu display, bounded `if`/`for_each`/`stop` control
flow, focus-history demotion/fallback, and non-interactive relative
move/resize.

Details on the less obvious ones:

- `focus_to_bottom` demotes without changing current focus; `unfocus` and
  `focus_fallback` select the next valid MRU client or clear focus.
- `focus` is deliberate activation: it restores an iconic target and follows a
  target on another workspace. Set `here = true` to move that target and its
  transient family to the active workspace instead.
- `maximize` and `unmaximize` accept an optional `direction` of `both` (the
  default), `horizontal`, or `vertical`; `decorate`, `undecorate`, `shade`,
  and `unshade` set rather than toggle state. `send_to_layer` requires
  `below`, `normal`, or `above`. Repeating any of these explicit actions is a
  no-op.
- `last_workspace` toggles to the previously active workspace, while
  `move_to_last_workspace` uses the same destination. `add_workspace` and
  `remove_workspace` accept `at = "current"` or the default `at = "last"`;
  removal merges clients safely and never removes the final workspace.
- `if` and `for_each` require a `query = [{ ... }]` array and `then = [...]`
  actions; `else = [...]` is optional, and `for_each` additionally accepts
  `none = [...]`. Queries can inspect the action or focused target's state,
  workspace, output, case-insensitive wildcard application
  name/class/role/title, functional type, plus the active workspace. Multiple
  queries are ANDed. `stop` terminates the current nested list and the
  enclosing `for_each` loop; trees are limited to eight nested levels and 128
  actions per root to keep configuration hostile-input safe.
- `debug` requires a `message` of at most 1024 bytes and writes it through the
  same structured runtime logger as backend diagnostics.
- `focus_direction` and `cycle_direction` accept `left`, `right`, `up`,
  `down`, and their diagonal combinations; they select by visible outer
  geometry and the cycle form previews until modifier release. A committed
  result is unshaded, focused, and raised; Escape restores focus without
  changing the previewed client's shade state.
- Relative amounts accept signed pixels, percentages such as `10%`, or
  fractions such as `1/4`; movement fractions use the active work area while
  resize fractions use the client's current dimension.
- Interactive `move` and `resize` actions use the invoking pointer gesture
  when one exists, or enter the grabbed arrow-key mode from a key binding or
  menu; Enter commits and Escape cancels. Pointer resize can set
  `edge = "left"`, `"top_right"`, and so on, while omission selects the frame
  edge or nearest corner. On a fixed-size but movable client, `resize` enters
  move mode as Openbox does. Menu actions can also toggle a client's
  all-workspaces assignment.
- `move_to_edge` walks a client toward the next overlapping window edge or
  the active work-area edge; invoking it again at an obstacle steps across
  that obstacle. `grow_to_edge`, `shrink_to_edge`, and `grow_to_fill` resize
  against the same visible obstacle field, preserve ICCCM size constraints,
  and retain Openbox's blocked-growth fallback.
- `move_resize_to` adds absolute placement with start, `center`, or negative
  end-edge coordinates, positive pixel/fraction sizes, content/outer size
  bases, and typed current/primary/pointer/next/previous/all/numbered output
  selection; `move_to_center` keeps the current size.
- The typed `kill` action is intentionally stronger than close: it
  immediately disconnects the X11 client without sending `WM_DELETE_WINDOW`
  and cleans up any pending ping deadline.

## `[[applications]]` rules

Ordered `[[applications]]` rules match a newly managed client's instance name,
class, window-group name/class, role, title, and functional kind. Text
patterns are case-insensitive and support `*` and `?`; every field in `match`
must match. Later matching rules override only the settings they specify.
Rules can select an initial one-based workspace or `all`,
`below`/`normal`/`above` layer, decorations, focus,
minimized/shaded/fullscreen/maximized state, pager/task-list visibility, and
work-area-relative position and size. Application position hints remain
authoritative unless `position.force = true`; restored session state has final
precedence. Rules affect initial management rather than pinning the client
against later user actions. The shipped config contains a commented example.

## `[agent]`

The agent seat is off, and no socket exists, unless `[agent].enabled` is set.
`docs/agent-protocol.md` specifies what it exposes and why,
`docs/agent-harness.md` walks through connecting a harness, and this section is
what you write to control it.

A grant binds to the absolute path of the companion's executable, optionally
narrowed to one `uid`. Nothing a connecting process declares about itself is a
matching key, so a truthful consent answer can never become a stored
authorization that something else can claim by choosing the same name.
`capabilities` accepts bundles (`observe`, `accessibility`, `capture`, `input`,
`manage`, `launch`) and individual atoms (`observe.titles`,
`observe.accessibility`, `manage.activate`,
`capture.client_obscured`, …); anything not listed is refused. An optional
`scope` restricts the grant to matching clients, which are then the only
clients that session can perceive at all.

```toml
[agent]
enabled = true
policy = "ask"      # deny, or ask with the manager's own consent dialog
suppression_ms = 750
kill_chord = "C-A-Escape"

[[agent.grants]]
label = "my harness"
executable = "/usr/bin/nobox-agent"
uid = 1000
capabilities = ["observe", "manage.activate"]
scope = { class = "Firefox" }
```

`policy = "ask"` shows a keyboard-only dialog the manager draws itself: `y`
allows the request once, `p` allows it and writes a grant into this file, and
`n` or Escape denies it. The dialog holds the keyboard while it is up, and the
session waits for an answer.

Every setting here takes effect on the next configuration reload, including
`enabled`: turning the seat off closes its socket, withdraws its advertisement,
and ends every session immediately, and turning it back on brings it up
without restarting the window manager. The settings application exposes seat
enablement, connection policy, suppression, the kill chord, and the list of
stored grants under **Agent seat**. Its application-launch editor provides
deny, selected-only, and all-installed-except-selected modes over the bounded
XDG catalog. Search, categories, desktop IDs, icons, and user-installed badges
describe what each row controls. User-installed entries have a separate switch
that remains off by default; configured entries stay visible and preserved
while that switch blocks them. Entries unavailable from the current catalog
are also preserved rather than silently removed.

`suppression_ms` is how long human input keeps agent input out; agent calls
during that window are refused as `interrupted` and report which steps had
already committed. `kill_chord` freezes every session at once and resumes them
when pressed again. It is handled in the manager's own input path ahead of any
agent traffic, so it works while a session is flooding the socket. Freezing is
not revocation: taking a grant away means removing it here, which takes effect
on the next configuration reload rather than at the next connection.

Launching runs installed code, so it starts closed:

```toml
[agent.launch]
policy = "allow_listed"     # deny, allow_listed, or allow_installed
allow = ["org.example.Editor.desktop"]
deny = []
user_entries = false        # user-installed entries stay unlaunchable
```

Individual applications can be kept away from agents with `agent_visibility`
on an `[[applications]]` rule. `redacted` keeps existence and geometry but
withholds the title and refuses capture and input; `hidden` makes the window
absent from every answer, and acting on its identity returns exactly what
acting on a window that never existed returns. Sensitivity only ever
increases while a window is managed, so a window cannot rename itself back
into view.

```toml
[[applications]]
match = { class = "Keepassxc" }
agent_visibility = "hidden"
```
