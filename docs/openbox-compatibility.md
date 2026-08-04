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
| `big` | Oversized clients remain valid and movable | Policy | Geometry clamping tests; add nested-X oversized placement case |
| `borderchange` | Client border-width requests do not corrupt framed geometry | Equivalent | `x11-edge-compat` live `CWBorderWidth` regression |
| `confignotify` | Exact synthetic `ConfigureNotify` coordinates and gravity | Equivalent | Pager/client geometry regressions; retain an exact event-stream follow-up |
| `confignotifymax` | Initial maximize geometry and notifications | Direct | `openbox-regressions` |
| `cursorio` | Input-only child cursor behavior survives reparenting | Equivalent | `x11-input-cursor` verifies parentage and the server-selected XFixes cursor image |
| `duplicatesession` | Duplicate session IDs restore deterministically | Deferred | Session persistence is not implemented yet |
| `extentsrequest` | Pre-map frame-extents estimates reflect policy | Direct | `openbox-regressions` |
| `fakeunmap` | Synthetic and real unmaps are distinguished | Direct | `openbox-regressions` |
| `fallback` | Focus recovers when a transient family vanishes | Equivalent | Modal focus and client-loss regressions |
| `focusout` | Focus transfers reconcile across child trees while inferior/grab noise is ignored | Equivalent | `x11-focus-events` child, ancestor/inferior, grab/ungrab, and root-focus regressions |
| `fullscreen` | EWMH fullscreen entry and exit | Equivalent | `openbox-regressions` state/geometry checks |
| `grav` | ICCCM window gravity preserves anchors | Direct | `openbox-regressions` |
| `groupmodal` | Group-modal focus redirection | Direct | `openbox-regressions` |
| `grouptran`, `grouptran2` | Group transient relationships | Policy | Core relationship, stacking, and workspace-family tests |
| `grouptrancircular`, `grouptrancircular2` | Circular group/transient hints terminate safely | Policy | Cycle-safe core traversal tests |
| `hideshow.py`, `showhide` | Rapid map/unmap/destroy lifecycle | Equivalent | Smoke, fake-unmap, and client-loss regressions; GTK 2 fixture itself is obsolete |
| `iconifydelay` | Iconify/map races do not withdraw a client | Equivalent | Iconic lifecycle and restore regressions |
| `icons` | `_NET_WM_ICON` parsing, bounds, and live replacement | Equivalent | Bounded parser unit tests and `x11-icons` live replacement regression |
| `mapiconic` | ICCCM iconic initial state | Direct | `openbox-regressions` |
| `mingrow`, `resize` | Minimum/base/increment resize constraints | Policy | Core size-hint and interactive-resize tests |
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
The remaining inventory is session restore. `_NET_WM_PING` is covered by
`x11-ping`: responsive and late clients remain connected, stale deadlines are
harmless, and only a repeated close after a verified timeout disconnects a hung
client. Each remaining item needs a
compatibility decision before X11 can be called feature complete; unsupported
behavior must be documented rather than omitted silently.
