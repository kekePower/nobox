# Agent accessibility discovery boundary

Status: B4 proof accepted for a deliberately narrow X11 mapping. Semantic
tools remain unimplemented; this document is the gate they must satisfy.

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

- Prove a client-to-root mapping before defining the public semantic schema.
- Keep X11 resources, D-Bus names, object paths, and process IDs off the Agent
  Seat Protocol wire.
- Read no accessible text while discovering a root.
- Make missing, stale, duplicate, cross-process, and toolkit-specific behavior
  explicit and tested.
- Bound input, enumeration, calls, wall time, process count, and output.
- Preserve hidden-client and sensitive-client non-disclosure in result bytes,
  error class, and manager-controlled response timing.
- Leave the manager event loop, the base agent seat, and capture usable when
  every accessibility component is absent or broken.

## Non-goals

- Shipping an accessibility helper or MCP semantic tool in B4.
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

Its only successful output is `{"v":1,"status":"matched"}`. The other
bounded statuses are `ambiguous`, `unavailable`, and `invalid`. Candidate
identities and counts never leave the process. This is an internal experiment,
not the future public wire shape.

## Measured matrix

Measurements were taken on 2026-08-06 with AT-SPI 2.58.3 in disposable
session-D-Bus and Xvfb sessions. The nested fixture runs the applications under
nobox, not the user's live desktop.

| Family | Measurement | Correlation consequence |
| --- | --- | --- |
| GTK 4.20.4 | `gtk4-demo` exposed one `FRAME` with the application PID and correct `800x600` size. After nobox placed the X client at `(240,100)`, AT-SPI still reported origin `(0,0)`. | Exact origin safely fails; the complete one-to-one, equal-size fallback matches. |
| Qt 6.10.0 | A QWidget fixture exposed one direct application child as `FILLER`, not `FRAME`. The private-session bridge required `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`. | Direct application children may use the bounded `FILLER` exception; the same one-to-one proof matches. |
| Zen 1.21.10b / Firefox family | A fresh private profile exposed one `FRAME` owned by the main browser PID. A bounded 38-node sample through depth seven reported the same application-bus PID throughout, although the browser used content processes. | Map the root to the server-verified main PID; cross-process implementation is transparent at this boundary. Re-check process identity at embed boundaries during B5. |
| Google Chrome 151 / Chromium family | A fresh profile launched with `--force-renderer-accessibility` exposed no Chrome application root in this isolated session; only an unrelated portal root appeared. | Missing is normal and returns semantic-unavailable. No title or portal-root fallback is permitted. Chromium's own documentation confirms the force flag, but availability remains runtime-dependent. |
| Electron | No Electron runtime was installed in the measurement environment. Deterministic fixtures cover the expected Chromium-family process and duplicate-root shapes. | B5 remains runtime-gated. Before claiming Electron support, add a real Electron fixture; absence continues to fall back safely. |

Deterministic fixtures additionally cover exact mapping, verified process
families, stale origins, unrelated processes, unrelated geometry, hidden and
defunct roots, incomplete scans, duplicate exact roots, duplicate
positionless roots, strict bounds, and unknown-field rejection. The nested
GTK/Qt test proves that a real AT-SPI bus, real toolkit bridges, a nested X
server, and nobox agree on the restricted mapping.

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
is ignored. When B5 traverses an embedded subtree, it must stop before any
object whose D-Bus PID is outside the verified process set. Browser content
that remains on the matched application's bus is part of that application's
accessible projection.

Hidden, redacted, sensitive, stale, and nonexistent targets are rejected by
the manager before helper creation with the same public result. Discovery
reads no strings. The helper emits no unmatched metadata. The manager maps
ambiguous, missing, over-limit, invalid, crashed, killed, and timed-out helper
outcomes to the same public `semantic_unavailable` code, pads release to the
same event-loop deadline, and revalidates the target after the helper exits.
Thus opening an out-of-scope client cannot add a name, count, alternate error,
or early response to an authorized query. System-wide resource exhaustion is
a denial-of-service condition; it never changes which root is selected or
causes a best-effort result.

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

The production helper is optional and spawned for one authorized semantic
request. Discovery and the bounded projection occur in that same process so a
D-Bus path never needs to cross the neutral wire. There is no persistent
desktop-wide cache. The manager permits one helper per session and a small
global fixed maximum; excess work receives the same unavailable result.

The manager communicates over length-bounded pipes, closes unrelated file
descriptors, supplies only the accessibility bus endpoint and one request,
uses an empty private working directory, sanitizes the environment, and sets
no-new-privileges plus strict address-space, CPU, output-file, descriptor, and
process limits. The production Rust helper must add a syscall allowlist and a
filesystem/network sandbox after its required libraries and inherited D-Bus
connection are open. It receives no X11 connection. Its code contains no
unsafe Rust.

Each D-Bus call has a 150 ms prototype timeout. The prototype result slot is
one second and the manager-owned hard deadline is later but fixed. The parent
must terminate the process at the hard deadline; an in-process timeout cannot
reliably interrupt a blocked foreign-library call. Human preemption,
revocation, session disconnect, target loss, generation change, and manager
shutdown terminate the helper and discard output. `SIGKILL` after a short
`SIGTERM` grace is acceptable because the process owns no durable state.

A helper crash, malformed JSON, extra stdout, nonzero exit, timeout, missing
runtime, inaccessible bus, or sandbox denial disables only that request's
semantic observation. It cannot stall the event loop, revoke unrelated
capabilities, or affect snapshot, management, input, launch, or capture.

## B5 gate

B5 may proceed only for the restricted local-client mapping above. Its first
implementation must add X-Resource 1.2 PID acquisition and fail closed where
it is unavailable; replace the Python experiment with a sandboxed optional
Rust helper; preserve fixed public timing and the single unavailable result;
and add real Chromium/Electron coverage before advertising those families as
tested. No semantic tool may ship with title matching, `_NET_WM_PID`, fuzzy
geometry, traversal-order selection, or a returned raw AT-SPI identifier.
