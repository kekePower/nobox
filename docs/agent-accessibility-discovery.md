# Agent accessibility discovery boundary

Status: B5 implemented for the deliberately narrow X11 mapping accepted in
B4; B6 hardening is in progress. The neutral wire contract, server-verified
X-Resource PID acquisition, and sandboxed Rust root-correlation helper exist.
Semantic manager integration is non-blocking and revalidates helper results at
a fixed deadline. Bounded root projection, subtree paging, and constrained
search are implemented and advertised as MCP `client_semantic_root`,
`client_semantic_tree`, and `client_semantic_find`. A real Firefox-family
browser regression now completes the motivating semantic-video workflow.

The accessibility interface is for language-model consumers. Its eventual
public results must therefore be compact, typed, deterministic, and useful
without explanatory prose. This document remains maintainer-facing because it
defines the security argument behind those machine-native results.

## End result

For a local X11 client, the manager can either prove one accessible
application root belongs to that client or return one generic semantic-
unavailable result. It never guesses from a title, application name, traversal
order, focus, or nearest rectangle. A successful proof requires:

1. a local process identity supplied by the X server, not `_NET_WM_PID`;
2. the accessibility bus daemon's process identity for an application root;
3. either one exact screen rectangle or a complete one-X-client to
   one-accessible-root bijection with equal dimensions; and
4. a second authorization, sensitivity, identity, and generation check before
   anything derived from that root leaves the manager.

Missing extensions, remote clients, disabled accessibility, stale objects,
duplicate roots, timeouts, crashes, and ambiguous evidence all take the same
safe fallback: semantic observation is unavailable and the already-granted
capture path is unchanged.

## Goals

- Preserve the proven client-to-root mapping before exposing semantic data.
- Keep X11 resources, D-Bus names, object paths, and process IDs off the Agent
  Seat Protocol wire.
- Read no accessible text until a root has been uniquely correlated.
- Make missing, stale, duplicate, cross-process, and toolkit-specific behavior
  explicit and tested.
- Bound input, enumeration, calls, wall time, process count, and output.
- Preserve hidden-client and sensitive-client non-disclosure in result bytes,
  error class, and manager-controlled response timing.
- Leave the manager event loop, the base agent seat, and capture usable when
  every accessibility component is absent or broken.

## Non-goals

- Retrofitting an accessibility helper or semantic tool into wire revision 1.
- Treating accessibility support as universal or enabling it globally.
- Dumping a desktop-wide accessibility tree.
- Using `_NET_WM_PID`, application names, window titles, AT-SPI Application
  `Id`, `AccessibleId`, focus, or fuzzy geometry as identity.
- Returning a D-Bus name or object path as a node handle.
- Keeping a privileged accessibility index alive between requests.
- Reading names, descriptions, values, text, attributes, relations, or
  descendants during root discovery.
- Semantic mutation. Existing window-addressed input remains the action path.
- Falling back from an ambiguous semantic target to the nearest or first root.

## Evidence and correlation

AT-SPI has one application root and exposes direct children, roles, states,
screen-coordinate component extents, and an application-bus process ID. It
does not expose a portable X11 window ID. Application `Id` is registry-assigned
and explicitly not a useful cross-system identity. `AccessibleId` is
application-defined and has no global lookup contract. Relevant upstream
interfaces are [Accessible](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Accessible.html),
[Application](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Application.html),
and [Component](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Component.html).
libatspi's `get_process_id` is a convenience call to D-Bus
`GetConnectionUnixProcessID`; it is useful evidence despite being documented
primarily as a debugging API.

On X11, the manager must use X-Resource extension 1.2 `QueryClientIds` with
`LocalClientPID`. `_NET_WM_PID` is client-authored and is not evidence. If the
server lacks X-Resource 1.2, the connection is remote, or the server provides
no local PID, that client has no semantic mapping. This restriction is local
to the X11 backend; a future Wayland compositor already owns trustworthy
surface-to-client credentials and feeds the same neutral proof shape.

The manager builds a bounded set of kernel-verified equivalent processes. The
first implementation should require an exact X-Resource PID; a descendant is
eligible only after its live parent chain has been verified and included.
Browser renderer processes do not need to be added merely because they render
content: measured browser roots and their exposed descendants share the
application root's bus identity.

For one already-authorized client, correlation is:

1. Count every live managed X11 top-level owned by the verified process set,
   including hidden, redacted, and out-of-scope clients. This count is policy
   input and is never returned.
2. Send only the verified process set, at most two target rectangles (content
   and frame), and whether that complete count is one to a disposable helper.
3. Enumerate at most four desktops and 64 application roots. For unrelated
   roots, read only the D-Bus-verified PID and never descend.
4. For matching application PIDs, inspect at most 64 direct children. A
   candidate must be showing, visible, non-defunct, implement Component, and
   have role `DIALOG`, `FRAME`, `WINDOW`, or the Qt top-level role `FILLER`.
5. One exact screen rectangle is a match. More than one exact rectangle is
   ambiguous.
6. When no origin matches, ignore origin only if the manager counted exactly
   one X11 top-level, the helper found exactly one live accessible top-level,
   and its width and height exactly match. This is a bijection, not fuzzy
   geometry.
7. Zero candidates, incomplete enumeration, stale state, a size mismatch, or
   multiple positionless candidates is semantic-unavailable. No tie-breaker
   exists.

The prototype request is strict JSON:

```json
{"v":1,"pids":[1234],"rects":[{"x":20,"y":40,"width":900,"height":600}],"single_client":true}
```

The Python experiment's only successful output is
`{"v":1,"status":"matched"}`. After the production Rust helper proves that
match, its internal success adds one bounded root projection: portable role,
optional 512-byte name, stable states, content-relative bounds, and child
count. A strict optional projection request names a helper-internal root plus a
breadth-first offset, page size, and depth. It inspects at most 4,096 objects to
locate and traverse that root, descends at most 16 levels, and returns at most
128 nodes with portable role, bounded name, stable states, optional
content-relative bounds, and child count. The other bounded statuses are
`ambiguous`, `unavailable`, and `invalid`, with no root metadata. Candidate
identities and counts never leave the process; the manager converts the
projection to the neutral public wire.

A mutually exclusive strict search request scans the correlated root in the
same deterministic breadth-first order and within the same 4,096-node,
16-level, 128-result limits. It accepts a bounded case-insensitive name
substring, role OR-set, and state AND-set, with at least one filter required.
Only matches cross the helper pipe. The manager independently rechecks every
predicate, remaps helper identities, and stores the original query plus scan
offset behind an opaque continuation.

## Measured matrix

Measurements were taken on 2026-08-06 with AT-SPI 2.58.3 in disposable
session-D-Bus and Xvfb sessions. The nested fixture runs the applications under
nobox, not the user's live desktop.

| Family | Measurement | Correlation consequence |
| --- | --- | --- |
| GTK 4.20.4 | `gtk4-demo` exposed one `FRAME` with the application PID and correct `800x600` size. After nobox placed the X client at `(240,100)`, AT-SPI still reported origin `(0,0)`. | Exact origin safely fails; the complete one-to-one, equal-size fallback matches. |
| Qt 6.10.0 | A QWidget fixture exposed one direct application child as `FILLER`, not `FRAME`. The private-session bridge required `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`. | Direct application children may use the bounded `FILLER` exception; the same one-to-one proof matches. |
| Zen 1.21.10b / Firefox family | A fresh private profile exposed one `FRAME` owned by the main browser PID. A bounded 38-node sample through depth seven reported the same application-bus PID throughout, although the browser used content processes. In the production path, a local HTML video named `Nobox demo video` projected as two `GROUP` nodes: an unbounded container and one focusable `640x360` media node with content-relative bounds. | Map the root to the server-verified main PID; cross-process implementation is transparent at this boundary. Select the actionable media node by accessible name plus typed state and bounds; do not require a portable `VIDEO` role. |
| Google Chrome 151 / Chromium family | A fresh profile launched with `--force-renderer-accessibility` exposed no Chrome application root in this isolated session; only an unrelated portal root appeared. | Missing is normal and returns semantic-unavailable. No title or portal-root fallback is permitted. Chromium's own documentation confirms the force flag, but availability remains runtime-dependent. |
| Electron | No Electron runtime was installed in the measurement environment. Deterministic fixtures cover the expected Chromium-family process and duplicate-root shapes. | B5 remains runtime-gated. Before claiming Electron support, add a real Electron fixture; absence continues to fall back safely. |

Rust unit and process-boundary fixtures additionally cover exact mapping,
verified process families, stale origins, unrelated processes, unrelated
geometry, hidden and defunct roots, incomplete scans, duplicate exact roots, duplicate
positionless roots, strict bounds, and unknown-field rejection. The nested
GTK/Qt test runs both the experimental probe and production Rust helper, then
requests `client.semantic_root` and paged `client.semantic_tree` through the
live manager. It proves that a real AT-SPI bus, toolkit bridge, nested X server,
X-Resource owner proof, manager revalidation, neutral protocol reply, opaque
continuation, stale-tree rejection, and constrained root-role search agree on
the restricted mapping. The optional real-browser regression launches a
Firefox-family browser with a disposable profile, disables first-run UI,
searches a checked-in local video fixture by accessible name, selects the
unique focusable bounded media node, and derives its center point without
capture. Five consecutive semantic-only runs passed in the measured
environment. The hardened path additionally takes one grounded content capture
without inspecting its pixels, proves the semantic bounds and center lie in
the capture's declared content extent, and emits compact latency and payload
metrics. Three consecutive combined runs passed; one observed sample used 705
semantic JSON bytes versus 46,824 capture JSON bytes (34,994 PNG bytes), with
the fixed-deadline semantic root-plus-search taking 2,401 ms and capture taking
347 ms.

The live agent-seat regression replaces the helper beside a private copy of
the manager with a controllable process-boundary fixture. A nonzero exit,
truncated JSON, and stdout one byte beyond the 1 MiB cap all produce the same
fixed-deadline `semantic_unavailable` result; a subsequent fresh helper returns
one valid bounded root, proving the worker survives each failure. The same test
cancels pending semantic work on human input and companion disconnect, then
proves freeze and live grant revocation return their typed session decisions
while discarding the helper.

## Threat model

The protected data is any accessible name, description, value, text,
attribute, state, relation, or geometry belonging to a client the requesting
session cannot observe semantically. The attacker is a granted agent that can
repeat requests and an untrusted X11/AT-SPI client that can choose its own
titles, properties, roles, and accessible content.

The window manager is authoritative for session identity, grant, scope,
sensitivity, client generation, X11 ownership evidence, and reply timing. The
helper is trusted only to translate a bounded request; it has no grant
authority and its answer is never sufficient by itself. X-Resource local PID
and D-Bus connection PID are identity evidence. Client properties, accessible
strings, application IDs, object paths, roles, states, and geometry are
untrusted claims used only after the two process identities agree.

An out-of-scope root cannot be substituted because discovery never descends
into a different D-Bus PID, exact mapping requires target geometry, and the
positionless fallback requires a complete bijection on both sides. A malicious
client cannot acquire another local PID by writing `_NET_WM_PID`; that property
is ignored. Subtree traversal rechecks every visited object's D-Bus owner PID
against the verified process set and stops before projecting an object outside
it. Browser content that remains on the matched application's bus is part of
that application's accessible projection.

Hidden and nonexistent targets are rejected by the manager before helper
creation with the existing indistinguishable `no_such_client` result.
Redacted or otherwise semantically sensitive visible targets are rejected
before helper creation as `semantic_unavailable`. Discovery reads no strings
before a unique match; only the matched root's bounded name is then projected.
The helper emits no unmatched metadata. The manager maps ambiguous, missing,
over-limit, invalid, crashed, killed, and timed-out helper outcomes to that
same public `semantic_unavailable` code, pads release to the same event-loop
deadline, and revalidates the target after the helper exits. Thus opening an
out-of-scope client cannot add a name, count, alternate error, or early
response within either established privacy equivalence. System-wide resource
exhaustion is a denial-of-service condition; it never changes which root is
selected or causes a best-effort result.

## Capability and sensitivity

Accessibility is a new `observe.accessibility` grant, not an implication of
`observe.structure`, `observe.titles`, `capture.client`, or input. Consent must
say that it permits reading application-provided control labels, values,
states, relationships, and text for the granted application scope, including
content not visible in a screenshot. Password/protected text and clients
classified sensitive remain redacted regardless of this capability.

Authorization happens before discovery. Authorization, scope, visibility,
sensitivity, process identity, and client/tree generation are checked again
after discovery and immediately before reply. Revocation or sensitivity change
while the helper runs cancels it and discards every byte.

## Lifecycle, isolation, and failure

The production Rust helper is optional and is spawned for one authorized
semantic request. Discovery and the bounded projection occur in
that same process so a D-Bus path never needs to cross the neutral wire. There
is no persistent desktop-wide cache. The first manager integration permits one
helper globally; concurrent excess work receives the same unavailable result
at the same fixed deadline.

The landed helper accepts at most 16 KiB of strict JSON, bounds PID, rectangle,
application-root, direct-child, and subtree enumeration, gives every bus call
150 ms, caps projected names at 512 bytes and output at 1 MiB, and caps total
discovery at one second. One page inspects at most 4,096 nodes, descends at most
16 levels, and returns at most 128 nodes. It sets
no-new-privileges plus CPU,
address-space, output-file, descriptor, and core limits. Once its AT-SPI and
D-Bus connections exist, a seccomp allowlist denies opening files, creating or
connecting sockets, executing programs, cloning processes, and other
unneeded syscalls. It reads no accessible strings during correlation and uses
safe Rust. Its small `async-io` reactor is confined to this disposable process;
the manager and companion remain blocking/event-driven without an async
runtime.

The manager communicates over length-bounded pipes, uses an empty private
working directory, and clears the environment before supplying only the
session/accessibility bus addresses and runtime directory. The helper receives
no display variable. The manager owns a dedicated worker thread and never
waits for the process in the X11 event loop. It kills a helper still running
after 1.1 seconds, releases every ordinary helper outcome at 1.2 seconds, and
rechecks authorization, redaction, client generation, and X-Resource PID
before using a match. Concurrent snapshots remain serviceable while discovery
runs.

Each D-Bus call has a 150 ms timeout and total helper discovery is capped at
one second. The manager-owned hard and reply deadlines are later and fixed.
Human preemption, revocation, session disconnect, target loss, generation
change, and manager shutdown terminate the helper and discard output. Direct
`SIGKILL` is acceptable because the process owns no durable state.

A helper crash, malformed JSON, extra stdout, nonzero exit, timeout, missing
runtime, inaccessible bus, or sandbox denial disables only that request's
semantic observation. It cannot stall the event loop, revoke unrelated
capabilities, or affect snapshot, management, input, launch, or capture.

## B5 gate

B5 is complete only for the restricted local-client mapping above. X-Resource
1.2 PID acquisition handles the server-returned client resource base and fails
closed where unavailable. The Python
experiment has been replaced at the production boundary by a sandboxed
optional Rust helper. Manager integration preserves fixed public timing,
post-helper revalidation, and the single unavailable result. Bounded root and
subtree projections are advertised as `client_semantic_root` and
`client_semantic_tree`; constrained search is advertised as
`client_semantic_find`. Helper
identities are collision-checked, remapped to manager-issued
session/client/tree-local handles, and never returned raw. Real
Chromium/Electron coverage remains required before advertising those families
as tested; their absence remains semantic-unavailable. No semantic tool may
ship with title matching, `_NET_WM_PID`, fuzzy
geometry, traversal-order selection, or a returned raw AT-SPI identifier.
