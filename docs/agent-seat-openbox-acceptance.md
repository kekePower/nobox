# Agent Seat Tier 0 Openbox acceptance contract

Status: approved for P0 on 2026-08-08. The future independent product authors
its own driver and fixtures from this observable specification. No Nobox test,
fixture, or implementation is transferred.

## Goal

Prove that the Tier 0 core is useful and fails predictably beside a released,
unmodified Openbox session. Cover discovery, ownership, peer policy, bounded
observation and diffs, supported EWMH management, controlled desktop-entry
launch, and isolation from every provider failure.

## Non-goals

- Testing Nobox or requiring an Openbox patch, plugin, config parser, or Agent
  Seat awareness.
- Reaching Openbox internals or accepting provider logs as proof of desktop
  behavior.
- Requiring capture, input, accessibility, a consent dialog, panel, compositor,
  browser, or network service for the core release.
- Treating request delivery as acceptance by Openbox.
- Reusing the Nobox conformance probe, shell tests, HTML pages, desktop files,
  or expected byte fixtures.

## End result

From public process, filesystem, X11, and socket boundaries, a release tester
can answer yes or no to every core claim. On every denial, malformed request,
timeout, disconnect, provider crash, and duplicate-start attempt, existing
Openbox keyboard/mouse management and ordinary applications continue working.

## Test environment

The release matrix records exact versions of the provider, protocol,
companion, Openbox, X server, EWMH extensions, OS, and Rust toolchain. Tests run
inside an isolated Xvfb/Xephyr/Xnest display and private runtime/config/data
directories. No test reads or mutates the person's live desktop.

At least one simple independently authored X11 client exposes deterministic
title, class, geometry, state, desktop, and `WM_DELETE_WINDOW` behavior. Launch
fixtures live only in the independent repository's private XDG data tree. A
second ungranted client is the scope/privacy control.

The driver observes through:

- process exit/status and bounded stdout/stderr;
- exact filesystem type, owner, mode, and path;
- X11 selections and properties;
- the published Agent Seat socket;
- a generic MCP process where MCP translation is under test;
- ordinary EWMH/X11 queries independent of provider replies.

Provider log text may diagnose a failure but never makes a test pass.

## A. Current no-provider baseline

1. Start Openbox with no provider and no `_AGENT_SEAT` owner/property.
2. Start the generic companion in a sanitized environment.
3. MCP initialization/discovery and tool listing finish without a desktop
   connection and expose the static core surface.
4. The first seat-status or desktop call returns bounded seat-unavailable, with
   no retry storm or Nobox-specific path attempt.
5. Start, focus, move, resize, and close an ordinary client through Openbox.

Pass: companion absence/failure changes no Openbox behavior. This preserves the
failed historical `nobox-agent` experiment as an expected architecture
baseline: a translator is not a provider.

## B. Discovery and lifecycle

### B1. Explicit enablement

- With provider disabled or configuration absent, no listener, selection, or
  property is created.
- With valid deny-by-default configuration, the provider creates one private
  pathname socket, claims `_AGENT_SEAT_S<screen>`, publishes identical bounded
  advertisements on its owner window and root, sends the standard `MANAGER`
  announcement, and becomes ready only after ownership, both properties, and
  the listener agree.
- Socket directory and inode ownership/modes match the readiness contract.

### B2. Resolution precedence

Exercise three distinct valid providers/fixtures so the chosen endpoint is
observable:

1. explicit `--socket` wins over environment and root;
2. `AGENT_SEAT_SOCKET` wins over root when no explicit path is supplied;
3. the live selection-bound root advertisement is used otherwise;
4. no source yields seat unavailable, never a conventional fallback.

For each higher-precedence source, empty, relative, overlong, malformed,
unreachable, wrong-peer, wrong-protocol, and wrong-revision values fail at that
source without falling through.

### B3. Atomic ownership

- A second provider on the same screen refuses before accepting sessions and
  does not alter the first provider's socket or properties.
- Providers on distinct isolated screens use distinct selection ownership.
- A stale root property with no owner, or a root value different from the
  owner-window value, is ignored.
- Forced selection loss stops the displaced provider, disconnects its peers,
  and leaves the new owner's artifacts untouched.
- Clean stop removes only the stopping provider's exact property/socket.
- `SIGKILL` leaves Openbox usable; the dead owner disappears automatically and
  the next start verifies and removes only its own stale socket.

### B4. Resource failure

Refuse predictably on unwritable/unsafe runtime directories, occupied
non-socket paths, exhausted display slots, invalid configuration, socket-path
overflow, X server loss, and listener failure. No case terminates Openbox.

## C. Peer identity, grants, and bounds

- A peer with no matching configured grant completes or receives the opening
  denial defined by the selected wire revision, but gains no capability.
- Two executable identities with identical declared harness/purpose strings do
  not share a grant.
- Copy/replacement behavior follows the published executable-identity rule and
  is tested explicitly; declared strings never alter it.
- A grant narrowed or removed on transactional reload revokes affected live
  sessions before their next request.
- Scope is applied identically to snapshot, lookup, diff, management, and
  launch correlation.
- Unknown configuration keys, partial invalid reloads, and excessive grant or
  scope entries fail without partially changing live policy.

Send zero-length, truncated, malformed, wrong-order, unknown-field, wrong-type,
maximum-size, and one-byte-oversize frames. Open many idle peers, a peer that
never reads, a peer that never completes a frame, and a peer that floods calls.
Each hits a documented finite bound or deadline and disconnects without
blocking a conforming peer or Openbox. Memory and descriptor use return to the
published idle envelope after disconnect.

## D. Observation and bounded diffs

### D1. Snapshot

Create clients on two desktops with known titles, types, geometries, states,
and stacking. Compare the provider result with independent EWMH reads.

Pass conditions:

- desktop count/current desktop/work area and visible client facts agree with
  the same bounded observation;
- every client handle is opaque and stable only in its provider session;
- no raw XID/atom appears in structured or text results;
- missing EWMH facts are absent/unavailable rather than invented;
- titles appear only with their title capability;
- the ungranted control client has no handle, title, direct lookup distinction,
  or event;
- a reconnect invalidates old handles.

### D2. Diffs and convergence

Subscribe, take the initial snapshot/cursor, then independently map, rename,
move, resize, minimize/restore, restack, change desktop/state/focus, and destroy
clients. After provider debounce/poll bounds, applying delivered diffs must
converge on a fresh snapshot.

The sequence is monotonic within one session. Duplicate or coalesced
observations are allowed only as documented; the final state must converge.
Overflow a deliberately small test queue: exactly one resync indication
replaces the abandoned backlog, and a new snapshot restores convergence.

Changing only a hidden client's title/geometry produces no direct diff or
sequence oracle promised absent by the contract. Global work-area/focus effects
are evaluated under the threat model's stated Tier 0 limitation, not under
Tier 1 equivalence.

### D3. Malformed EWMH data

Exercise absent, wrong-type, truncated, oversized, duplicate, unknown, and
changing-during-read properties. The provider bounds retries and values,
returns unavailable where appropriate, and remains responsive. It does not
crash, spin, allocate from an untrusted count without a cap, or leak a filtered
client through an error.

## E. EWMH management

For each operation below, first prove the required root atom and applicable
client allowed-action atom are advertised. Supply the current client handle and
generation, request the operation, then verify the terminal result against an
independent EWMH read.

| Operation | Required observation |
| --- | --- |
| Activate | A source-2 request with a current server timestamp makes the requested client `_NET_ACTIVE_WINDOW`. |
| Polite close | A client advertising `WM_DELETE_WINDOW` records that protocol and disappears; a client without it is unsupported. |
| Switch workspace | A request with a current server timestamp makes `_NET_CURRENT_DESKTOP` the requested valid index. |
| Send to workspace | Target `_NET_WM_DESKTOP` becomes the requested index/sticky value. |
| Change state | Only requested supported `_NET_WM_STATE` atoms reach the desired presence. |
| Move/resize | Public frame/client geometry reaches the requested, WM-constrained result defined by the operation. |

Each operation also covers:

- denied capability and out-of-scope target: `refused`, no client message;
- old handle/generation or changed expected fact: `stale`, no client message;
- unadvertised root/client support: `unsupported`, no XTEST/menu fallback;
- Openbox intentionally configured or fixture-designed to ignore the request:
  `timed_out`, never observed success;
- target destroyed before send and after send;
- malformed arguments and over-bound values;
- provider/X server failure while waiting.

The result must state whether no request was sent, a request was sent but not
observed, or the requested public state was observed. Diagnostic wording is
not used to select the branch.

## F. Desktop-entry launch

Use an isolated XDG catalog containing:

- one valid system-style allowed entry;
- one valid unlisted entry;
- one valid user-writable entry;
- one hidden/shadowing entry;
- malformed `Exec`, unsupported field-code, missing `TryExec`, over-bound, and
  shell-metacharacter entries;
- one allowed entry that exits unsuccessfully and one that maps a client with
  valid public correlation evidence.

Pass conditions:

1. deny policy launches nothing;
2. allow-listed launches only the exact allowed desktop ID;
3. allow-installed honors the deny list;
4. the user entry is refused until the separate switch is enabled;
5. precedence/visibility/`TryExec` and all catalog bounds are enforced;
6. argv is executed directly: metacharacters create no extra process/file and
   unsupported fields are refused;
7. successful spawn returns a bounded unique token;
8. correlation is returned only for sufficient evidence and otherwise remains
   explicitly unavailable;
9. spawn/correlation failure does not hang the provider or Openbox.

Launch scope/policy is rechecked at request time. A catalog change between list
and launch yields a current policy/result, not execution of cached argv.

## G. Required failure vocabulary

The selected wire revision gives machine-readable, mutually exclusive results
for at least:

| Condition | Required actionability |
| --- | --- |
| unavailable provider | Start/select a provider; do not retry-spin. |
| unsupported revision | Install a compatible pair; do not parse by shape. |
| denied/refused | Change grant/policy out of band or stop. |
| absent/hidden/out-of-scope client | Re-observe; no visibility distinction. |
| stale | Re-observe and retry only if still intended. |
| unsupported backend action | Choose another supported operation; no emulation. |
| timed out after request | Treat outcome as unknown, re-observe before deciding. |
| malformed/invalid/too large | Correct the exact bounded field or stop. |
| internal/provider failure | Retry only according to explicit retry metadata. |
| resync required | Discard local model and take a new snapshot. |
| revoked/session closed | Obtain a new out-of-band grant/session or stop. |

Tests assert stable codes and structured fields, not English messages.

## H. Openbox availability after failure

After every forced provider failure above, an independent control sequence must
still:

1. map a new ordinary client;
2. focus it with Openbox;
3. move and resize it;
4. switch desktops;
5. close it politely;
6. confirm the Openbox process remains alive and owns `WM_Sn`.

Run this after malformed-peer disconnect, slow-peer eviction, invalid reload,
duplicate provider refusal, selection loss, provider `SIGTERM`, provider
`SIGKILL`, launch failure, management timeout, and X server reconnection test
where applicable. Provider success is never coupled to the WM control result.

## I. Core-release exit matrix

| Area | Required for Tier 0 core | Optional/later |
| --- | --- | --- |
| No-provider behavior | Pass | — |
| Discovery and atomic ownership | Pass | — |
| Local peer credentials and configured grants | Pass | Consent UI |
| Bounded snapshot/diffs/resync | Pass | WM-atomic pushed events |
| Supported EWMH management with observed outcomes | Pass | Non-EWMH emulation |
| Policy-controlled desktop launch | Pass | Settings GUI |
| Capture | Must report unsupported | Separate T4 approval/tests |
| Input/human interruption | Must report unsupported | Separate T5 approval/tests |
| Accessibility | Must report unsupported | Separate T6 approval/tests |
| Openbox failure isolation | Pass after every case | — |

The release passes only when every required case succeeds from a clean source
build on the published matrix. Skips caused by a missing optional browser or
toolkit do not affect the core; skips caused by missing Openbox, X11 isolation,
peer credentials, or required EWMH coverage block the core release.

## P0 review checklist

- [x] Every pass condition is visible outside implementation internals.
- [x] The no-provider baseline matches the historical Openbox result.
- [x] Management separates requested, observed, refused, stale, unsupported,
  timed-out, and failed outcomes.
- [x] Launch tests prove both policy and shell-free parsing.
- [x] Provider failure never becomes an Openbox failure.
- [x] Optional profiles cannot slip into the core release implicitly.
- [x] Future fixtures and driver will be independently authored in the
  ZaguanLabs repository after E0.
