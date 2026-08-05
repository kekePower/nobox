# Agent protocol

Status: implemented. The protocol is named **Agent Seat Protocol**
(`agent-seat`); its wire types live in the extraction-ready
`agent-seat-proto` crate, its policy in `nobox-core`, its X11 realization in
`nobox-x11`, and its MCP companion in `nobox-agent`. Everything below is
implemented and covered by `tests/x11-agent-seat.sh` and
`tests/x11-agent-mcp.sh`. Enforcement on X11 remains cooperative; see the
caveat at the end.

Nobox exposes structured desktop observation and control to AI agent
harnesses. The window manager already owns the facts agents currently
reconstruct from screenshots — window identity, geometry, focus, stacking,
workspaces — and already routes every input event. This protocol serves those
facts and accepts typed intent, with screenshots demoted from the medium of
interaction to one capability among several. The result is an agent seat
controlled by the window manager and subordinate to the human seat: bounded,
attributable actions against named objects instead of a synthetic human at a
global keyboard and mouse.

The design has two tiers. Tier 0 is a standalone mapping of EWMH onto agent
tools; it works on any compliant X11 window manager without cooperation and is
not nobox-specific. This document specifies Tier 1: the WM-integrated surface
covering what EWMH cannot express — pushed semantic events, atomic
orchestration, window-relative input, human/agent arbitration, and a consent
model. Nobox is the reference implementation. Integration is advertised the
traditional X11 way: the `_AGENT_SEAT` root property, a UTF-8 string of
protocol name, version, and socket path separated by nul bytes, so capability
discovery needs no side channel and no nobox-specific knowledge.

## Process shape and trust boundary

`nobox-agent` follows the `nobox-xsmp`/`nobox-panel` companion pattern: a
separate optional process, spawned per agent harness, speaking MCP on stdio
toward the agent and a typed, bounded protocol to the manager over a UNIX
socket. The companion is a translator, not an authority. It faces the agent,
so the WM treats it as exposed: every request is validated against the
session's grant inside the manager, regardless of any companion-side checks.

The companion targets MCP revision 2026-07-28, which is stateless: there is
no initialization handshake, every request carries its protocol version and
capabilities in `_meta`, and a stdio process is explicitly not a session. The
WM-side session is unaffected — it binds to the verified companion process
and its grant, not to any MCP lifecycle — and every cross-request reference
an agent holds (client identities, sequence numbers, generation counters) is
already an explicit identifier passed on each request, which is exactly the
state model stateless MCP requires. Agent-facing event delivery is therefore
cursor-based retrieval against the sequence stream, with the long-lived
`subscriptions/listen` stream as an optional additional delivery path; the
WM-side push protocol over the socket is unchanged. MCP's extension framework
is the candidate long-term home for this surface as a vendor-prefixed
extension once Tier 1 is proven.

Targeting that revision alone turned out to be a way of shipping nothing.
Hosts in the field open with `initialize`; a companion that refuses it is
reported as a broken server, and the user sees an installed, configured,
granted seat expose no tools and explain nothing. The companion therefore also
answers the handshake revisions, agreeing a version once and taking later
requests on that agreement. Both dialects reach the same tools and the same
seat, and the manager is untouched by the distinction: the WM-side session
never bound to an MCP lifecycle in the first place. A host that handshakes
never calls `server/discover`, so the model-facing instructions travel in the
`initialize` result as well.

The manager never blocks on the agent socket. Writes are bounded and
non-blocking; a slow, dead, or misbehaving companion is disconnected without
affecting window management. Companion failure never enters the manager's
failure boundary.

All identities in the protocol are `nobox-core` client, workspace, and output
identities. X11 window IDs and atoms never appear. This keeps the protocol
implementable unchanged by the future Wayland compositor backend, where it
becomes the only path in rather than a better alternative to X11's open
display access.

## Session model

A connection is a session with an identity and a grant, not a socket.

1. **Handshake.** The companion presents the agent's declared identity
   (harness name, purpose string). Declared strings are display-only and never
   an authorization input; nothing in the configuration schema can match on
   them. The manager separately records verified peer identity: UID/GID/PID
   from socket peer credentials, executable identity from `/proc` where
   available, and a best-effort parent-process chain. Each
   connection carries a manager-issued nonce; a session cannot be resumed or
   replayed. On X11 this verification is informative rather than a hard
   boundary — any same-user process can bypass the manager entirely — but it
   is specified now because persisted grants bind to it and the Wayland
   backend enforces it.
2. **Consent.** The manager checks `[agent]` configuration for a stored grant;
   otherwise it renders its own consent dialog showing the identity and
   requested capabilities, described in the terms they actually grant rather
   than in protocol vocabulary. It is keyboard-only and holds the keyboard
   while it is up; the session waits for a person to answer. The dialog is WM-drawn and cannot be created,
   covered, interacted with, or dismissed through the protocol. The answer may
   be one-shot or persisted; a persisted grant binds to the verified peer
   identity, never to the self-declared strings, so a truthful consent dialog
   cannot become a stored authorization reusable by an impostor.
3. **Grant.** A grant is a capability set plus an optional application scope.
   Capabilities are independent; none implies another:
   - `observe` — structured desktop state and events.
   - `capture` — per-window pixel access.
   - `input` — synthesized input, window-addressed.
   - `manage` — activation, geometry, state, workspace, close.
   - `launch` — starting applications from the desktop-entry catalog.

   A scope restricts the grant to clients matching application-rule identity
   (class, name, role, type), reusing `nobox-config` rule matching. Scope
   applies identically to snapshots, tool calls, and events: out-of-scope
   clients are absent, not merely inert.

   The five names are consent-presentation bundles over a finer internal
   capability model (`observe.structure`, `observe.titles`,
   `capture.client_visible`, `capture.client_obscured`, `capture.output`,
   `input.pointer`, `input.keyboard`, `manage.activate`, `manage.geometry`,
   `manage.state`, `manage.close`, `manage.workspace`, `launch.desktop`, …).
   Grants record the fine-grained atoms, so a future consent UI or policy can
   narrow a bundle — titles without capture, activate without geometry —
   without a protocol change, and granting `observe` is never silently
   redefined to include new privacy surfaces.
4. **Revocation.** Grants revoke live on configuration reload. A configured
   kill chord freezes every session immediately; it is processed in the
   manager's own input path ahead of all agent traffic, so it works under
   agent input flood. Freezing is distinct from revocation: the human decides
   afterward whether sessions resume or end.

Two standing indicators are WM-rendered, and the protocol offers no way to
draw, cover, target, or dismiss them: a persistent marker while any session
holds `input` or `capture`, and a frame highlight on the client currently
receiving agent input, drawn in a theme color reserved for the purpose. On X11 other same-user clients
remain able to imitate or obscure UI generally; under Wayland the claim
becomes system-level.

## Sensitive clients

Application rules gain `agent_visibility = "visible" | "redacted" |
"hidden"`. Redacted clients appear in snapshots with existence and geometry
but no title; capture and input against them fail. Hidden clients are absent
from every response and event, and acting on their identity returns the same
"no such client" error as a genuinely nonexistent one, so errors are not an
oracle for what is hidden.

Sensitivity only increases while a client is managed. Rules match on identity,
and a client controls part of its own identity — most obviously its title — so
re-evaluating a rule must never be a way back into view. A client that becomes
sensitive is hidden immediately; one that was sensitive stays hidden until it
is no longer managed.

Sensitivity also governs indirect pixel paths. Hidden and redacted clients
must not be exposed through captures addressed at other objects: an output
capture is denied (or region-masked, where the backend can composite) while
such a client is visible on that output. Full-output capture is effectively
permission to see every currently displayed pixel, so application scope alone
never makes it safe.

## Tool surface

**Observe.** `desktop.snapshot` returns the full world model in one call:
outputs, workspaces, stacking order, focus, and per-client descriptors (core
identity, application identity, title, role, geometry, workspace, state
flags, specific-transient parent). `client.get` returns one detailed
descriptor. Both carry the sequence number they correspond to.

**Capture.** `client.capture` returns an image of one client's decorated or
content rectangle, stamped with its geometry and sequence number. Capturing a
client while obscured requires composite redirection or compositor
cooperation on X11, so visible-only, while-obscured, and whole-output capture
are three separately advertised capabilities with distinct backend support,
not one promise. Reading pixels off the screen returns whatever is in front of
a window, so a client capture overlapped by a sensitive client takes the
compositing path or is refused: a capture addressed at one object is never a
way to see another. A client that is not mapped has no pixels anywhere — the
server frees its contents and no extension brings them back — so capturing one
is refused rather than answered with a substitute. `output.capture` is deliberately the highest named
sensitivity, because full-screen pixels see everything, and it is subject to
the hidden/redacted exclusion above.

**Input.** All Tier 1 input is window-addressed; global coordinates are
inexpressible. `client.pointer {client, x, y, action, button}` and
`client.key` / `client.type {client, ...}` take content-relative coordinates
that the manager translates against live geometry at injection time, refusing
if the target is gone. An `ensure_visible` flag performs
activate-raise-inject as one operation serialized in the event loop, so it
cannot race the human or a geometry change.

**Manage.** `client.activate` routes through the core `Focus` activation
contract as a first-class activation source, like a pager. `client.close`
uses ICCCM negotiation only; the protocol never exposes `Kill`.
`client.move_resize`, `client.set_state`, and `client.send_to_workspace`
reuse the shared action model's constraints; `workspace.switch` is the
ordinary workspace transition. The agent surface adds intent sources, not new
state machinery.

**Launch.** `launch {desktop_entry_id, ...}` resolves through
`nobox-desktop`'s bounded catalog and safe Exec expansion — never a shell
string — and returns a startup-notification correlation token. The eventual
`client_mapped` event carries that token, so launch-and-identify is one round
trip. Arbitrary command execution is not a capability of this protocol, but a
desktop entry still runs code: launch grants support allow/deny lists of
desktop IDs, application-identity scoping, and a policy switch for whether
user-installed (non-system) entries are launchable at all. Consent UI must
present this capability as "may launch approved installed applications", not
as selecting harmless catalog items.

## Event model

The event stream replaces polling, so it must be trustworthy enough to
maintain a world model against.

- One WM-side monotonic sequence number stamps every snapshot and event. An
  agent snapshots at sequence N and applies events N+1, N+2, … to stay
  consistent.
- Subscription and initial snapshot are established as one atomic operation
  at a single event-loop boundary: `subscribe_and_snapshot(filters)` begins
  buffering and captures the snapshot at the same sequence, so no event
  between "snapshot generated" and "subscription active" can be missed. A
  later re-snapshot on an existing subscription is stamped against the live
  stream the same way. This atomicity is the foundation of a trustworthy
  world model, not an implementation detail.
- Every client descriptor carries a per-client generation counter, bumped on
  any descriptor-visible change. It exists so freshness checks do not require
  exact global-sequence equality.
- Each session has a bounded event queue. On overflow the session receives a
  single `resync_required` event, the backlog is dropped, and the agent must
  re-snapshot. Slow consumers degrade themselves, never the manager.
- Event kinds: `client_mapped` (full descriptor, transient parent, launch
  correlation token), `client_closed`, `title_changed`, `focus_changed`,
  `state_changed`, `geometry_changed` (coalesced; interactive drags emit the
  settled result, not the storm), `workspace_switched`, `human_activity`, and
  the session-control events `session_frozen`, `session_resumed`,
  `session_revoked`.
- Subscriptions filter by event kind at subscribe time. Grant scope filters
  events exactly as it filters snapshots.

## Freshness preconditions

Mutating and input tools accept an optional `expects` block naming the state
the agent observed: client generation, geometry, workspace, focus. The
manager rejects with a structured `stale_state` error — including the current
generation — when a precondition no longer holds, instead of acting on
obsolete assumptions. An agent can therefore say "click this client only if
it is still what I inspected" and re-observe cheaply on rejection.

## Arbitration

The human wins structurally. Agent input is serialized through the manager's
event loop; any human-originated input opens a configurable suppression
window during which agent input calls return a structured `interrupted`
result and a `human_activity` event fires. The protocol offers no way to
contend with the human for the pointer or keyboard — politeness is not
delegated to the agent.

Suppression keys on provenance, not arrival. The manager injects agent input
itself, records what it injected, and never interprets its own injections
returning through server event paths as fresh human activity. Only events the
manager did not originate count as human.

Seeing human input at all requires device-level notifications. A window
manager receives almost none of the user's input through ordinary events —
keys go to the focused client, clicks to the client under the pointer — so the
X11 backend selects XInput2 raw events on the root. Without them the promise
that the human preempts the agent would hold only where the manager already
happened to receive input. Another process's synthetic input still counts as
human, because on X11 the manager genuinely cannot tell it from a keyboard.

Preemption of a multi-step operation (such as `ensure_visible` plus input)
has exact semantics: steps not yet committed are cancelled, steps already
committed are reported as committed, and the result names where the sequence
stopped. No request reports full success after human preemption.

## Security invariants

- Every agent action is attributable to a session identity in structured
  tracing.
- No capability implies another; every request is validated against the grant
  inside the manager, not the companion. A compromised companion gains
  nothing beyond the active WM-issued grant.
- Persisted grants bind to verified peer identity, never to self-declared
  identity strings.
- Hidden clients are indistinguishable from nonexistent clients in every
  response and error, and are never exposed through indirect capture paths.
- Agent input is window-addressed; global input is inexpressible in Tier 1.
- Human input preempts agent input; the kill chord is processed ahead of all
  agent traffic. Suppression counts only events the manager did not itself
  inject.
- Snapshot and subscription establishment are atomic; no event can fall
  between them.
- Consent and status surfaces are WM-owned and cannot be created, covered,
  targeted, or dismissed through the agent protocol.
- A dead, slow, or malicious companion never blocks or crashes the manager.
- Denied, stale, or interrupted requests return structured errors naming
  exactly which steps committed; nothing partially succeeds silently.
- A sequence counts desktop changes one session could observe, and nothing
  else. It is per session, not global: a shared counter moves when some other
  session is delivered an event, which both makes its value depend on who else
  is watching and lets a scoped session read out-of-scope activity out of the
  jumps. Absolute ordering across sessions bought nothing in exchange, since
  no agent can observe another session's events. A client repainting, a page
  loading, or a reply arriving moves no window and therefore does not advance
  it; freshness of what is *inside* a window is a question only pixels answer.
- A result never claims more than the manager observed. Activation, geometry,
  stacking, and workspace movement are manager-owned state, so they are
  reported as `committed`. Input is not: the manager emits events addressed to
  a client and cannot see whether the control under them accepted anything, so
  input answers `injected` with `delivery: unverified`. The distinction is not
  pedantry. Reporting injection as a commit gave agents strong evidence for
  actions that had no effect, and they proceeded on it.

## X11 enforcement caveat

On X11, enforcement is cooperative: any local client can bypass the manager
with XTEST and core protocol requests, which is exactly the status quo agent
harnesses exploit today. Tier 1's present value is the better path — richer,
more reliable, attributable, and consented — that a well-behaved harness
prefers for capability reasons alone. Hard enforcement arrives with the
Wayland backend, where the compositor is the only gate and this protocol is
the sole entry point. Designing the consent model before it is strictly
enforceable is deliberate: the compositor inherits a proven contract instead
of retrofitting one.

## What implementation changed

Three things were learned by running this rather than writing it down, and the
contract above reflects them:

- Sensitivity has to ratchet. Re-evaluating an application rule against a
  changed title let a hidden window rename itself back into view.
- Human preemption needs device-level input notifications, not ordinary
  events, or it silently holds only in the cases where the manager already
  saw the input.
- "Obscured" and "unmapped" are different questions. Composite answers the
  first; nothing answers the second, and saying so is better than substituting
  something plausible.

## Verification boundary

- Grant evaluation, scope filtering, sequence/coalescing rules,
  freshness-precondition evaluation, generation counters, and
  sensitive-client visibility are pure policy and belong in `nobox-core` /
  `nobox-config` unit tests.
- Injection against live geometry, input provenance under suppression,
  snapshot/subscription atomicity under event races, consent UI ownership,
  indicator rendering, kill-chord priority, and companion disconnect behavior
  belong in nested X11 integration tests driving a real `nobox-agent`
  session.
- A harness-facing smoke test exercises the MCP surface end to end:
  snapshot, subscribe, launch with correlation, activate, window-relative
  click, and human-interrupt suppression.
