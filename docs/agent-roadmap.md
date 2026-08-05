# Agent protocol roadmap

This is the implementation plan for `docs/agent-protocol.md` (Tier 1, X11
backend). The protocol document is the contract; this document is the order
of work, the crate boundaries, and the definition of done. Checkboxes are
updated as milestones land.

## End result

A stock MCP-capable agent harness, configured with the `nobox-agent` command
and no nobox-specific glue, can drive a live nobox session:

1. Connect; be denied by default; be granted `observe` + `manage` + `input` +
   `launch` scoped to a test application, via config or the consent dialog.
2. Call `subscribe_and_snapshot` and hold a consistent world model from
   sequence numbers alone.
3. `launch` a desktop entry, receive `client_mapped` carrying its correlation
   token, and learn the new client's identity without capture.
4. `client.activate` a background client on another workspace; nobox performs
   the workspace/raise/focus transition through the core `Focus` contract.
5. Perform a window-relative `client.pointer` click with an `expects` block;
   observe a `stale_state` rejection after the window is mutated, re-observe,
   and succeed.
6. `client.capture` the window and receive pixels stamped with geometry and
   sequence.
7. Be interrupted: human input during an agent sequence returns `interrupted`
   naming the committed steps, and the kill chord freezes the session while
   the WM stays fully responsive.

Every action appears in structured tracing with the session identity. The
hidden-client rule, indicator rendering, and companion-death isolation hold
throughout. The whole flow above runs as one nested-X smoke test in CI, and
the same flow is dogfooded against a real harness on a live desktop.

## Goals

- Implement Tier 1 exactly as specified in `docs/agent-protocol.md`,
  including every security invariant listed there.
- Keep the wire protocol in an extraction-ready crate with no nobox
  dependencies, under a neutral protocol name.
- Dogfood continuously: a minimal MCP companion ships in the first functional
  milestone and grows with each subsequent one.
- Preserve nobox's existing discipline: no unsafe Rust, bounded everything,
  policy in core, protocol I/O at the backend boundary, companion failure
  outside the manager's failure boundary.

## Non-goals (v1)

- Tier 0 (the standalone EWMH server). It is sequenced after Tier 1 proves
  the tool surface, and is not part of this plan.
- Wayland enforcement. The contract is designed for it; no compositor work
  happens here.
- AT-SPI / in-application widget trees.
- A grants UI in `nobox-settings` (TOML editing and the consent dialog are
  the v1 interfaces).
- Any transport other than a local UNIX socket; any remote access.
- Global-coordinate input tools, in any form.
- Recording/replay, scripting, or scheduling features in the companion.

## Crate and boundary layout

- **`agent-seat-proto`** (new, library): the wire protocol. Request,
  response, and event types; capability atoms; error codes; protocol version;
  frame encoding (length-prefixed JSON, per-message-type size bounds).
  Depends on `serde`/`serde_json` only — never on `nobox-core`. This crate is
  the future standard artifact and must remain extractable by `git mv`. The
  protocol's neutral name, `agent-seat`, lives here, in the `_AGENT_SEAT` root
  property, and in the spec; "nobox" appears only in implementation crates.
- **`nobox-agent`** (new, binary): the MCP companion. A blocking JSON-RPC 2.0
  stdio server (serde_json; no async runtime) translating MCP tools to
  protocol frames on the WM socket. Depends on `agent-seat-proto` alone. It
  is a reference client for any WM that implements the socket, and it
  enforces nothing. It targets MCP revision 2026-07-28 (stateless): it
  implements `server/discover`, validates the per-request `_meta` protocol
  fields (`protocolVersion`, `clientCapabilities`) and rejects mismatches
  with the spec's error codes, stamps `resultType` on results, returns
  deterministic `tools/list` output with the required `ttlMs`/`cacheScope`
  fields, logs to stderr, and adopts none of the deprecated features
  (sampling, roots, MCP logging).
- **`nobox-core`**: a new `agent` policy module — session and grant model,
  capability evaluation, application-scope filtering, `agent_visibility`
  filtering, snapshot assembly, per-client generation counters, global
  sequence bookkeeping, event coalescing policy, freshness-precondition
  evaluation. Pure, deterministic, unit-tested. No I/O, no protocol frames.
- **`nobox-config`**: the `[agent]` schema — `enabled` (default false),
  socket path override, default policy, persisted grants (verified-identity
  binding, capability atoms, scopes), launch policy (allow/deny desktop IDs,
  user-entry switch), suppression window, kill chord binding — plus
  `agent_visibility` on application rules. Strict validation; unknown keys
  fail, consistent with the rest of the schema.
- **`nobox-x11`**: realization. Socket listener and per-session I/O threads;
  typed control-event wakeups into the blocking loop (the existing
  `_NOBOX_CONTROL` support-window pattern); request queue drained at event
  boundaries; peer-credential collection; XTEST injection with provenance
  tracking; capture; indicators; consent dialog; the `_NOBOX_AGENT`-style
  root property advertising protocol version and socket path.

Threading follows the established shape: I/O threads only move bounded bytes
and wake the loop; every decision happens on the event loop against core
policy. Session writer channels are bounded and non-blocking from the loop's
perspective; a full channel disconnects that session, never stalls the WM.

## Milestones

### A0: wire crate, socket, handshake, deny-by-default — done

- [x] `agent-seat-proto` with versioned hello, typed requests/responses/
      events, capability atoms, structured error codes, frame codec with
      per-type size bounds; serde round-trip and unknown-field rejection
      tests.
- [x] `[agent]` config schema, disabled by default, with validation tests.
      Stored grants bind to an absolute executable path; a declared harness
      name is not a matching key anywhere in the schema. The suppression
      window and kill chord are deliberately absent until A4 implements them,
      so configuration never promises behavior that does not exist.
- [x] Listener at `$XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock` (0700
      directory, 0600 socket), created only when `[agent].enabled`.
- [x] Per-session reader/writer threads, bounded queues, loop wakeup via a
      new typed control code, coalesced so a flood produces one wakeup per
      drain; peer credentials (UID/GID/PID via `SO_PEERCRED`, executable and
      bounded parent chain from `/proc`) captured at accept time.
- [x] `_AGENT_SEAT` root property advertising protocol name, version, and
      socket path; withdrawn on clean shutdown with the socket.
- [x] Handshake completes, and every capability request is denied with a
      structured error when no grant exists. A granted-but-unimplemented tool
      answers `unsupported`, which is deliberately distinct from `denied`.
- Exit: `tests/x11-agent-seat.sh` drives a real socket client
  (`agent-seat-proto`'s `agent-seat-probe` example) through grant issuance,
  deny-by-default from an unnamed executable, version mismatch, out-of-order
  and repeated handshakes, oversized frames, malformed frames, abandonment
  mid-frame, and a request flood, then proves the manager still manages
  windows and still advertises its seat.

### A1: sessions, grants, observe — first dogfood — done

- [x] Core `agent` module: session/grant model, scope filter, hidden/redacted
      filtering, snapshot assembly, generation counters. `nobox-core` takes
      `agent-seat-proto` as a dependency so policy and protocol share one
      vocabulary; the crate stays free of display-server types.
- [x] Config-declared grants (consent dialog comes in A6; default policy
      `deny` until then).
- [x] `desktop.snapshot` and `client.get` end to end, stamped with sequence.
- [x] Session-identity attribution on every request in structured tracing:
      session, declared harness, and the verified uid/pid behind the socket.
- [x] Minimal `nobox-agent` MCP companion: `server/discover`, per-request
      `_meta` validation, compliant `tools/list`, snapshot/get tools.
      Dogfooding starts here.
- Exit: core unit tests prove hidden ≡ nonexistent (responses *and* errors),
  scope filtering, generation bumps; the nested-X test snapshots a desktop
  containing a rule-hidden client, never sees it, and proves every withheld
  window answers byte-for-byte as a window that never existed. The same test
  drives the real companion through discovery, a deterministic tool list, a
  live snapshot, and the two malformed requests the revision requires servers
  to reject.

### A2: event stream

- [ ] Atomic `subscribe_and_snapshot` at a single event-loop boundary.
- [ ] Monotonic sequence stamping; per-session bounded queue with
      `resync_required` on overflow.
- [ ] Event kinds: `client_mapped`, `client_closed`, `title_changed`,
      `focus_changed`, `state_changed`, `geometry_changed` (coalesced),
      `workspace_switched`, session-control events.
- [ ] Kind filters at subscribe time; grant scope applied to events.
- [ ] Companion exposes the stream as cursor-based retrieval: an event
      long-poll tool taking `after_seq` (+ bounded wait), because the
      sequence number is precisely the explicit cross-request identifier
      stateless MCP requires. Delivery over `subscriptions/listen` is an
      optional addition for hosts that opt in, never the only path.
- Exit: nested-X race test (map/close storms around subscription) shows no
  missed or duplicated events; overflow test forces and recovers via
  re-snapshot; scoped session never receives out-of-scope events.

### A3: manage and freshness

- [ ] `client.activate` (core `Focus` contract, agent as first-class
      activation source), `client.close` (ICCCM path only),
      `client.move_resize`, `client.set_state`, `client.send_to_workspace`,
      `workspace.switch` — all mapped onto existing action paths, no new
      state machinery.
- [ ] `expects` preconditions (generation, geometry, workspace, focus) with
      `stale_state` errors carrying current generation.
- [ ] Companion grows the manage tools.
- Exit: nested-X tests for cross-workspace activation, negotiated close, and
  a mutated-then-rejected `expects` flow; core unit tests for precondition
  evaluation.

### A4: input, arbitration, kill chord, indicators

- [ ] Window-addressed XTEST injection translated against live geometry at
      injection time; refusal when the target vanished.
- [ ] Injection provenance: the manager tracks its own synthesized input and
      never counts it as human activity.
- [ ] Human suppression window (configurable) returning structured
      `interrupted`; `human_activity` events.
- [ ] `ensure_visible` as one serialized activate-raise-inject operation with
      exact committed-step reporting under preemption.
- [ ] Kill chord processed ahead of all agent traffic; freeze/resume/revoke
      lifecycle and events.
- [ ] WM-rendered indicators: persistent marker while `input`/`capture` is
      held; frame tint or cursor badge on the client receiving agent input.
- Exit: nested-X tests prove suppression does not self-trigger on injected
  events, mid-sequence interruption names committed steps, and the kill
  chord freezes sessions under agent input flood while the WM keeps
  processing human input.

### A5: capture

- [ ] `client.capture` for visible clients; obscured capture behind
      XComposite as a separately advertised capability, absent when the
      extension is unavailable.
- [ ] `output.capture`, denied (or region-masked where compositing allows)
      while any hidden/redacted client is visible on that output.
- [ ] Image encoding decision (a minimal PNG dependency) recorded and
      bounded; capture responses use the large frame bound.
- Exit: nested-X tests capture an obscured window under composite
  redirection, verify pixels/geometry/sequence stamping, and prove the
  hidden-client output denial.

### A6: launch, consent dialog, grant persistence

- [ ] `launch` through the `nobox-desktop` catalog with startup-notification
      correlation tokens surfacing in `client_mapped`; launch policy
      (allow/deny IDs, user-entry switch) enforced.
- [ ] WM-drawn consent dialog reusing the existing override-redirect surface
      machinery; one-shot and persisted answers; persisted grants bound to
      verified peer identity through the `nobox-config` document API.
- [ ] Live grant revocation on config reload.
- Exit: nested-X launch test with fixture desktop entries and correlation
  assertions; consent flow test covering grant, deny, persist, and reload
  revocation.

### A7: end-to-end smoke, hardening, docs

- [ ] The full end-result flow above as one nested-X CI smoke test driving
      the real `nobox-agent` binary over MCP.
- [ ] Adversarial companion tests: malformed frames, protocol-version
      mismatch, capability probing for hidden clients, slow-consumer
      disconnect.
- [ ] `docs/agent-protocol.md` status updated from design to implemented,
      with any contract corrections discovered during dogfooding folded back
      into the spec first, code second.
- [ ] `docs/usage.md` and `docs/configuration.md` cover `[agent]`, the kill
      chord, and harness setup; a short harness-setup example ships with
      `nobox-agent`.
- Exit: the smoke test is green in CI, and one real multi-step agent task has
  been performed end to end on a live desktop through a real harness.

## Standing rules for every milestone

- Spec first: when dogfooding contradicts `docs/agent-protocol.md`, the spec
  is amended in the same change that alters behavior. The documents never
  drift apart.
- Every milestone lands with its unit and nested-X coverage per the spec's
  verification boundary, passes `cmake --build --preset check` and
  `/usr/bin/ctest --preset dev`, and is committed and pushed as one verified
  step.
- The protocol version is pre-1.0 and may break at any milestone; the
  companion and WM always refuse mismatched versions rather than guessing.
- The MCP target is revision 2026-07-28 and is pinned: statelessness rules
  are honored (no session inferred from the stdio process; all cross-request
  state referenced by explicit identifiers), and deprecated MCP features are
  never adopted. Once Tier 1 is proven, proposing the surface as a
  vendor-prefixed MCP extension (negotiated via the `extensions` capability
  field) is the preferred standardization route on the MCP side.
- New dependencies require explicit justification. Anticipated: a peer-
  credential/pidfd crate (`rustix` or equivalent) in A0, `serde_json` for the
  companion in A1, a minimal PNG encoder in A5. No async runtime anywhere.

## Open decisions (resolve in the named milestone)

- A0 (resolved): the protocol is `agent-seat`, in crate `agent-seat-proto`,
  advertised as `_AGENT_SEAT`. Frame bounds are per message type: 8 KiB
  handshake, 64 KiB request, 4 MiB response, 32 MiB capture, 256 KiB event.
  Sockets are per display at
  `$XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock`, with an absolute-path
  override bounded by the platform's `sockaddr_un` limit.
- A2: whether `subscriptions/listen` delivery ships alongside the long-poll
  tool in v1 or is deferred until a host demonstrably uses it.
- A4: provenance mechanism details for XTEST round-trips (tagging strategy
  and its test), suppression-window default.
- A5: PNG dependency choice; whether output capture masks or denies under a
  running external compositor.
- A6: consent-dialog interaction model (keyboard-only is acceptable for v1).
