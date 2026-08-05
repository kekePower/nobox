# X11 behavior reference

How nobox implements the ICCCM and EWMH contracts that applications, pagers,
and taskbars rely on. This is the detailed companion to the acceptance scope
in [x11-acceptance.md](x11-acceptance.md); the per-fixture inventory lives in
[openbox-compatibility.md](openbox-compatibility.md).

## Focus and activation

Focus assignment respects the ICCCM `WM_HINTS` input model and
`WM_TAKE_FOCUS` protocol. Modal transients, including ICCCM window groups,
receive focus and are raised when an application tries to activate a blocked
parent or group member. Read-only EWMH focused state tracks the decorations
across direct focus, Alt+Tab, minimization, and workspace changes.

## Geometry and size constraints

Client-requested and Super+right-drag resizing honor ICCCM minimum/maximum
sizes, base sizes, and resize increments. Client resize requests also
preserve the anchor described by window gravity. Framed clients retain
content-root geometry across configure requests. Pager moveresize requests
share ordinary client geometry handling, including field masks, gravity
anchoring, size constraints, and synthetic configure notifications.

Clients with their own titlebars or resize grips can delegate pointer or
keyboard interaction through EWMH `_NET_WM_MOVERESIZE`. Nobox retains bounded
pointer and keyboard grabs, applies the same work-area resistance and
size-hint constraints as native frame drags, commits on the initiating button
release or Enter, and restores the exact starting geometry on Escape or an
explicit cancel request.

Interactive resizes use EWMH `_NET_WM_SYNC_REQUEST` pacing when a client opts
in and the X Sync extension is available. Nobox initializes the advertised
counter, sends each sequence before its configure, and keeps only the latest
motion while waiting for the client to repaint. A one-second missed
acknowledgement disables pacing for that drag so an unresponsive client
cannot freeze the user's resize; clients and servers without the protocol
keep the direct path.

## Transient families

Specific transient families move between workspaces together, inherit higher
parent layers, and remain stacked above their parents even after restacking
or relationship changes.

## Iconic state and minimization

ICCCM iconic initial state and `WM_CHANGE_STATE` requests keep clients
managed while unmapped; activating an iconified client restores it normally.
Genuine minimization publishes EWMH hidden state, while off-workspace windows
are not misreported as minimized. The minimize button uses the same ICCCM
iconic/restore lifecycle as client requests.

## Stacking

Client and pager restacking requests support all X11 stack modes while
keeping the EWMH stacking list synchronized with the server's actual order.
EWMH above/below requests are mutually exclusive and remain within the core's
deterministic desktop/below/normal/dock/above/fullscreen stacking model.

## Frames, decorations, and properties

Framed clients publish `_NET_FRAME_EXTENTS` and are protected by the X save
set if nobox terminates. EWMH window types and Motif hints select per-client
roles, capabilities, and decorations; live hint changes update frames without
remanaging the client, and pre-map `_NET_REQUEST_FRAME_EXTENTS` estimates use
the same policy. UTF-8 and legacy X11 titles are mirrored onto frames and
refresh live. Client `_NET_WM_WINDOW_OPACITY` is likewise mirrored onto the
reparenting frame initially and on every change or deletion, so an external
compositor observes the intended top-level opacity. The EWMH support window
publishes nobox's PID. (For how GTK applications decide to draw their own
decorations, see [client-side-decorations.md](client-side-decorations.md).)

## Maximize, shading, and fullscreen

Initial and runtime EWMH maximize requests support independent axes and
preserve exact restore geometry; the maximize button toggles both axes
together. Initial and runtime EWMH shading keeps the titlebar active while
client content is unmapped, preserves geometry changes made while shaded, and
unshades before fullscreen. Client menus expose shade only when the current
frame supports it.

Fullscreen clients cover the complete output without decorations, stay above
docks, reject application geometry churn, and restore maximized or normal
geometry exactly. Validated EWMH fullscreen-monitor requests can span
selected output edges.

Legacy clients that exactly cover the root or one output without decorations
receive Openbox-compatible conditional fullscreen stacking without being
misreported as EWMH fullscreen. Their geometry remains client-controlled:
resizing or managed maximization leaves compatibility coverage immediately,
and exact coverage can be re-entered without hidden restore state.

## Struts and work areas

Dock and panel struts update `_NET_WORKAREA` dynamically, reflow maximized
clients, and fall back from `_NET_WM_STRUT_PARTIAL` to legacy
`_NET_WM_STRUT`. Work areas are independent per workspace: sticky docks
reserve every workspace, while local docks affect only their assigned
workspace. Desktop and dock roles do not steal focus and occupy their default
EWMH layers.

## Show desktop

EWMH show-desktop mode keeps desktop and dock surfaces mapped while
temporarily hiding ordinary clients without changing their genuine minimized
state; pager or Super+D requests toggle the mode, and explicit client
activation restores it. The typed action defaults to Openbox's non-strict
behavior, so launching a new ordinary window also restores the workspace. Set
`strict = true` on that action when show-desktop must remain active across
new windows.

## Taskbar and pager integration

EWMH skip-taskbar and skip-pager hints are honored both initially and at
runtime. ICCCM urgency and EWMH demands-attention share the urgent theme
state; activation clears demands-attention while leaving the client-owned
ICCCM hint untouched. Taskbars and pagers receive live EWMH allowed actions
derived from the same core capabilities used by nobox. Fixed-size clients do
not advertise resize or maximize, and fullscreen clients temporarily expose
only meaningful actions.

## Close, ping, and kill

Pager close requests use normal ICCCM `WM_DELETE_WINDOW` negotiation and
policy checks. Clients advertising `_NET_WM_PING` are checked once after a
close request. A timeout marks the frame as "Not Responding" without killing
it; close the marked window again to explicitly force-disconnect it, or let a
late reply restore it normally. The typed `kill` action immediately
disconnects the X11 client without sending `WM_DELETE_WINDOW` and cleans up
any pending ping deadline.

## Shaped clients

Shaped clients retain both their visible bounding region and pointer input
region after reparenting. The X11 backend adds the configured titlebar to
those regions, tracks Shape notifications, and returns the frame to its
native rectangle when the client clears a custom shape. Ordinary rectangular
clients and servers without Shape keep the zero-overhead fallback.

## Outputs

RandR monitors are selected through shared output policy for placement,
maximize, fullscreen, per-monitor struts, and safe recovery after
disconnects. Servers without RandR retain a single-root fallback.
