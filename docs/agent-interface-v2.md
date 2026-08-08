# Agent interface v2 roadmap

Status: implementation started. This plan follows the completed X11 baseline
in `agent-roadmap.md`; it does not reopen or weaken that baseline.

Naming and product note: source references below use the current in-tree crate
name `agent-seat-proto`. It is Nobox's GPL-2.0-only implementation and will be
renamed `nobox-agent-wire` only after B6 and the pre-separation Tier 0 readiness
package are complete. It is not extracted into or shared with the independent
Apache-2.0 product; future work follows
[`agent-seat-separation-roadmap.md`](agent-seat-separation-roadmap.md).

The interface is consumed by language models through agent harnesses. Its wire
format, images, schemas, errors, and tool descriptions are therefore optimized
for reliable model reasoning, not for direct human consumption. Source code,
security prompts, and maintainer documentation remain human-readable.

## End result

A granted agent can operate a dynamic application without reconstructing the
desktop or repeatedly guessing coordinates:

1. It takes one structured desktop snapshot and targets a client by its stable
   identity and generation.
2. Where an application exposes accessibility semantics, it requests a
   bounded, typed projection for that client and receives stable node handles,
   roles, names, states, relationships, and content-relative bounds.
3. Where semantics are absent or pixels are authoritative, it requests a
   bounded client capture with a coordinate overlay aligned to the exact
   coordinate system accepted by input tools.
4. It performs an input action with freshness preconditions and an optional
   bounded observation request. One result distinguishes manager-owned steps
   that committed, input events that were merely injected, events observed
   afterward, and pixels captured afterward. It never claims that application
   state changed unless that state was actually observed.
5. If a request is unusable, the result carries a stable code and structured
   correction data: the failing path, expected shape or bound, received kind,
   retryability, and any steps already committed. Model logic never needs to
   parse diagnostic prose.
6. Sequence numbers, client generations, accessibility-tree generations,
   action tokens, and explicit coordinate spaces let the model correlate every
   result without relying on titles, timing guesses, or conversation history.

The complete browser dogfood task that motivated this work should normally use
one snapshot, one semantic query or grounded capture, one action-and-observe
call, and at most one bounded follow-up. Unsupported applications fall back to
pixels without weakening grants or exposing global input.

## Goals

- Make the shortest reliable model workflow the natural tool workflow.
- Prefer compact typed facts over prose and pixels; use pixels when only pixels
  can answer the question.
- Make every coordinate-bearing result state its coordinate space and origin.
- Make errors directly correctable without interpreting a Rust, serde, MCP, or
  display-server diagnostic.
- Reduce tool round trips and repeated image context while preserving honest
  delivery semantics.
- Keep every request and response deterministic, bounded, and independently
  useful to a stateless MCP host.
- Preserve the existing deny-by-default grant, sensitivity, freshness,
  arbitration, indicator, and companion-is-not-authority invariants.
- Keep policy and identities display-server-neutral. X11 and a future Wayland
  backend implement the same protocol contract.
- Keep the manager event loop non-blocking. Optional helpers may fail without
  taking the manager or the base agent seat down.
- Keep Nobox's wire crate isolated from policy, display-server, toolkit, and
  MCP dependencies so its boundary remains reviewable after its planned
  `nobox-agent-wire` rename.

## Non-goals

- A human-oriented inspector, screenshot debugger, or accessibility browser.
- Attractive overlays or prose-first errors. Visual and textual output is
  judged by model grounding accuracy, compactness, and stability.
- Claiming that injected input was accepted. A delayed capture is an
  observation, not proof of causality or semantic success.
- Global pointer or keyboard input, screen-coordinate actions, or any route
  around the manager's client scope and human-preemption rules.
- Revealing hidden or redacted clients through accessibility, indirect pixels,
  error details, timing, node counts, or helper-process failures.
- Treating the existing `observe` bundle as permission to expose application
  text. Accessibility is a new privacy surface and requires an independently
  granted capability.
- A raw, unbounded AT-SPI tree dump. Queries and projections must have depth,
  node-count, text-length, and response-size bounds.
- Browser-specific DOM or debugging-protocol integration in the window
  manager. A later adapter may use the same neutral semantic-node contract.
- Semantic mutation in the first accessibility milestone. Observation and
  content-relative bounds come first; existing input remains the action path.
- OCR, visual element detection, workflow recording, macros, arbitrary sleeps,
  scripting, scheduling, or remote transport.
- Toolkit, D-Bus, or AT-SPI dependencies in `nobox-core` or the main `nobox`
  executable.
- Enriching malformed JSON that an MCP host rejects before invoking the
  companion. Structured correction begins once a request reaches this server.
- Wire compatibility with Agent Seat Protocol v1. Both peers already refuse
  versions they do not implement; v2 changes are explicit.

## Design rules

### Machine-native results

- Stable enums and numeric fields are load-bearing; prose is diagnostic only.
- Optional fields are omitted when absent. Repeated defaults and duplicated
  image data do not consume context.
- Lists have deterministic ordering and explicit truncation/resume metadata.
- Every observation says what object, generation, sequence, region, and
  coordinate space it represents.
- Compound results are factual transcripts: requested, committed, injected,
  observed, interrupted, and refused remain distinct states.

### Bounds and failure isolation

- All text, node lists, tree depth, capture area, overlay density, wait time,
  event count, and encoded response sizes have protocol constants.
- No manager request blocks on D-Bus, an application, an accessibility helper,
  image settling, or an MCP client.
- Delayed observations are manager-owned pending operations with deadlines;
  they yield back to the event loop between samples and are cancelled on human
  preemption, revocation, disconnect, or target loss.
- Accessibility runs in a separate optional helper. Its crash or absence makes
  semantic tools unavailable and leaves snapshots, management, input, and
  capture intact.

### Security and capability shape

- The manager authorizes every semantic query before asking a helper and
  filters every returned node before replying.
- Accessibility application text is never added to `observe.structure` or
  `observe.titles`. A new atom such as `observe.accessibility` is separately
  granted and presented in consent in concrete privacy terms.
- The helper has no grant authority. It receives only the bounded target and
  query authorized for one request; the manager revalidates the client and
  generation when the result returns.
- Node handles are session-scoped, generation-stamped opaque identifiers, not
  toolkit pointers, D-Bus paths, X11 IDs, or process IDs.
- Sensitive and out-of-scope clients produce the same answer as absent clients
  before any helper lookup.

## Architecture

- Nobox's in-tree wire crate (currently `agent-seat-proto`, planned as
  `nobox-agent-wire`) owns v2 wire types, bounds, coordinate-space metadata,
  semantic projections, structured correction fields, and compound-result
  transcripts. It contains no X11, Wayland, D-Bus, AT-SPI, MCP, or nobox type.
- `nobox-core` owns grants, client scope, freshness, sequence/generation policy,
  semantic-query authorization, and pending-operation policy. It never parses
  toolkit data.
- `nobox-x11` maps neutral client identities to X11 resources, renders capture
  overlays, and coordinates non-blocking helper requests with the event loop.
- The optional `agent-semantic-helper` owns AT-SPI/D-Bus translation, a small
  isolated async reactor, and strict response bounds. It has no authority and
  no toolkit dependency leaks into the manager. It is a disposable process,
  not a persistent desktop index.
- `nobox-agent` exposes compact MCP tools and JSON Schemas, translates MCP
  correction failures to typed data, and removes image bytes from textual and
  structured duplication. It does not add policy or infer success.

## Milestones

### B1: capture grounding — done

Add an opt-in coordinate grid to `client_capture`.

- Request: a typed grid object with bounded pixel spacing.
- Rendering: high-contrast lines and compact numeric labels aligned to the
  capture area's content coordinates, including cropped captures whose origin
  is not zero.
- Reply: exact applied spacing and coordinate origin beside the image.
- The original pixels remain the default when no grid is requested.
- Output capture is unchanged; this milestone assists window-addressed input,
  not global-coordinate reasoning.
- Unit tests cover bounds, negative/off-grid origins, clipping, tiny images,
  deterministic raster output, and serde rejection of unknown fields.
- Nested-X/MCP coverage proves the overlay arrives as an image block while its
  grounding metadata remains structured and base64 is not duplicated.

Exit: a model can read a point from a full or cropped client capture and pass
that point directly to `client_pointer` without counting pixels from an
unstated origin.

### B2: correctable errors — done

Define one compact correction shape for MCP-boundary and seat-boundary errors.

- Fields include `code`, `path`, `expected`, `received`, `retryable`,
  `current_generation`, and `committed` where applicable.
- MCP argument parsers report JSON paths and expected shapes or numeric bounds.
- Protocol validation reports typed argument locations without exposing hidden
  object information.
- Retryability is an enum or boolean defined by the producer, never inferred
  from prose.
- Existing error codes retain their privacy equivalences.

Exit: tests can repair every supported invalid tool argument using structured
content alone. Diagnostic text may be removed without changing test logic.

Implemented contract: MCP `-32602` responses carry the same protocol error in
`error.data` that seat refusals carry in tool `structuredContent`. `path` is an
RFC 6901 JSON Pointer relative to the tool argument object (the empty string is
its root). `expected` is a typed constraint, `received` is a JSON kind, and
`retryable` names the prerequisite for another attempt. Unknown fields use
`expected.kind = "absent"`, so removal is a first-class correction.

### B3: action and bounded observation — done

Add an optional observation block to window-addressed input calls.

- The block can request a bounded event window and an optional post-action
  client capture region, including one from a separately named stable client.
- Injection still replies `delivery: unverified`; returned captures and events
  are stamped observations, not a claim that the action caused them.
- The manager schedules rather than sleeps, continues processing the human
  seat, and reports partial progress exactly on interruption.
- An action token correlates injection, resulting events, observation samples,
  and the final response.
- Capture authorization and sensitive-client checks are re-evaluated at the
  time pixels are read.

Exit: the common click-then-check workflow takes one call while preserving all
existing arbitration and honesty guarantees.

Implemented contract: `client_pointer`, `client_key`, and `client_type` accept
an optional strict `observe` object containing optional `capture`, `minimum_ms`,
`quiet_ms`, and `maximum_ms`. The maximum is 5 seconds and the correlated event
slice is capped at 64 envelopes. Only one observed action may be pending per
session. The manager assigns a session-local `action`, schedules completion in
its existing runtime timer, and never sleeps in the event loop. Completion is
the later of the minimum and last-correlated-event-plus-quiet deadlines, capped
by the maximum. The result keeps `delivery: unverified`, returns the observed
sequence interval, elapsed time, bounded event slice, dropped count, and zero
or one final capture attempt. Capture failure is a structured observation
sample and does not retract successful injection.

Correlation is temporal: target-client events plus focus and workspace changes
during the window are included, but are never labeled as effects of the input.
The capture grant, scope, visibility, sensitive overlap, and live capture target
are checked again when pixels are read. Human activity interrupts immediately;
closing the input target remains a correlated event and makes only a capture of
that client fail. Freeze, revocation, disconnect, and timer failure terminate
the pending request.
Errors after injection carry both the committed steps and the action identity.
MCP lifts the final PNG into one image content block and removes its base64 from
structured and textual duplication.

### B4: accessibility discovery and threat model — done

Before exposing a semantic tool, prove the Linux integration boundary.

- Prototype bounded AT-SPI discovery in a disposable helper.
- Establish reliable client-to-accessible-root correlation without putting
  X11 identities on the neutral wire.
- Measure Firefox/Zen, Chromium/Electron, GTK, and Qt behavior, including
  missing, stale, duplicated, and cross-process trees.
- Specify helper sandboxing, lifecycle, timeouts, cancellation, sensitivity
  filtering, grant presentation, and failure behavior.
- Record why the chosen mapping cannot reveal an out-of-scope client.

Exit: a reviewed design and nested-session fixtures demonstrate correct target
mapping and safe failure. If mapping cannot satisfy the privacy invariant, the
semantic milestone remains blocked rather than shipping a best-effort leak.

Implemented proof: [`agent-accessibility-discovery.md`](agent-accessibility-discovery.md)
defines a restricted local-X11 mapping based on X-Resource's server-supplied
PID, the accessibility bus connection PID, exact geometry or a complete
one-client/one-root equal-size bijection, and mandatory post-helper
revalidation. A disposable strict-JSON probe reads no accessible strings and
returns no candidate identity. Deterministic fixtures cover missing, stale,
duplicated, cross-process, unrelated, partial, and malformed inputs; a private
D-Bus plus nested-X test exercises real GTK and Qt bridges under nobox.
Ambiguity, missing X-Resource identity, helper failure, or an unsupported
runtime is semantic-unavailable and falls back to capture. B5 is approved only
inside this restriction and still must add a sandboxed Rust helper plus real
Chromium/Electron coverage before those families are advertised as tested.

### B5: bounded semantic observation — done

Expose the neutral accessibility projection under its own capability.

Contract slice implemented: the v2 wire defines the independent
`observe.accessibility` atom and `accessibility` consent bundle; strict
`client.semantic_root`, `client.semantic_tree`, and `client.semantic_find`
calls; portable roles and states; generation-stamped opaque node handles;
bounded content-relative node projections; deterministic continuation tokens;
and `stale_tree` with the current tree generation. Existing `observe` grants
do not expand.

Root-correlation slice implemented: CMake builds and installs the optional
Rust helper by default. The X11 backend obtains only the server-supplied
X-Resource 1.2 local PID and never consults `_NET_WM_PID`. The helper accepts a
strict bounded request, enumerates only matching AT-SPI application roots and
direct top levels, reads no accessible strings, and applies resource limits,
no-new-privileges, per-call/total deadlines, and a post-connect seccomp
allowlist. Rust fixtures and nested GTK/Qt sessions cover exact, process-family,
complete-bijection, ambiguous, unavailable, malformed, and over-limit shapes.
Manager lifecycle slice implemented: authorized requests now enter a dedicated
worker that owns at most one disposable helper globally. The X11 event loop
continues serving requests; helper completion only wakes it through the
existing control channel. A manager timer releases ordinary outcomes at a
fixed 1.2-second boundary, kills overdue work, and rechecks the grant,
visibility, generation, and X-Resource PID. Human activity, target change,
freeze, revocation, disconnect, and shutdown cancel or discard pending output.
A nested-X regression proves a snapshot completes while discovery is pending.

Root-projection slice implemented: after unique correlation the helper reads
only the matched root's bounded name, portable role/states, extents, and child
count. The manager converts that to a one-node `SemanticTreePage`, issues a
session/client-scoped tree generation, and repeats authorization, visibility,
descriptor-generation, and normalized X-Resource owner checks before reply.

Tree-projection slice implemented: `client.semantic_tree` performs bounded,
deterministic breadth-first paging from the current root or a generation-scoped
node handle. Each disposable helper invocation inspects at most 4,096 objects,
descends at most 16 levels, returns at most 128 nodes, rechecks the verified
D-Bus owner PID for every visited object, and rejects internal identity
collisions. The manager validates page arithmetic and shape, remaps helper
identities to monotonic session/client/tree-local handles, retains at most 16
opaque continuations, and returns typed `stale_tree` after a root refresh or
identity change. Continuations own their original root, offset, and depth, so a
later caller cannot alter an in-progress traversal.

The MCP companion requests the separate accessibility bundle and advertises
`client_semantic_root` and `client_semantic_tree` with compact bounded schemas.
GTK and Qt nested tests exercise root discovery, multi-page traversal, and
stale-handle rejection across the complete helper-manager-wire path.

Constrained-search slice implemented: `client.semantic_find` searches the
correlated root in deterministic breadth-first order using an optional bounded
case-insensitive accessible-name substring, a role OR-set, and a state AND-set,
with at least one predicate required. Filtering occurs inside the disposable
helper so nonmatches do not inflate the response; each invocation inspects at
most 4,096 nodes through depth 16 and returns at most 128 matches. The manager
re-evaluates the predicate, remaps only returned identities, and stores the
original predicate in an opaque continuation. The MCP companion advertises
`client_semantic_find` with exact role/state enums and a default 16-result page.
GTK and Qt nested tests prove a live root-role query through the complete path.

Browser exit slice implemented: an optional nested-X regression launches a
real Firefox-family browser with a private profile and checked-in local HTML
fixture. It searches for `Nobox demo video`, deterministically selects the
single focusable bounded media node from the name matches, and derives a
content-relative center point without capture or iterative grid measurement.
Zen exposes that HTML media node as `GROUP`, so the fixture records portable
name/state/bounds behavior instead of asserting a backend-specific `VIDEO`
role. The path passed five consecutive runs in the measured environment.

- Tools support root summary, bounded subtree projection, and constrained
  search by role/name/state with explicit result limits.
- Nodes carry opaque handles, tree generation, role, bounded name/value text,
  states, relationships needed for interpretation, and content-relative
  bounds when available.
- Pagination or continuation is deterministic; stale handles return a typed
  stale-tree response and current generation.
- Events invalidate or advance tree generations without flooding the desktop
  event stream with raw toolkit events.
- Pixel capture remains the oracle for rendering and unsupported canvases.

Exit met: the browser dogfood task identifies a requested video semantically
and derives a content-relative click point without iterative grid measurement.

### B6: hardening and dogfood

In progress: the real Firefox-family fixture is repeatable and part of CTest
when Zen or Firefox is installed. Chromium and three live Electron applications
(Claude Desktop, Beeper, and Devin) are measured safe-unavailable cases in this
environment; neither family is advertised as tested semantic support without a
repeatable isolated fixture.

The launch-policy picker prerequisite shipped as N0 in
[`agent-seat-separation-roadmap.md`](agent-seat-separation-roadmap.md). B6 can
now close once its remaining hardening, measurements, full verification, and
source-release requirements are met.

Failure-lifecycle slice implemented: the live agent-seat regression drives
helper crash, truncated JSON, stdout beyond the hard response cap, recovery by
a fresh helper, human cancellation, companion disconnect, session freeze, and
live grant revocation. Ordinary helper failures and human cancellation retain
the fixed generic unavailable result; freeze and revocation retain their typed
session errors. Every path leaves the manager and the next semantic request
usable.

Writing-hardening slice implemented from an Antigravity/WPS dogfood trace: the
old backend queued each complete `client.type` string as one unpaced XTEST
burst, and WPS visibly repeated held letters while dropping intervening text.
Text is now a paced pending operation with one complete character stroke per
event-loop boundary, immediate flushes, human preemption between characters,
and exact partial-commit reporting. The MCP contract tells models to send one
coherent multiline passage with embedded newlines instead of issuing a
separate Return call for every line break. Large-image fallback guidance now
uses a labeled coarse grid followed by a smaller grounded crop, because the
same trace guessed repeatedly despite receiving correct full-window grid
metadata. WPS remains a measured semantic-unavailable fallback rather than a
reason to weaken accessibility correlation.

Geometry/measurement slice implemented for every supported runtime: the real
browser probe takes one grounded content capture after semantic selection,
reads only its typed extent and byte counts, and proves the media bounds plus
derived center are contained without interpreting pixels. Three consecutive
combined runs passed. One sample encoded root plus search in 705 JSON bytes at
the fixed 2,401 ms semantic deadline, versus 46,824 capture-image JSON bytes
containing a
34,994-byte PNG in 347 ms. The GTK/Qt regression now performs the same bounded
root-plus-search and grounded-capture comparison after paging and stale-tree
checks. One run encoded 532/536 semantic JSON bytes versus 74,997/7,317 capture
JSON bytes containing 56,124/5,363 PNG bytes; positionless GTK and Qt roots
normalized to content-relative `(0,0)`. Live Electron requests retained only
the typed unavailable result and deadline, never application text. Broader
scaling, multiple-output, and responsive-reflow measurements remain B6 work.

- Compare semantic bounds to captures across scaling, decorations, workspaces,
  multiple outputs, and responsive reflow.
- Exercise revocation, hidden/redacted clients, helper crash/restart, stale
  trees, response truncation, human preemption, and companion disconnect.
- Measure calls, image bytes, structured bytes, and elapsed time for repeated
  browser, GTK, Qt, Electron, and canvas-only tasks.
- Update the protocol, harness guide, installed agent guide, acceptance tests,
  and source-release notes.

Exit: supported semantic tasks meet the target workflow above; unsupported
tasks degrade to grounded capture; the full prescribed build and nested-X test
suite passes; a proper source release documents v1 incompatibility.

## Overall definition of done

- No tool requires prose parsing for control flow.
- No coordinate is ambiguous between root, output, frame, content, crop-local,
  or image-local space.
- No response duplicates large pixel or tree payloads for compatibility.
- No new privacy surface is silently included in an old grant.
- No helper, delayed observation, or slow agent can block the window manager.
- Every new bound, state transition, error shape, and privacy equivalence has a
  focused unit test and relevant nested-X regression coverage.
- The browser feedback scenario is recorded as a repeatable dogfood fixture,
  with before/after round-trip and payload measurements.
