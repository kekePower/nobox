# Why integrated Tier 1 was easier than standalone Tier 0

Agent Seat's tiers describe guarantees, not implementation difficulty. The
apparently richer Nobox Tier 1 implementation was easier to build and reason
about because Nobox already owned the desktop state, policy decisions, and
event ordering that Agent Seat needed. The standalone Tier 0 provider must
reconstruct a weaker approximation of those facts from outside an unmodified
window manager.

There is also an important naming distinction: `nobox-agent` is not the Tier 1
security authority. It is an intentionally untrusted MCP translator. The
authority lives in `nobox-core` and `nobox-x11`, and every request from the
companion is checked again inside the manager.

```text
Tier 1:
agent -> untrusted companion -> Nobox
                                |- owns policy
                                |- owns client identity and state
                                |- performs the action
                                `- produces authoritative events

Tier 0:
agent -> untrusted companion -> standalone provider -> EWMH request -> foreign WM
                                        ^                    |
                                        `- sample state <----'
```

That placement of authority changes almost every security and correctness
problem.

| Problem | Nobox Tier 1 | Standalone Tier 0 |
| --- | --- | --- |
| Window identity | Native `nobox-core` client identities | XIDs can disappear or be reused, so the provider issues session-local opaque handles |
| Freshness | Authoritative generations updated with real state changes | Provider-local generations inferred from repeated samples |
| Mutation | Direct policy operation serialized in the WM event loop | Advisory EWMH request followed by observation of what happened |
| Events | Produced when Nobox changes managed state | Diffs reconstructed from non-atomic property samples |
| Scope | Hidden clients are removed at the policy source | The provider filters after the foreign WM has published global effects |
| Input | Nobox schedules and identifies its own injection | XTEST has no trustworthy origin marker and races an independent WM |
| Human priority | Structural event-loop priority and cancellation | External activity evidence, coordination, and lock/session handling |
| Consent and status | WM-owned surfaces outside Agent Seat targeting | Standalone X11 surfaces can be covered, imitated, or manipulated |
| Failure handling | An optional subsystem inside an existing manager lifecycle | A separate authority needs socket, ownership, recovery, policy, and lifecycle machinery |

## Nobox already possessed the required truth

Nobox did not need to add a second model of the desktop. `nobox-core` already
owned protocol-neutral identities, geometry, focus, stacking, workspaces, and
client state. Agent Seat could expose a filtered projection of that model and
accept typed policy intents against the same objects.

The implementation consequently has a direct reference-monitor shape:

- a grant and its application scope use native `ClientId` values;
- `AgentState::authorize` checks every call inside the manager;
- `AgentState::check_expects` compares preconditions directly with the current
  managed client, geometry, workspace, focus, and generation;
- snapshots are built from the live core client, stacking, focus, workspace,
  and output models; and
- snapshot/subscription establishment and mutations are serialized with the
  manager's event loop.

The same component therefore knows whether an operation is allowed, whether
its target is current, and when the operation commits. More authority in the
reference monitor does not grant the agent more authority. It makes the
agent's smaller grant easier to enforce precisely.

## Tier 0 sees requests and observations, not decisions

EWMH is an interoperability surface for publishing properties and sending
requests. It is not an authoritative policy interface. Properties may be
missing, stale, inconsistent, or spoofed, and a foreign manager may ignore a
valid client message.

The standalone provider must therefore:

- validate which features the root and target advertise;
- repeatedly sample bounded EWMH and ICCCM state;
- replace raw XIDs with opaque session handles;
- compute provider-local generations and event diffs;
- refresh scope and freshness immediately before mutation;
- send a standard request without assuming acceptance; and
- observe until it can distinguish `observed`, `timed_out`, target-gone, and
  internal-failure outcomes.

Even careful sampling cannot make a foreign WM's state atomic. A server grab
can close the provider's own observe/send scheduling window, but it cannot
turn the provider into the window manager or make later realization atomic.

Scope has the same limitation. Tier 0 can omit a hidden client, its title, and
its direct events, but it cannot erase effects the foreign manager has already
published through work areas, focus, placement, or stacking. Nobox can make a
hidden client absent from the Agent Seat model at its source.

## Standalone policy is additional infrastructure

The Tier 0 provider must create a complete authority beside software that does
not participate in Agent Seat. It owns a private socket, peer-credential and
executable checks, strict configuration, grants, scopes, provider selection,
bounded sessions and queues, EWMH feature detection, controlled desktop-entry
launch, and stale-object recovery. Failure must remain isolated so Openbox or
another foreign manager stays usable.

Most of the corresponding building blocks already existed naturally in
Nobox's manager lifecycle, strict configuration, client policy, and bounded
XDG application catalog. Tier 1 extended those facilities; Tier 0 had to
independently construct them.

The independent Apache-2.0 product also follows a clean-source provenance
rule. It cannot reuse Nobox's GPL-2.0-only wire implementation, schemas,
parsers, fixtures, tests, comments, or prose. Public behavior is implemented
again from standards, the public specification, and black-box compatibility
tests. This is not a security property, but it materially increases the work
needed to obtain the same public behavior.

## Human-priority input exposes the difference most clearly

Nobox injects agent input itself and routes desktop policy and input through
one event loop. It can distinguish its own injection from other observed
activity, give the human path structural priority, cancel uncommitted steps,
and stop paced input if focus or freshness changes.

An external X11 provider does not receive trustworthy provenance from XTEST,
XInput, or X Record. Reliably detecting physical activity pushes Tier 0 toward
kernel evdev access. That is keylogging-grade machine authority and brings
device enrollment, complete seat coverage, hotplug and overflow behavior,
lock and virtual-terminal transitions, session eligibility, confinement, and
a separate broker failure boundary.

The governing rule is therefore that Tier 0 must not acquire extra machine
authority merely to imitate a guarantee Tier 1 gets naturally from owning the
display server. A smaller, honest Tier 0 without input is preferable to a
nominally feature-compatible implementation with an indefensible boundary.

Capture and accessibility have related problems. A standalone provider must
prove that hidden or out-of-scope pixels cannot enter a result and correlate
an accessibility tree with the intended X11 client. The integrated manager
already knows the authorized client, its current lifecycle, geometry,
visibility, and generation, so it can reapply policy at the sensitive action.

## X11 remains a cooperative security boundary

Nobox Tier 1 is structurally stronger, but current X11 does not isolate
mutually hostile processes sharing one display authority. A malicious
same-user X11 client can bypass Agent Seat with ordinary X11 requests or
XTEST. Tier 1's present guarantees apply to the Agent Seat path: a compromised
companion remains bounded by its manager-issued grant, desktop state is
authoritative, disclosure is scoped, and operations are serialized and
attributable.

Hard enforcement follows naturally in a future Wayland backend, where Nobox
as compositor is the sole gate. The protocol-neutral `nobox-core` policy model
was designed so the already-proven Tier 1 contract can become enforceable
without turning X11 into the internal model.

In short, Tier 1 was easier because Nobox already owned the facts, decisions,
and ordering. Tier 0 owns none of them but must mediate them safely without
claiming that its observations are authoritative. Standalone compatibility
reduces the guarantees while increasing the coordination and proof burden.

Related documents:

- [Agent Seat Protocol](agent-protocol.md)
- [Agent Seat product separation and Tier 0 roadmap](agent-seat-separation-roadmap.md)
- [Agent Seat Tier 0 readiness contract](agent-seat-tier0-readiness.md)
- [Standalone X11 Agent Seat threat model](agent-seat-tier0-threat-model.md)
- [Architecture](architecture.md)
