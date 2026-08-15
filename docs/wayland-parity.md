# Wayland backend parity matrix

This matrix is the W3 accounting record for the public `nobox-config` model.
It answers whether each existing option has a native Wayland meaning; it does
not claim that later direct-display, panel, XWayland, or Agent Seat milestones
are complete.

The status terms are deliberately strict:

- **native**: implemented for native Wayland clients through neutral policy and
  Wayland compositor mechanics.
- **XWayland-only**: meaningful only for an X11 client once W7 supplies the
  XWayland/XWM boundary.
- **documented fallback**: accepted and deterministic, but the current
  single-output or external-command behavior is narrower than the final
  Wayland session.
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
| documented fallback | `move_resize_to`, `move_to_center` when `output` is `primary`, `pointer`, `next`, `previous`, `all`, or index 1 | W3 has one synthetic output, so each selector resolves to that output. A missing numeric output is diagnosed and ignored. W4 supplies full topology semantics without changing the action model. |

No action is XWayland-only or intentionally unsupported. XWayland clients will
enter these same actions through W7 instead of gaining a second executor.

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
| `theme.agent_marker` | intentionally unsupported | Reserved for the protected Agent Seat overlay delivered by W8; ordinary Wayland UI never impersonates it. |
| `mouse`: `inherit_defaults`, `disabled_bindings`, `modifier`, `compatibility_modifiers`, `move_button`, `resize_button`, `snap_to_windows`, `edge_resistance`, `drag_threshold`, `double_click_ms`, `bindings` | native | The effective typed binding set handles exact context/modifiers and press, release, click, double-click, drag, and wheel triggers. |
| `keyboard`: `inherit_defaults`, `disabled_bindings`, `chain_quit_key`, `chain_timeout_ms`, `bindings` | native | Handles complete sequences, prefix timeout/cancel, press/release interception, and ordered actions. |
| application match: `name`, `class` | native | Both match the bounded `xdg_toplevel.app_id`, preserving the existing neutral matcher without importing Wayland objects into config/core. |
| application match: `title`, `kind` | native | Matches bounded title and native normal/dialog role classification. |
| application match: `group_name`, `group_class`, `role` | XWayland-only | Native xdg-shell has no ICCCM group or `WM_WINDOW_ROLE` equivalent. W7 populates them only for XWayland clients; a native client therefore cannot match a rule that requires one. |
| application settings: `workspace`, `layer`, `decorated`, `focus`, `minimized`/`iconic`, `shaded`, `skip_pager`, `skip_taskbar`, `fullscreen`, `maximized`, `position`, `size` | native | Applied on initial management, with session restoration taking final precedence. Position/size output selectors use the W3 single-output fallback described above. |
| application setting `agent_visibility` | intentionally unsupported | Parsed and preserved, but no Wayland Agent Seat is advertised until W8 can enforce capture/input/redaction and protected indicators. |
| `panel`: `enabled`, `position`, `height`, `background`, `foreground`, `active_background`, `urgent_background`, `padding`, `spacing`, `task_max_width`, `task_scope`, `items`, `launchers`, `clock_format`, `show_workspaces`, `show_tasks`, `show_clock` | intentionally unsupported | The separate X11 EWMH panel is not started in a Wayland session. `panel.enabled = true` produces an explicit warning. W6 implements the separate layer-shell client and readiness handoff; the compositor does not absorb these settings. |
| `agent`: `enabled`, `socket`, `policy`, `grants`, `suppression_ms`, `kill_chord` | intentionally unsupported | W8 owns the Wayland seat, authorization, consent, preemption, and protected UI. No socket or capability is advertised during W3. |
| `agent.grants`: `label`, `executable`, `uid`, `capabilities`, `scope` | intentionally unsupported | Stored and validated by the shared config model, but inactive until W8. Scope retains the same matcher caveats listed above. |
| `agent.launch`: `policy`, `allow`, `deny`, `user_entries` | intentionally unsupported | Stored and validated, but no Wayland agent launch authority exists until W8. |

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
  acceptance rather than synthetic X gesture claims.
- W4 owns real outputs, scale, DRM/KMS, DMA-BUF, and direct-seat lifecycle. W5
  owns data transfer, advanced input, presentation/scale protocols, idle, and
  session lock. W7 owns every XWayland-only row above. W6 owns panel rows. W8
  owns agent rows and `theme.agent_marker`.
- Unknown or invalid config remains a strict parse error. A supported action is
  never accepted and then discarded merely because the active client lacks the
  required capability; it instead follows the same capability check as X11.
