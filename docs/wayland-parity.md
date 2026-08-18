# Wayland backend parity matrix

This matrix began as the W3 accounting record for the public `nobox-config`
model and now describes the completed W5-W8 implementation plus the W4/W9 code
baseline awaiting its guarded physical record. It answers whether each existing
option has a native Wayland meaning without treating nested coverage as proof
of direct DRM lifecycle.

The status terms are deliberately strict:

- **native**: implemented for native Wayland clients through neutral policy and
  Wayland compositor mechanics.
- **XWayland-only**: meaningful only for an X11 client through the optional
  W7 XWayland/XWM boundary.
- **documented fallback**: accepted and deterministic, but a standard protocol
  or external-command boundary is narrower than the X11 behavior.
- **intentionally unsupported**: accepted configuration is diagnosed or kept
  inactive until the milestone named in the notes; it is not silently treated
  as implemented.

## Actions

Every `Action` variant is accounted for below. Parameters inherit the status of
their action unless a note says otherwise.

| Status | Actions | Native meaning or boundary |
| --- | --- | --- |
| native | `execute`, `launch_terminal`, `screenshot` | Starts the configured command with the compositor's `WAYLAND_DISPLAY`. Execute startup notification and desktop-entry launches receive a bounded, short-lived `XDG_ACTIVATION_TOKEN`; screenshot capture itself remains the selected external program's responsibility. |
| native | `show_menu`, `reconfigure`, `restart`, `session_logout`, `exit`, `debug` | Compositor-owned menu/confirmation UI, last-good live reload, typed process disposition, session snapshot, and clean handoff. |
| native | `if`, `for_each`, `stop` | Ordered action trees use neutral application/state queries and preserve short-circuit behavior. |
| native | `close`, `kill` | Sends `xdg_toplevel.close` or disconnects the owning xdg-shell client with the protocol's unresponsive error. |
| native | `focus`, `focus_to_bottom`, `unfocus`, `focus_fallback`, `raise`, `lower`, `raise_lower` | Uses `ClientSet` focus history and policy-layer stacking. |
| native | `minimize`, `maximize`, `unmaximize`, `toggle_maximize`, `toggle_maximize_horizontal`, `toggle_maximize_vertical`, `toggle_fullscreen` | Core state plus xdg-shell configure/state transitions, including per-axis restore geometry. |
| native | `toggle_always_on_top`, `toggle_always_on_bottom`, `send_to_layer` | Changes the neutral policy layer; this does not expose a client-controlled Wayland layer. |
| native | `decorate`, `undecorate`, `toggle_decorations` | Changes server-side decoration policy and rendered/hit-tested extents. |
| native | `toggle_sticky`, `shade`, `unshade`, `toggle_shade`, `shade_lower`, `unshade_raise`, `toggle_show_desktop` | Uses neutral workspace/presentation state; shading is compositor presentation rather than a Wayland protocol state. |
| native | `move`, `resize`, `move_relative`, `resize_relative`, `move_to_edge`, `grow_to_edge`, `grow_to_fill`, `shrink_to_edge` | Keyboard and pointer interaction use shared constraints, resistance, obstacle, and work-area policy. |
| native | `focus_direction`, `cycle_direction`, `next_window`, `previous_window` | Immediate focus and modifier-held preview sessions share the compositor switcher; Escape restores and primary-modifier release commits. |
| native | `previous_workspace`, `next_workspace`, `last_workspace`, `add_workspace`, `remove_workspace`, `workspace_left`, `workspace_right`, `workspace_up`, `workspace_down`, `switch_workspace` | Updates core state and publishes one atomic `ext-workspace-v1` result. |
| native | `move_to_workspace`, `move_to_previous_workspace`, `move_to_next_workspace`, `move_to_last_workspace`, `move_to_workspace_left`, `move_to_workspace_right`, `move_to_workspace_up`, `move_to_workspace_down` | Moves the client family and optionally follows it through the same workspace policy. |
| native | `move_resize_to`, `move_to_center` with every `output` selector | Uses the selected live output work area for sizing and placement. `current`, `primary`, and `pointer` use compositor topology; `next`/`previous` wrap in stable discovery order; `all` uses the bounding work area; one-based indexes are validated and a missing index is diagnosed without moving the client. |

No action is XWayland-only or intentionally unsupported. XWayland clients enter
these same actions through W7 instead of gaining a second executor.

## Configuration

The table lists every top-level section and every public leaf field in the
current schema.

| Section and fields | Status | Wayland behavior |
| --- | --- | --- |
| `focus`: `focus_new`, `follow_mouse`, `prevent_focus_stealing`, `raise_on_focus` | native | Map focus, pointer-entry focus, recent seat/serial validation, attention fallback, and configured raising are enforced. Disabling focus-stealing prevention permits any fresh compositor-known activation token; it does not permit invented or expired tokens. |
| `switcher`: `enabled`, `width`, `row_height`, `max_rows` | native | Bounds the compositor-owned linear and directional focus overlay. |
| `menu`: `width`, `row_height`, `max_rows`, `command_timeout_ms`, `definitions` | native | Static, command, applications, client, client-workspaces, and windows sources are implemented. Items, ordered actions, named/inline submenus, separators, accelerators, pointer/keyboard navigation, and recursive `_More...` continuation are bounded. |
| `commands`: `terminal`, `screenshot`, `window_screenshot`, `session` | native | Used by their semantic actions. Empty session command selects the compositor confirmation/exit path. |
| `shortcuts`: `terminal`, `screenshot`, `window_screenshot` | native | Folded into the effective key-binding model before backend dispatch. |
| `placement.center_free_space` | native | Selects shared smart-placement centering. |
| `margins`: `top`, `right`, `bottom`, `left` | native | Combined with layer-shell reservations by core work-area policy. |
| `workspaces`: `names`, `columns`, `wrap`, `initial` | native | Defines initial and live workspace topology and published names/layout. |
| `theme`: `border_width`, `titlebar_height`, `font`, `title_alignment`, `title_padding`, `active_border`, `inactive_border`, `urgent_border`, `active_titlebar`, `inactive_titlebar`, `urgent_titlebar`, `title_text`, `minimize_button`, `maximize_button`, `close_button`, `button_glyph` | native | Drives compositor rendering and the same geometry used by hit testing. Font fallback and glyph caching are bounded. |
| `theme.agent_marker` | native | Colors the compositor-owned Agent Seat marker and addressed-client highlight; ordinary Wayland UI never supplies or impersonates it. |
| `mouse`: `inherit_defaults`, `disabled_bindings`, `modifier`, `compatibility_modifiers`, `move_button`, `resize_button`, `snap_to_windows`, `edge_resistance`, `drag_threshold`, `double_click_ms`, `bindings` | native | The effective typed binding set handles exact context/modifiers and press, release, click, double-click, drag, and wheel triggers. |
| `keyboard`: `inherit_defaults`, `disabled_bindings`, `chain_quit_key`, `chain_timeout_ms`, `bindings` | native | Handles complete sequences, prefix timeout/cancel, press/release interception, and ordered actions. |
| application match: `name`, `class` | native | Both match the bounded `xdg_toplevel.app_id`, preserving the existing neutral matcher without importing Wayland objects into config/core. |
| application match: `title`, `kind` | native | Matches bounded title and native normal/dialog role classification. |
| application match: `group_name`, `group_class`, `role` | XWayland-only | Native xdg-shell has no ICCCM group or `WM_WINDOW_ROLE` equivalent. W7 populates them only for XWayland clients; a native client therefore cannot match a rule that requires one. |
| application settings: `workspace`, `layer`, `decorated`, `focus`, `minimized`/`iconic`, `shaded`, `skip_pager`, `skip_taskbar`, `fullscreen`, `maximized`, `position`, `size` | native | Applied on initial management, with session restoration taking final precedence. Position and relative size use the same complete live-output selector model as absolute geometry actions; an unavailable index leaves ordinary smart placement intact. |
| application setting `agent_visibility` | native | Drives the shared observation/scope projection. Hidden clients are absent, redacted clients retain structural placeholders, client capture is refused, and output capture masks their rendered frame and popup regions before readback. |
| `panel`: `enabled`, `position`, `height`, `background`, `foreground`, `active_background`, `padding`, `spacing`, `task_max_width`, `task_scope`, `items`, `launchers`, `clock_format`, `show_workspaces`, `show_tasks`, `show_clock` | native | Starts the independent layer-shell frontend. Standard workspace publication and wlr output membership provide exact current/all task scope; task, workspace, launcher, and clock behavior retain the readiness replacement contract. The compositor does not absorb panel rendering or process health. |
| `panel.urgent_background` | documented fallback | The canonical option remains effective for X11. Neither `ext-foreign-toplevel-list` v1 nor wlr foreign-toplevel v3 publishes urgency, so the native panel cannot distinguish attention state without a private extension. |
| `agent`: `enabled`, `socket`, `policy` | native | Controls the private Wayland socket and stored-grant default. Compositor-launched children receive the exact live `AGENT_SEAT_SOCKET`; no X11 discovery property or synthesized path is added. |
| `agent`: `suppression_ms`, `kill_chord` | native | Human input suppresses/interrupts agent injection for the configured interval, and the compositor intercepts the kill chord before locks, inhibitors, menus, or ordinary bindings. |
| `agent.grants`: `label`, `executable`, `uid`, `capabilities`, `scope` | native | Verified peers receive only realized atoms. Observation, management, launch, capture, window-addressed input, and native accessibility apply shared scope/privacy/generation policy; consent and live revocation remain compositor-owned. |
| `agent.launch`: `policy`, `allow`, `deny`, `user_entries` | native | Bounds desktop-entry lookup and authorization before a one-shot native/XWayland activation token correlates the resulting client event. |

## Protocol and lifecycle boundaries

- Native Wayland advertises only globals whose implemented lifecycle is exercised:
  xdg-shell, xdg-decoration, xdg-activation, wlr-layer-shell,
  `ext-foreign-toplevel-list`, and `ext-workspace-v1` on top of the W2 surface,
  seat, SHM, and output foundation. The first W5 tranche additionally exercises
  `wp_viewporter` v1 and `wp_fractional_scale_manager_v1` v1 through rendering,
  exact preferred-scale delivery, duplicate-object rejection, and bounded
  hostile-client recovery. The next W5 tranche adds `wl_data_device_manager` v3
  and `zwp_primary_selection_device_manager_v1` v1 with byte-transfer,
  selection replacement/cancellation, exact cross-client owner-death cleanup,
  bounded source/device/MIME exhaustion, real-serial DND copy/drop/cancellation,
  DND icon rendering, and a native GTK4 startup smoke. The advanced-pointer
  tranche adds `zwp_relative_pointer_manager_v1` v1 and
  `zwp_pointer_constraints_v1` v1 with raw nested/direct deltas, lock,
  confinement, committed cursor hints, duplicate-constraint rejection, a
  cumulative 64-object client limit, and healthy-client recovery. The timing
  tranche adds `wp_presentation` v2 with monotonic submitted-frame feedback,
  output/refresh/sequence data, a cumulative 256-feedback limit, and
  healthy-client recovery. Shortcut inhibition adds
  `zwp_keyboard_shortcuts_inhibit_manager_v1` v1 with focus-scoped activation,
  key forwarding, destruction restoration, a cumulative 64-object limit, and
  healthy-client recovery. Pointer gestures add `zwp_pointer_gestures_v1` v3,
  direct libinput swipe/pinch/hold translation, all three bounded object
  classes, hostile-client isolation, and explicit hardware-only delivery
  acceptance rather than synthetic X gesture claims. Cursor shape adds
  `wp_cursor_shape_manager_v1` v2, focus-serial authorization, a compositor-
  rendered bounded glyph theme shared by every renderer, a cumulative
  64-device client limit, hostile-client isolation, and healthy-client
  recovery. Touch adds the `wl_touch` capability to `wl_seat` v9, forwards
  nested-winit and direct-libinput slot streams, enforces a cumulative
  16-device client limit, and explicitly leaves real event delivery to the
  guarded input-device hardware record instead of synthesizing touch from X
  pointer events. Tablet-v2 publishes `zwp_tablet_manager_v2` v1, forwards
  bounded direct-libinput tool axes/tip/button state and complete pad
  button/ring/strip mode groups, pairs split libinput nodes by device group,
  provides deterministic client-visible hot-unplug removal, and isolates
  tablet-seat exhaustion. Guarded physical event delivery remains open rather
  than being inferred from the nested object fixture.
  Conditional text input publishes `zwp_text_input_manager_v3` v1 only when a
  strict `[wayland].input_method` argv is configured. The compositor-launched
  process alone receives the filtered `zwp_input_method_manager_v2` v1 global
  over an inherited private socket; ordinary clients cannot claim it. Focused
  surrounding/content/cursor state, exact commit delivery, cumulative object
  exhaustion, IME death, child reaping, and healthy-client recovery are covered
  by the nested fixture.
  Idle lifecycle publishes `zwp_idle_inhibit_manager_v1` v1 and
  `ext_idle_notifier_v1` v2. Only buffered visible toplevel/layer surfaces
  suppress ordinary idle notifications; input-only notifications bypass
  inhibitors. Nested and direct input classes resume notifications and restart
  deadlines, weak protocol handles prevent disconnected clients from retaining
  compositor state, both object classes have cumulative 64-object limits, and
  the nested fixture proves exhaustion isolation, suppression, bypass,
  uninhibit restart, input resume, and a healthy shell afterward.
  Secure lock publishes `ext_session_lock_manager_v1` v1 with eight lock and
  sixteen lock-surface objects per connection. Lock acceptance immediately
  isolates ordinary focus, input, compositor UI, and rendering; confirmation
  waits for a secure frame on every output. Confirmed owner unlock restores the
  ordinary scene, while malformed early unlock or locker death retains a pure
  black locked state and refuses competitors. Nested coverage requires lock
  keyboard input, clean recovery, ordinary callback suppression, an X11 pixel
  proof of death fallback, invalid-unlock retention, and hostile exhaustion.
  Core resource hardening bounds concurrent SHM pools/buffers, frame callbacks,
  XDG positioners/popups, individual SHM allocation geometry, and outstanding
  configure queues. Hostile fixtures cover every boundary, while native GTK,
  Qt, SDL, Chromium/Ozone, and text-input acceptance cover the W5 toolkit exit.
  W6 adds wlr foreign-toplevel-management v3 with 16 manager bindings per
  client. The independent panel binds layer-shell v4, standard foreign-list v1,
  workspace-manager v1, and wlr management v3. Its nested fixture proves actual
  pointer-driven current/all task filtering and controls, workspace switching,
  launchers, drawable readiness, replacement, failure retention, and recovery.
- W4 owns real outputs, scale, DRM/KMS, DMA-BUF, and direct-seat lifecycle. W5
  owns data transfer, advanced input, presentation/scale protocols, idle, and
  session lock. W7 owns every XWayland-only row above. W6 owns the native panel
  rows except the explicitly unavailable urgency signal. W8
  owns agent rows and `theme.agent_marker`.
- Unknown or invalid config remains a strict parse error. A supported action is
  never accepted and then discarded merely because the active client lacks the
  required capability; it instead follows the same capability check as X11.
