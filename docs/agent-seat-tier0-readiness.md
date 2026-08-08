# Agent Seat Tier 0 readiness contract

Status: approved as the P0 design input on 2026-08-08, together with the
[threat model](agent-seat-tier0-threat-model.md) and the
[Openbox acceptance contract](agent-seat-openbox-acceptance.md). It specifies
public behavior only. It is not source, a transferable fixture, or
authorization to create the independent repository.

## Goal

Define the smallest honest standalone Agent Seat for an unmodified EWMH X11
window manager. The Tier 0 core observes bounded desktop state, requests only
management operations the foreign manager advertises, and launches only
policy-approved desktop entries. Every answer distinguishes what the provider
observed from what it merely requested.

## Non-goals

- Reproducing Nobox's Tier 1 consent, atomic state changes, authoritative
  generations, secure indicators, or structural human priority.
- Shipping capture, input, or accessibility in the core release.
- Assigning a wire revision before E1 compares every required shape with
  released revisions. An incompatible shape never reuses revision 2.
- Defining Rust APIs, crate layout, reusable tests, or implementation details.
- Providing a Nobox socket fallback, parsing Nobox configuration, or depending
  on any Nobox crate.
- Treating X11 properties, titles, process IDs, or startup tokens as trusted
  identity.

## End result

With Openbox running, an explicitly enabled standalone provider owns one seat,
publishes a race-free discovery record, authenticates local peers, applies its
own deny-by-default grants, and serves the Tier 0 core. A generic MCP companion
can discover it without Nobox knowledge. If the provider is absent, refused,
stale, unsupported, timed out, or dead, the result says so and Openbox remains
usable.

## Profiles and assurances

Feature support and authorization are separate. A feature says the backend can
attempt an operation in this environment; a grant says this peer may request
it. Both must permit a call.

| Profile | Release status | Public behavior | Assurance | Stop condition |
| --- | --- | --- | --- | --- |
| Core observation | Required | Bounded EWMH snapshot and diffs | Provider-observed, not WM-authoritative | Required EWMH baseline is absent or cannot be bounded |
| Core management | Required | Supported activate, close, workspace, state, and geometry requests followed by observation | Requested and observed; never assumed accepted | The result cannot distinguish unsupported, stale, ignored, timed out, and failed |
| Core launch | Required | Bounded XDG catalog, shell-free launch, policy decision, best-effort correlation | Process start observed; window correlation optional and qualified | Entry parsing or policy cannot fail closed |
| Client/output capture | Optional later profile | Grounded, bounded pixels when scope can be reapplied | Standalone X11, explicitly weaker than Tier 1 | Any hidden or out-of-scope pixels can enter the result |
| Pointer/keyboard input | Optional later profile | Fresh client-relative XTEST with best-effort interruption | Events queued, never application acceptance | Human and injected input cannot satisfy the published suppression contract |
| Accessibility | Deferred profile | Bounded client semantic projection | Independently re-proven | Client/root correlation or hidden-client equivalence is ambiguous |

The core reports capture, input, and semantics as typed `unsupported`; it does
not omit the tools in a way that invites guessing and does not emulate them
through a shell or global input.

## Core observation contract

The provider reads public EWMH/ICCCM state and returns only bounded,
session-scoped values:

- desktop count, names, current desktop, geometry, viewport, and work areas;
- managed client list and stacking order;
- active client;
- each visible client's provider-issued handle, desktop, geometry, title when
  granted, type, state, and allowed actions when present.

The observation feature requires valid `_NET_SUPPORTING_WM_CHECK`,
`_NET_SUPPORTED`, `_NET_NUMBER_OF_DESKTOPS`, `_NET_CURRENT_DESKTOP`,
`_NET_CLIENT_LIST`, `_NET_CLIENT_LIST_STACKING`, `_NET_ACTIVE_WINDOW`, and
`_NET_WORKAREA` support. A runtime missing that baseline omits
`ewmh_observation` and reports its tools unsupported. Desktop names, viewport,
individual client properties, and individual management atoms remain optional
facts rather than fabricated release blockers.

Raw XIDs and atoms never cross the provider boundary. Missing properties are
unavailable, not zero-filled facts. Unknown enum values remain unknown or
unsupported; they are not coerced into a familiar value.

The provider takes one bounded observation, assigns a session-local sequence,
and diffs later observations. A subscription begins with a snapshot and a
cursor. Queue overflow produces one `resync_required` result and discards the
old backlog. Tier 0 does not claim that a diff is atomic with a foreign WM's
event loop.

Client generations are provider-local hashes or counters over the descriptor
facts that session can observe. They support stale refusal but are not Nobox's
authoritative Tier 1 generations. Handles and generations end with the
provider session and are never persisted.

Scope and title visibility are applied before response allocation. A direct
lookup of a missing, hidden, or out-of-scope client returns the same public
result. Shared EWMH aggregates may still be influenced by unreported X11
clients; Tier 0 does not claim system-wide noninterference.

## Core management contract

The provider supports an operation only when the root advertises its EWMH atom
and, where applicable, the client advertises the corresponding allowed action.
It re-observes the target, checks scope and supplied freshness, sends the
standard client message, then observes until one of these terminal results:

| Result | Meaning |
| --- | --- |
| `observed` | The requested public EWMH state became true before the deadline. |
| `refused` | Policy or grant denied the request; no message was sent. |
| `stale` | The target or named precondition changed; no message was sent. |
| `unsupported` | The WM/client did not advertise the required operation; no emulation occurred. |
| `timed_out` | A valid request was sent but the requested state was not observed before the fixed deadline. |
| `failed` | The provider could not encode, send, or re-observe the request; diagnostic text is not control data. |

The core subset is activation, polite close, current-workspace change,
send-to-workspace, supported `_NET_WM_STATE` changes, and
`_NET_MOVERESIZE_WINDOW` geometry. It never uses `XKillClient`, XTEST, or a
synthetic menu as a management fallback. A disappearing client is reported as
gone, not as proof that close succeeded unless the close observation contract
was already satisfied. Polite close additionally requires the client to
advertise `WM_DELETE_WINDOW`; otherwise it is unsupported. This prevents a
foreign WM such as Openbox from translating `_NET_CLOSE_WINDOW` into
`XKillClient` for a client with no negotiated close protocol.

## Core launch contract

Launch is a separate capability and a separate global policy. The independent
provider owns its bounded XDG discovery and desktop `Exec` parser. It applies
data-directory precedence, desktop visibility, `TryExec`, field-code, argument,
entry-count, and string bounds before presenting an entry as launchable.

The three policies are deny, allow-listed, and allow-installed with a deny
list. User-writable entries require a separate switch that defaults off. The
provider executes argv directly without a shell and rejects unsupported field
codes or malformed entries before spawning.

A successful spawn returns a unique bounded launch token. Correlation to a
later client is best effort and returned only when public startup-notification
or process evidence is sufficient. No correlation is preferable to a guessed
window.

## Generic discovery contract

### Resolution order

A companion resolves exactly one source in this order:

1. an explicit command-line socket;
2. `AGENT_SEAT_SOCKET` when set;
3. the `_AGENT_SEAT` property on the selected X11 screen's root window.

An explicitly selected source that is empty, malformed, inaccessible, or
incompatible is an error. Resolution does not silently fall through to a lower
precedence source. With no source, the companion reports seat unavailable.
There is no conventional filesystem fallback and no Nobox path.

MCP initialization, discovery, and tool listing do not resolve a seat. The
first status or desktop operation performs resolution and connection.

### Root advertisement grammar

`_AGENT_SEAT` is type `UTF8_STRING`, format 8, and contains exactly three
UTF-8 fields separated by one NUL byte, with no trailing NUL. The complete
value is at most 256 bytes:

```text
agent-seat NUL <unsigned-decimal-wire-revision> NUL <absolute-socket-path>
```

The protocol name is exactly `agent-seat`. The revision is canonical decimal:
ASCII digits only, no sign, whitespace, leading zero except `0`, or overflow.
The socket field is non-empty and contains no NUL. Extra or missing fields,
another X property type/format, or an over-bound value are malformed.

The advertisement names one exact revision, never a range. The opening hello
repeats the protocol and revision; both sides require equality. Unknown
revisions are refused rather than negotiated by structural guesswork.

Provider identity, version, grant, backend features, and assurance are not put
in the spoofable root string. The provider reports them in its bounded welcome
after peer authentication. Feature names are unique and deterministically
ordered. A capability grant never implies a missing feature, and a feature
never grants authority.

The welcome identifies `tier0` assurance and the `x11_ewmh` backend. Its core
feature set is `ewmh_observation`, `ewmh_management`, and `desktop_launch`.
Optional profiles add only the applicable names from `client_visible_capture`,
`obscured_capture`, `output_capture`, `input_injection`, `human_activity`, and
`accessibility`. Unknown feature or assurance names require a revision whose
schema defines them; peers do not ignore them and assume support. Per-client
EWMH allowed actions can narrow `ewmh_management` without changing the welcome.

### Local socket requirements

The Tier 0 provider uses a pathname `AF_UNIX` stream socket, never TCP, UDP, an
abstract socket, or a forwarded transport. Its directory is owned by the user
and mode 0700; the socket is mode 0600. The provider rejects peers whose local
credentials do not match the configured user and binds grants to verified peer
identity, not declared harness strings.

All discovery paths are absolute, valid for the platform's `sockaddr_un`, and
bounded before allocation or connection. The provider's normal path is below
its own directory in `XDG_RUNTIME_DIR`; the companion does not synthesize that
path. A provider may unlink an exact stale socket only after it owns the screen
selection and a connection probe confirms that no listener is alive.

### Atomic provider ownership

The per-screen selection is `_AGENT_SEAT_S<screen-number>`. A conforming
provider creates a dedicated owner window, obtains a current server timestamp,
refuses startup when the selection already has an owner, claims the selection,
and verifies ownership before publishing or accepting sessions. It never uses
the X11 replace convention.

After the matching owner/root properties are installed, the provider sends the
standard format-32 `MANAGER` client message to the root with the acquisition
timestamp, selection atom, and owner window. Consumers still validate current
ownership and both properties; the announcement is notification, not proof.

The owner window carries the same `_AGENT_SEAT` value as the root. Root
discovery is valid only while the selection has an owner and the owner-window
and root values are byte-identical. This makes a stale root property inert and
binds the location to the atomic owner. A provider that receives
`SelectionClear` stops accepting work, withdraws only properties it still
owns, closes sessions and its socket, and exits nonzero.

On clean shutdown a provider removes the root property only if it still equals
its own advertisement, destroys its owner window, closes the listener, and
unlinks its exact socket. It never removes another provider's property or
socket.

All conforming integrated and standalone providers use this ownership
contract. Nobox must adopt it before the independent repository is created;
the behavior-neutral crate rename does not absorb that change.

## Published performance evidence

Nobox v0.1.1 provides prior behavioral evidence, not reusable implementation
or acceptance fixtures. In its isolated measurements:

| Runtime/task | Semantic work | Grounded capture |
| --- | --- | --- |
| Firefox video, three reflows | 2 calls, 705--757 JSON B, 2,401 ms | 1 call, 49,867--54,840 PNG B, 247--366 ms |
| Firefox canvas-only | 1 call, 66 JSON B, 1,200 ms, no match | 1 call, 49,867--54,564 PNG B, 247--348 ms |
| GTK | 2 calls, 532 JSON B, 2,401 ms | 1 call, 56,124 PNG B, 205--208 ms |
| Qt | 2 calls, 536 JSON B, 2,401 ms | 1 call, 5,363 PNG B, 78--83 ms |
| Chrome unavailable | 1 call, 111 JSON B, 1,200 ms | 1 call, 25,620 PNG B, 211--219 ms |

The Tier 0 core has no pixel or semantic payload and should remain materially
smaller. The independent project establishes its own timings, fixtures, and
thresholds; these numbers are comparison baselines, not pass values.

## Revision decision

E1 compares this contract with each released wire revision before assigning
the independent implementation's first supported revision. Revision 2 may be
claimed only if every framing, feature, assurance, management-result, and error
shape is black-box compatible with Nobox v0.1.1. Otherwise E1 allocates a new
revision and documents the incompatibility. Similar JSON is not compatibility.

## P0 exit checklist

- [x] The maintainer approves this feature, discovery, ownership, and release
  scope.
- [x] The Tier 0 threat model is approved with every optional-profile stop
  condition intact.
- [x] The Openbox contract expresses every pass condition at a public process,
  X11, filesystem, or socket boundary.
- [x] The roadmap includes Nobox's atomic-ownership milestone before G0.
- [x] No repository, source crate, transferable fixture, or implementation
  branch has been created for the independent product.
