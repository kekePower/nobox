# Agent interface v2 roadmap

Status: implementation started. This plan follows the completed X11 baseline
in `agent-roadmap.md`; it does not reopen or weaken that baseline.

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
- Keep `agent-seat-proto` extractable and dependent on serde alone.

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

- `agent-seat-proto` owns v2 wire types, bounds, coordinate-space metadata,
  semantic projections, structured correction fields, and compound-result
  transcripts. It contains no X11, Wayland, D-Bus, AT-SPI, MCP, or nobox type.
- `nobox-core` owns grants, client scope, freshness, sequence/generation policy,
  semantic-query authorization, and pending-operation policy. It never parses
  toolkit data.
- `nobox-x11` maps neutral client identities to X11 resources, renders capture
  overlays, and coordinates non-blocking helper requests with the event loop.
- A future optional accessibility helper owns AT-SPI/D-Bus translation and
  strict response bounds. It has no authority and no toolkit dependency leaks
  into the manager.
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

### B2: correctable errors

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

### B3: action and bounded observation

Add an optional observation block to window-addressed input calls.

- The block can request a post-action client capture region and a bounded
  observation policy with explicit minimum, quiet, and maximum durations.
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

### B4: accessibility discovery and threat model

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

### B5: bounded semantic observation

Expose the neutral accessibility projection under its own capability.

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

Exit: the browser dogfood task identifies a requested video semantically and
derives a content-relative click point without iterative grid measurement.

### B6: hardening and dogfood

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
