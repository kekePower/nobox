# Openbox compatibility matrix

This matrix records the behavioral intent of every fixture currently present in
`../openbox/tests`. It is an inventory, not a claim that compiling an old test
is sufficient: nobox tests assert observable contracts and may use smaller,
deterministic clients when an upstream fixture relies on sleeps or manual
inspection.

Status meanings:

- **Direct**: the upstream fixture runs in the automated nested-X gate.
- **Equivalent**: a deterministic nobox integration exercises the same contract.
- **Policy**: the protocol-neutral invariant has unit coverage, but the original
  X11 fixture is not yet in the nested-X gate.
- **Pending**: the behavior still needs an explicit implementation or test.
- **Deferred**: intentionally belongs to a later subsystem rather than the
  current window-management slice.

| Openbox fixture | Behavioral contract | Status | Nobox evidence or next work |
|---|---|---:|---|
| `aspect` | ICCCM aspect constraints | Direct | `openbox-regressions` |
| `big` | Oversized clients remain valid and movable | Direct | `x11-size-compat` preserves the upstream 2000x2000 client while moving it on an 800x600 root |
| `borderchange` | Client border-width requests do not corrupt framed geometry | Equivalent | `x11-edge-compat` live `CWBorderWidth` regression |
| `confignotify` | Exact synthetic `ConfigureNotify` coordinates and gravity | Direct | `x11-configure-notify` observes root-relative move/resize, southeast-gravity anchoring, maximized request denial, border width, sibling, and send-event fields from the client event stream |
| `confignotifymax` | Initial maximize geometry and notifications | Direct | `openbox-regressions` plus exact maximized synthetic events in `x11-configure-notify` |
| `cursorio` | Input-only child cursor behavior survives reparenting | Equivalent | `x11-input-cursor` verifies parentage and the server-selected XFixes cursor image |
| `duplicatesession` | Duplicate session IDs restore deterministically | Equivalent | Duplicate candidates are discarded by bounded unit policy and `x11-session-restore` verifies that ambiguous live clients retain their current state |
| `extentsrequest` | Pre-map frame-extents estimates reflect policy | Direct | `openbox-regressions` |
| `fakeunmap` | Synthetic and real unmaps are distinguished | Direct | `openbox-regressions` |
| `fallback` | Focus recovers when a transient family vanishes | Equivalent | Modal focus and client-loss regressions |
| `focusout` | Focus transfers reconcile across child trees while inferior/grab noise is ignored | Equivalent | `x11-focus-events` child, ancestor/inferior, grab/ungrab, and root-focus regressions |
| `fullscreen` | EWMH fullscreen entry and exit | Equivalent | `openbox-regressions` state/geometry checks |
| `grav` | ICCCM window gravity preserves anchors | Direct | `openbox-regressions` |
| `groupmodal` | Group-modal focus redirection | Direct | `openbox-regressions` |
| `grouptran`, `grouptran2` | Group transient relationships | Direct | `x11-group-transients` preserves helper/child stacking and workspace families after adversarial raises |
| `grouptrancircular`, `grouptrancircular2` | Circular group/transient hints terminate safely | Direct | `x11-group-transients` verifies sibling independence, specific descendants, and live-manager survival |
| `hideshow.py`, `showhide` | Rapid map/unmap/destroy lifecycle | Equivalent | Smoke, fake-unmap, and client-loss regressions; GTK 2 fixture itself is obsolete |
| `iconifydelay` | Iconify/map races do not withdraw a client | Equivalent | Iconic lifecycle and restore regressions |
| `icons` | `_NET_WM_ICON` parsing, bounds, and live replacement | Equivalent | Bounded parser unit tests and `x11-icons` live replacement regression |
| `mapiconic` | ICCCM iconic initial state | Direct | `openbox-regressions` |
| `mingrow`, `resize` | Minimum/base/increment resize constraints | Direct | `x11-size-compat` verifies live invalid-limit normalization, constrained requests, and exact later client resizing |
| `modal`, `modal2` | Specific modal focus redirection | Direct | `openbox-regressions` |
| `modal3` | Live modal-state relationship changes | Equivalent | `x11-edge-compat` add/remove focus redirection |
| `noresize` | Fixed-size clients lose resize/maximize operations | Equivalent | Allowed-actions and presentation regressions |
| `oldfullscreen` | Legacy undecorated root/output-sized coverage gets conditional fullscreen stacking | Direct | `x11-legacy-fullscreen` drives the upstream client through focus, Above, maximize, and client-resize transitions without synthesizing EWMH fullscreen |
| `override`, `overrideinputonly` | Override-redirect and input-only windows stay unmanaged | Equivalent | `x11-edge-compat` client-list, frame, and parent assertions |
| `positioned` | ICCCM program/user positions bypass smart placement | Equivalent | `x11-placement` |
| `restack`, `stackabove` | Client and pager sibling restacks preserve policy layers | Equivalent | Restack and stacking regressions |
| `shape` | Bounding and input shapes survive framing and live changes | Direct | `x11-shape` using the upstream client |
| `skiptaskbar`, `skiptaskbar2` | Initial and runtime taskbar exclusion | Equivalent | `x11-presentation` plus menu/switcher filtering |
| `stacking` | Group/specific-transient stacking order | Direct | `openbox-regressions` |
| `strut` | Live legacy strut reservation changes | Equivalent | Partial/legacy, workspace, and output-aware strut regressions |
| `title` | Legacy/UTF-8 title import and live refresh | Direct | `openbox-regressions` |
| `urgent` | ICCCM urgency changes presentation without stealing ownership | Equivalent | `x11-presentation` |
| `usertimewin` | Auxiliary user-time windows affect activation policy | Equivalent | `x11-focus-stealing` |
| `wmhints` | Live Motif decoration changes preserve geometry | Equivalent | Dynamic decoration regression in `openbox-regressions` |

## Gaps outside the historical fixture directory

The old fixtures do not cover the whole modern X11 contract. ICCCM colormap
windows are covered by `x11-colormaps`, including ordered installation, implicit
top-level priority, live property and `ColormapNotify` changes, hostile bounded
input, focus switching, and default restoration. `_NET_WM_SYNC_REQUEST` is
covered by `x11-sync-resize`: the manager initializes opted-in counters,
responsive clients pace subsequent geometry, and stalled clients fall back
without freezing the drag. ICCCM manager-selection conversions, replacement
ordering, and `PRIMARY`/`CLIPBOARD` coexistence are covered by `x11-selections`.
Local session persistence is covered by `x11-session-restore`, including clean
WM restart, stable identity matching, restored geometry/state/focus, and
duplicate rejection. Client-owned move/resize grips are covered by
`x11-net-moveresize`, including pointer and keyboard interaction, capability
rejection, commit, and both cancellation forms. The same regression invokes
configured `Move` and `Resize` actions from keyboard bindings, while
`x11-mouse-bindings` verifies that an explicit pointer resize edge overrides
hit-test inference and preserves its opposite anchor under ICCCM size
increments. `_NET_WM_PING` is covered by
`x11-ping`: responsive and late clients remain connected, stale deadlines are
harmless, and only a repeated close after a verified timeout disconnects a hung
client. The same regression verifies that the explicit `Kill` action bypasses
`WM_DELETE_WINDOW`, cancels protocol tracking, and disconnects only its target
connection. Openbox-style configured relative movement and edge resizing are
covered by `x11-geometry-actions`, including pixel, percentage, and fraction
resolution, on-screen movement, constrained edge anchoring, repeated `MoveToEdge` steps,
directional grow/shrink, blocked-growth fallback, and two-pass fill around live
client obstacles. The same test covers `MoveResizeTo` fractional sizing,
gravity-style end anchoring, centering, and fixed-size capability enforcement;
unit policy covers deterministic cross-output preservation and selector wrapping.
Openbox-style `DirectionalTargetWindow` behavior is covered by
`x11-focus-cycle`: real clients are selected spatially, automatically unshaded,
and raised independently of the general focus policy; core tests cover all
eight directions, cone priority, and stable MRU tie-breaking.
The same regression covers `DirectionalCycleWindows` preview, retained-grab
overlay, modifier-release commit, Escape focus rollback, and cancellation that
does not unshade the rejected preview target.
Openbox's richer `Execute` contract is covered by `x11-execute`: a grabbed
confirmation must prevent premature launch; `$pid`, `$wid`, and `$pointer`
expand from the original action context; the child receives a unique
`DESKTOP_STARTUP_ID`; and its matching window inherits the launch workspace.
Unit coverage bounds and parses `_NET_STARTUP_INFO_BEGIN`/`_NET_STARTUP_INFO`,
including quoted values, malformed input, timeouts, and failed launches.
It also covers `FocusToBottom`, `Unfocus`, and `FocusFallback` through real MRU
selection, alias-equivalent fallback, shaded-client exclusion, and deletion of
`_NET_ACTIVE_WINDOW` when no valid fallback remains.
`x11-conditional-actions` additionally exercises `Focus` as activation rather
than a visibility-limited primitive: the default follows a target to its
workspace, `here = true` brings its transient family to the active workspace,
and an iconic target is restored before receiving focus.
`x11-stacking-actions` covers `RaiseLower` overlap-driven raise/lower/no-op and
both conditional branches of the legacy `ShadeLower` and `UnshadeRaise`
composites against server-observed stacking and EWMH shade state.
`x11-explicit-state-actions` covers Openbox's idempotent `Maximize`/
`Unmaximize`, `Decorate`/`Undecorate`, `Shade`/`Unshade`, and `SendToLayer`
contracts. It verifies independent maximize axes, exact repeat behavior,
decoration/shade interlock, and mutually exclusive above/below EWMH state.
`x11-workspace-actions` covers `DesktopLast`, `SendToDesktopLast`, and runtime
add/remove-at-current/last behavior through server-observed desktop names,
counts, client assignments, visibility, and focus, including the one-workspace
lower bound.
`x11-restart` covers Openbox's `Restart` contract for both self-restart and an
optional replacement command. It verifies live-client adoption and restored
workspace/layer/focus state, single-run autostart, persisted state, authoritative
manager-selection release, and synchronous EWMH root-property cleanup.
`x11-conditional-actions` covers Openbox `If`, `ForEach`, and `Stop` semantics:
action/focus target queries, state/workspace/output/application predicates,
then/else/none branches, live title refresh, stable management order, consumed
loop termination, and propagation that stops the surrounding action list.
The same nested-X test verifies that the typed Openbox `Debug` equivalent
reaches bounded structured logging.
`x11-applications` covers ordered Openbox-style initial application policy with
client and group identity matching. Real X11 clients verify sticky and numbered
workspace assignment, focus/layer/decorations, minimized/shaded/maximized/
fullscreen and task-list/pager states, relative sizing and absolute placement,
plus preservation and explicit override of ICCCM position hints. Saved session
state remains the later authority by design.
`x11-opacity` verifies Openbox-compatible `_NET_WM_WINDOW_OPACITY` propagation
from reparented clients to compositor-visible frames across initial publication,
live replacement, and deletion. It also checks the advertised EWMH support
window PID.
`x11-margins` verifies configured initial-workspace selection and Openbox-style
outer-screen margins through exact multi-workspace `_NET_WORKAREA` values,
maximized frame geometry, live reload, and maximized-client reflow.
Openbox pipe-menu workflows are covered by `x11-menus` through command-backed
definitions that regenerate on open. The generator has a 50--5000 ms deadline,
64 KiB UTF-8 output cap, private temporary output, and the same strict typed
entry, action, submenu, cycle, and resource validation as persisted menus;
failure leaves no partial popup or source-tree artifact.
The same test covers the default native applications menu using isolated XDG
desktop entries. Discovery honors precedence and visibility, groups entries by
stable FreeDesktop categories, and launches parsed argv directly; a hostile
shell metacharacter fixture proves desktop `Exec` content is not evaluated as a
shell command.
Native XSMP coordination is covered by `x11-xsmp`: a real libSM test manager
verifies client-ID reconnection, required clone/restart/process/user properties,
durable in-place `SaveYourself`, save completion and cancellation delivery,
clean `Die`, restart-style change, connection close, and outbound global
interactive logout requests. The same test drives `SessionLogout` through its
default grabbed confirmation and prompt-free form, proves cancellation retains
X11 ownership, and verifies the clean local-exit fallback without XSMP. The
optional companion does not turn nobox into a desktop session manager;
application clients and the external manager retain responsibility for
application relaunch.
`x11-exit` covers default confirmation, cancellation, confirmed local manager
exit, and the explicit prompt-free form. Unlike Openbox's historical aliases,
nobox intentionally keeps local `Exit` distinct from session-aware
`SessionLogout`, so users can release the window manager without requesting
that the surrounding desktop session end.
`x11-show-desktop` covers Openbox's default non-strict launch restoration and
the explicit strict form, along with EWMH requests, activation restoration,
desktop/dock visibility, and preservation of genuine iconic state.
Graphical configuration is covered outside the historical fixture set by the
`nobox-settings` model tests and `x11-settings`. Friendly controls retain
comments and complex bindings, invalid advanced source cannot replace the last
valid file, and the optional GTK/libadwaita process maps and atomically saves on
a nested X server without introducing toolkit linkage into `nobox` itself.

## Intentional action API boundaries

Nobox preserves Openbox's default user-visible action contracts without
transcribing every XML-era presentation switch into each binding. Linear and
spatial cycles use one MRU, current-workspace, ordinary-client policy; their
overlay presentation is configured once under `switcher`, and commit always
unshades, focuses, and raises. Openbox's per-invocation `linear`, `dialog`,
`bar`, `raise`, `panels`, `desktop`, `hilite`, `allDesktops`, and arbitrary
`finalactions` variants are therefore intentionally not a second configuration
surface. Cross-workspace selection remains available through the grouped window
list and typed `for_each` queries.

`ShowMenu` anchors to the invoking pointer or centers on the target output
instead of accepting arbitrary gravity coordinates. `Focus` always closes an
active cycle before deliberate activation rather than exposing Openbox's
`stopInteractive = false` state leak. These are bounded configuration and UX
decisions, not missing ICCCM/EWMH behavior. Pointer `Resize` does retain the
useful fixed `edge` option, and keyboard/menu invocations enter the same grabbed
keyboard moveresize mode as Openbox.

Openbox `RaiseDock`, `LowerDock`, and `ToggleDockAutoHide` operate on Openbox's
private dockapp container. Nobox intentionally has no such container: EWMH
panels are managed dock-role clients and retain ownership of their own
visibility. Bulk panel policy is expressible honestly with `for_each` and a
`kind = "dock"` query. `BreakChroot` is also not exposed because nobox's finite,
timeout-bounded key sequences do not enter persistent keyboard chroots.
