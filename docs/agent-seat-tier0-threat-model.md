# Standalone X11 Agent Seat threat model

Status: approved for P0 on 2026-08-08. This model applies to an unmodified X11
window manager such as Openbox. It does not inherit Nobox's Tier 1 assurances.

## Goal

Constrain a local agent companion to an explicit, auditable grant while keeping
the foreign window manager usable through provider, peer, policy, and backend
failure. State exactly which protections are real on same-user X11 and stop an
optional profile when its advertised protection cannot be upheld.

## Non-goals

- Isolating mutually hostile processes that already share one X11 connection
  authority.
- Preventing a same-user X11 client from reading windows, injecting events,
  spoofing EWMH properties, drawing deceptive UI, or taking a selection.
- Providing remote access, multi-user delegation, a privileged daemon, a
  security prompt, or a secret store.
- Claiming that Openbox accepted a client message, an application accepted
  input, or launch opened a particular window without later evidence.
- Making hidden clients globally noninterfering. Tier 0 controls its own
  outputs, not the foreign WM's shared work area or stacking decisions.

## End result

Against accidental overreach, a compromised MCP translator, malformed peers,
stale agent beliefs, and ordinary provider failures, the standalone boundary
fails closed and remains bounded. Against a malicious same-user X11 client,
the product makes only the limited claims listed below and directs users to OS
account/session isolation for a stronger boundary.

## Assets

- the provider's private socket, configuration, live grants, and audit events;
- scoped desktop structure and titles returned to a peer;
- authority to request EWMH changes or launch desktop entries;
- optional pixels and input authority, if those profiles later pass review;
- Openbox availability and the person's continuing control of the session;
- protocol integrity: exact revision, bounded frames, stable result meanings,
  and no silent partial success.

The provider does not treat XIDs, root properties, client properties, titles,
PIDs, startup tokens, pixels, or accessibility content as secrets it can
protect from another same-user X11 client.

## Actors and trust

| Actor | Trust decision |
| --- | --- |
| Person/session owner | Chooses configuration and grants; may stop the provider. |
| Standalone provider | Sole policy authority for its socket; trusted to enforce its documented bounds. |
| MCP companion | Untrusted translator. Every request is revalidated by the provider. |
| Agent/model/harness | Untrusted within the active grant and scope. Declared names are display-only. |
| Openbox/foreign WM | Source of observed EWMH behavior, not part of the provider's failure boundary or policy engine. |
| X11 server | Carries observations and requests; offers no same-user client isolation. |
| Application/desktop entry | Untrusted content and code, including its properties and launch metadata. |
| Other same-user process | Can bypass or spoof X11-level controls; outside the security boundary. |
| Different OS user/network peer | Must not reach the private local socket. |

## Assumptions

- `XDG_RUNTIME_DIR` is local, owned by the session user, and mode 0700.
- Provider configuration and its parent directory are not writable by another
  user. Unsafe ownership or mode is a startup refusal.
- The OS exposes peer credentials for pathname UNIX sockets. If verified local
  identity is unavailable, configured persistent grants are unsupported.
- Openbox supplies the EWMH atoms needed by a feature before that feature is
  advertised. Missing behavior is normal and produces `unsupported`.
- Every buffer, frame, list, queue, scan, string, image, and deadline has a
  published finite bound checked before expensive work.

## Core guarantees

1. The provider is disabled and deny-by-default.
2. Only one conforming provider owns a screen. Selection loss is fatal.
3. Only verified local peers can reach grant evaluation; declared identity is
   never an authorization key.
4. The provider, not the companion, checks capability, scope, policy, feature,
   and freshness for every request.
5. Missing, hidden, and out-of-scope direct client lookups are indistinguishable
   in provider responses.
6. Raw X11 identities never leave the provider.
7. Mutation reports observation, refusal, stale state, unsupported behavior,
   timeout, and internal failure distinctly.
8. A slow, malformed, disconnected, or malicious peer cannot block Openbox or
   create unbounded provider state.
9. Provider exit, crash, or selection loss leaves Openbox usable.
10. No core operation falls back to shell commands, XTEST, or global screen
    coordinates.

## Known X11 limits

The X11 server normally grants all clients of one user broad authority. A
same-user process can bypass the provider and send EWMH messages itself, inspect
windows, inject input, alter root/client properties, imitate an indicator, or
steal the ownership selection. The selection prevents accidental simultaneous
conforming providers; it is not an unstealable lock. On `SelectionClear`, the
losing provider exits so two conforming providers do not continue silently.

EWMH identity is self-asserted. Class, title, role, PID, type, state, allowed
actions, and startup tokens can be missing, stale, or spoofed. Policy matching
therefore limits cooperative agent behavior but cannot authenticate a hostile
application. A high-risk workflow requires a separate OS user, nested X server,
container, or compositor-enforced session.

## Threats and controls

| Threat | Core control | Residual limit / required result |
| --- | --- | --- |
| Second provider races startup | Per-screen X11 selection, owner-window advertisement, verified ownership before publish | A malicious client can steal the selection; the displaced provider exits |
| Stale root property or socket | Require live selection owner and byte-identical owner/root advertisement; probe before exact stale unlink | Explicit/env paths bypass root discovery intentionally but still require handshake and peer checks |
| Remote or cross-user peer | Filesystem UNIX socket, private directory/mode, peer credentials | No TCP, abstract socket, forwarding, or multi-user ACL in Tier 0 |
| Companion grants itself power | Provider checks every call against verified-peer grant | Compromised companion retains only its configured grant |
| Self-declared identity spoof | Names are display/log metadata only | Persistent grant requires verified executable identity according to platform policy |
| Oversized/malformed traffic | Length prefix bound before allocation, strict schema, per-session rate/queue limits, disconnect | Diagnostic text is bounded and never parsed for recovery |
| Slow reader/writer | Nonblocking or independently bounded I/O, finite output queue, deadline, disconnect | Other peers and Openbox continue |
| Raw XID reuse | Session-local opaque handles plus current descriptor generation | Provider restart invalidates all handles |
| Stale target | Re-observe immediately before mutation and check supplied generation/facts | Tier 0 generation is provider-observed, not WM-authoritative |
| Hidden client probing | Filter before allocation; identical missing/hidden/out-of-scope result; no raw counts or gaps | Shared work area, active-window absence, or other aggregate effects may reveal that the desktop changed |
| Property/title spoofing | Treat values as untrusted observations; bound and sanitize display/log text | Cannot authenticate an application through EWMH alone |
| Unsupported management | Check root/client advertised support; never emulate | Return typed `unsupported` |
| Close becomes a forced kill | Require `WM_DELETE_WINDOW` before sending `_NET_CLOSE_WINDOW` | A foreign WM's close policy beyond that public contract remains its own |
| Ignored EWMH request | Observe until fixed deadline | Return `timed_out`, not success |
| Client disappears mid-call | Re-observe and report gone or observed terminal state exactly | Disappearance alone is not generic proof of requested success |
| Hostile desktop `Exec` | Strict bounded parsing and direct argv; no shell; reject unknown fields | An allowed desktop entry still executes code and is presented as such |
| User-writable launch entry | Separate off-by-default policy switch | Enabling it is an explicit code-execution decision |
| Provider crash | Separate process, no WM hooks, no required panel, system supervision optional | Socket/property may remain stale but are ignored/recovered safely |
| Sensitive log content | Structured IDs/codes by default; no titles, arguments, pixels, typed text, or semantic content in normal logs | Explicit diagnostic mode must document any added content and remain bounded |

## Configuration and grant handling

The provider reads one strict TOML file under its own XDG configuration
directory. Unknown keys, duplicate semantic entries, unbounded values, unsafe
file ownership/mode, or an invalid complete document refuse startup or reload
without partially applying it. It does not execute configuration.

The baseline has configured grants only; there is no provider-drawn consent
dialog. A grant binds to verified local peer identity and lists capability
atoms plus optional application scope. Reload is transactional. A removed or
narrowed grant revokes affected live sessions before accepting another call.
Revocation does not claim to undo already observed management or launch.

Audit events contain timestamp, provider session, verified peer identity,
operation, target handle when visible, decision code, and terminal observation.
They omit application content. Retention and forwarding are administrator
choices outside the protocol and default to ordinary bounded system logging.

## Observation and hidden-client limits

Filtering happens while building a session result, before allocating strings
or event objects. Hidden/out-of-scope clients receive no handle, descriptor,
title, direct event, capture, or semantic root. Stacking lists are compacted,
not left with visible gaps. Active client is absent when filtered.

Tier 0 cannot erase indirect effects already published by the WM. A hidden dock
may alter `_NET_WORKAREA`; a hidden client may affect placement, focus, or the
visible clients' stacking; desktop counts are global. The provider documents
these facts rather than claiming Nobox's stronger hidden-client equivalence.
If a proposed field creates a new direct oracle without user value, omit it.

## Optional capture profile

Capture receives a separate review. At capture time the provider rechecks
grant, scope, visibility, geometry, stacking, and target existence. Returned
pixels are bounded and stamped with source rectangle, coordinate space,
provider sequence, target generation, and Tier 0 assurance.

The profile stops or narrows when any of these is true:

- an out-of-scope or hidden client can enter a client/output capture;
- an off-workspace, destroyed, unmapped, or stale target can yield substitute
  pixels;
- obscured capture cannot prove which drawable supplied the pixels;
- image dimensions or encoding work cannot be bounded before allocation;
- provider failure can hang or destabilize the WM.

Client-visible-only is acceptable if named honestly. Output or obscured capture
is not required for release.

## Optional input profile

Input receives a separate review and is explicitly best effort. Coordinates
are client-relative and translated only after fresh geometry and focus checks.
XTEST availability is a reported feature, not an implied grant. Results say
events were queued; they never claim application acceptance.

The profile attempts XInput human-activity detection, a configurable
suppression window, cancellation of pending work, bounded paced text, and a
local emergency stop independent of MCP. It stops in an environment where:

- provider injection cannot be distinguished sufficiently from observed human
  input to implement the stated suppression rule;
- stale geometry or lost focus can silently redirect input;
- a local stop cannot interrupt pending work;
- partial progress cannot be reported exactly;
- global coordinates would be needed as fallback.

An X11 indicator, if added, is explicitly spoofable and coverable. It is a
status aid, not a security boundary.

## Deferred semantic profile

Nobox's B6 measurements are prior evidence only. The independent product must
repeat client-to-accessible-root correlation and hidden-scope analysis with its
own design and fixtures. A disposable helper has no policy authority. Missing,
ambiguous, slow, crashed, or over-bound correlation has one safe-unavailable
result. No desktop-wide tree or raw accessibility identity is exposed.

The profile remains unsupported if filtered clients can affect returned
content distinguishably or a current node cannot be grounded to the requested
client. Persistent coordinates, page content, trees, and learned workflows are
never introduced as fallback.

## Review checklist

- [x] Every claimed boundary names the actor it protects against.
- [x] Same-user X11 bypass is stated wherever it changes the meaning of a
  feature.
- [x] Provider ownership, socket identity, grants, scope, freshness, and
  failure isolation have public acceptance checks.
- [x] Core observation, management, and launch have no dependency on capture,
  input, semantics, Nobox, or an MCP lifecycle.
- [x] Each optional profile retains its independent stop condition.
- [x] No result language exceeds what the provider observed.
