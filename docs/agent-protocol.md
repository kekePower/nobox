# Agent protocol

Status: design. Nothing in this document is implemented yet. It specifies the
contract that `nobox-agent` and the WM-side agent surface are built and
verified against.

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
traditional X11 way: a root property naming the protocol version and socket
path, so capability discovery needs no side channel.

## Process shape and trust boundary

`nobox-agent` follows the `nobox-xsmp`/`nobox-panel` companion pattern: a
separate optional process, spawned per agent harness, speaking MCP on stdio
toward the agent and a typed, bounded protocol to the manager over a UNIX
socket. The companion is a translator, not an authority. It faces the agent,
so the WM treats it as exposed: every request is validated against the
session's grant inside the manager, regardless of any companion-side checks.

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
   an authorization input. The manager separately records verified peer
   identity: UID/PID from socket peer credentials, executable identity via
   pidfd/`/proc` where available, and a best-effort parent-process chain. Each
   connection carries a manager-issued nonce; a session cannot be resumed or
   replayed. On X11 this verification is informative rather than a hard
   boundary — any same-user process can bypass the manager entirely — but it
   is specified now because persisted grants bind to it and the Wayland
   backend enforces it.
2. **Consent.** The manager checks `[agent]` configuration for a stored grant;
   otherwise it renders its own consent dialog showing the identity and
   requested capabilities. The dialog is WM-drawn and cannot be created,
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
holds `input` or `capture`, and a visible marker (cursor badge or frame tint)
on the client currently receiving agent input. On X11 other same-user clients
remain able to imitate or obscure UI generally; under Wayland the claim
becomes system-level.

## Sensitive clients

Application rules gain `agent_visibility = "visible" | "redacted" |
"hidden"`. Redacted clients appear in snapshots with existence and geometry
but no title; capture and input against them fail. Hidden clients are absent
from every response and event, and acting on their identity returns the same
"no such client" error as a genuinely nonexistent one, so errors are not an
oracle for what is hidden.

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
not one promise. `output.capture` is deliberately the highest named
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
itself, tags those events, and never interprets its own injections returning
through server event paths as fresh human activity. Only events the manager
did not originate count as human.

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
