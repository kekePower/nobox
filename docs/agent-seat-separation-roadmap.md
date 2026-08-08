# Agent Seat product separation and Tier 0 roadmap

Status: planned. This document is the authoritative plan for agent work after
the in-progress B6 hardening milestone in
[`agent-interface-v2.md`](agent-interface-v2.md). It supersedes the earlier
idea that Nobox's current `agent-seat-proto` crate would be extracted or become
the shared implementation for other products.

The implemented Nobox contract remains specified in
[`agent-protocol.md`](agent-protocol.md). The completed v1 history remains in
[`agent-roadmap.md`](agent-roadmap.md). This document governs future product
separation, the Nobox settings work, coordinate-efficiency work, and the
independent Tier 0 X11 product.

## Decision summary

- Nobox keeps its existing GPL-2.0-only Agent Seat implementation and its full
  Tier 1 window-manager integration.
- The current in-tree `agent-seat-proto` crate is not extracted, relicensed,
  published as the independent product, or made a dependency shared with that
  product. It will be renamed to `nobox-agent-wire` after the current v2
  hardening milestone closes.
- A separate product and repository named `agent-seat-proto` will be created
  under the Apache License 2.0 (`Apache-2.0`, sometimes called ASL-2.0). It has
  its own source, history, governance, releases, and development path. Its
  canonical GitHub repository is created directly as
  `ZaguanLabs/agent-seat-proto` under the ZaguanLabs organization.
- The two products may implement the same public technology and compatible
  wire revisions. They share ideas, observable behavior, and public protocol
  concepts, not implementation source.
- Nobox may independently implement useful ideas developed by
  `agent-seat-proto`. No source file, patch, commit, generated source, or test
  implementation is copied between the Apache-2.0 and GPL-2.0-only products.
- Tier 0 belongs to the independent `agent-seat-proto` product. It is a
  standalone X11 provider for Openbox and other sufficiently EWMH-compliant
  window managers. Nobox continues to provide the integrated Tier 1 seat.
- Nobox Settings will expose the launch policy that already exists in
  `[agent.launch]`, using Nobox's existing bounded XDG catalog.
- Nobox will not learn or persist frequently used pointer coordinates.
  Semantic targets and current grounded captures remain the supported ways to
  avoid repeated screenshot reasoning.

## End results

### Nobox

A Nobox user can enable the integrated seat, grant a companion only the
capabilities intended, select exactly which installed applications agents may
launch from Nobox Settings, and operate supported applications through compact
semantic targets or grounded captures. The manager remains the authority for
every request and retains its existing Tier 1 guarantees: live scope checks,
hidden-client equivalence, freshness, human preemption, indicators, bounded
observation, and failure isolation.

Nobox's wire-format crate has a Nobox-specific implementation name and remains
part of the GPL-2.0-only Nobox product. No build of Nobox fetches, links,
vendors, or generates code from the independent Apache-2.0 product.

### Independent `agent-seat-proto` product

The independent project publishes an Apache-2.0 protocol implementation,
specification, MCP companion, and standalone X11 provider. It can be adopted by
companies and other projects without depending on Nobox or accepting Nobox's
license. Contributions intentionally submitted to that repository use the
project's Apache-2.0 inbound terms and documented provenance process.
ZaguanLabs is the GitHub repository owner and administrative home; forks and
package publications identify that canonical upstream explicitly.

On a stock Openbox session, a user can start the Tier 0 provider from
`autostart`, inspect its status, connect a generic MCP harness, observe EWMH
desktop state, perform the supported EWMH management operations, and launch
only applications allowed by the standalone provider's own configuration.
Later opt-in X11 capture and input profiles may add best-effort interactive
automation, but they never claim the stronger guarantees of an integrated
window manager.

### Interoperability

Each provider advertises its protocol name, exact wire revision, socket, and
backend feature set. A companion refuses unknown revisions rather than
guessing. When Nobox and the independent product claim the same wire revision,
black-box conformance verifies their shared observable contract. Either
product may advance independently by advertising a new revision or extension;
compatibility is explicit, never inferred from similar type or tool names.

## Goals

- Preserve Nobox's complete integrated Agent Seat behavior and security
  invariants while removing the expectation that its implementation will be
  extracted.
- Establish an unambiguous license and provenance boundary between the
  GPL-2.0-only Nobox sources and the Apache-2.0 independent product.
- Make the existing Nobox application-launch policy understandable and usable
  without hand-editing TOML.
- Give Openbox and other EWMH window managers a useful standalone Tier 0 seat
  without requiring changes to those window managers.
- Describe every Tier 0 limitation in terms a user and an agent can act on.
- Keep every request, response, queue, scan, capture, string, and timeout
  bounded in both implementations.
- Prefer typed, current semantic facts over pixels, and pixels over guessed or
  historical coordinates.
- Keep desktop-server identities behind the provider boundary. X11 window IDs,
  atoms, D-Bus paths, and toolkit object IDs never become public Agent Seat
  identities.
- Test compatibility at process and wire boundaries instead of sharing source.
- Release and version Nobox and `agent-seat-proto` independently.

## Non-goals

- Relicensing, moving, copying, or publishing Nobox's current
  `agent-seat-proto` source as the independent product.
- A Cargo path, Git, workspace, submodule, generated-code, or vendored-source
  dependency between Nobox and the independent product.
- Automatic synchronization or cherry-picking between the two repositories.
- Making Tier 0 indistinguishable from Tier 1. A standalone X11 client cannot
  honestly reproduce WM-owned consent, atomic event-loop decisions, secure
  indicators, authoritative client generations, or compositor enforcement.
- Requiring Openbox, tint2, or another existing WM/panel to implement Agent
  Seat support.
- Coupling Openbox or standalone-provider failure to Nobox.
- Remote access, network listening, multi-user sockets, or a cloud relay.
- Arbitrary shell-command execution. Launch remains desktop-entry based and
  policy controlled.
- Persisting click coordinates, screenshots, accessibility trees, page
  contents, workflow recordings, macros, or learned interaction history.
- Browser-specific DOM, debugging-protocol, URL, account, or service logic in
  a window manager or Tier 0 provider.
- Claiming that injected input was accepted or that a later observation was
  caused by the injection.
- Making either product wait for the other's release or accept its unreviewed
  behavior as authoritative.

## Product and ownership boundaries

| Concern | Nobox | Independent `agent-seat-proto` |
| --- | --- | --- |
| License | GPL-2.0-only | Apache-2.0 |
| Repository and history | Nobox repository | `ZaguanLabs/agent-seat-proto` from its first commit |
| Wire implementation | `nobox-agent-wire` after rename | Independently authored `agent-seat-proto` crate |
| Policy authority | `nobox-core` and `nobox-config` | Standalone provider policy crate/process |
| X11 realization | Integrated `nobox-x11` Tier 1 backend | Standalone Tier 0 X11 provider |
| MCP translation | `nobox-agent` | Independent generic companion |
| XDG catalog | `nobox-desktop` | Independently authored catalog implementation |
| Settings | `nobox-settings` and Nobox TOML | Independent configuration and later optional UI |
| Accessibility | Nobox's disposable helper and Tier 1 checks | Out of Tier 0 baseline; independently designed later |
| Releases | Nobox tags and source releases | Independent tags and source releases |

`nobox-agent-wire` is the working final name for the in-tree crate. The public
wire string may remain `agent-seat`, because an implementation name and an
interoperability identifier answer different questions. The rename must not
change framing, serialized names, protocol version, socket behavior, or tool
behavior.

The independent repository may contain several crates while remaining one
product:

```text
agent-seat-proto/
├── crates/agent-seat-proto   # public wire types and bounded codec
├── crates/agent-seat-mcp     # generic MCP translator
├── crates/agent-seat-x11     # standalone Tier 0 provider
├── docs/                     # public specification and threat model
└── tests/                    # independent fixtures and conformance driver
```

The exact repository layout is decided in its bootstrap milestone. It is not
created inside the Nobox workspace first and moved later.

## License and provenance rules

The independent product uses the unmodified Apache License 2.0 and the SPDX
identifier `Apache-2.0`. Its repository includes `LICENSE`, a concise
`CONTRIBUTING.md`, and a provenance policy. Contributions use Developer
Certificate of Origin sign-off unless that project later makes a separately
reviewed governance decision.

The source boundary is bidirectional:

- Public ideas, concepts, message semantics, interoperability requirements,
  measured behavior, and published specifications may inform either product.
- Code, patches, commit text intended as code, generated source, and test
  implementations are not copied across the boundary.
- Nobox changes inspired by the independent product are freshly designed and
  implemented under Nobox review, using Nobox names, types, and architecture.
- The independent product does not begin by copying or relicensing the current
  Nobox crate. It starts with its own repository and independently authored
  source.
- Black-box tools may connect to a released provider over its public socket.
  This tests behavior without creating a source or link dependency.
- Each repository records external inspiration in normal design notes when it
  materially shapes a change. Neither repository claims source provenance it
  cannot demonstrate.

This is a project provenance policy, not a promise that the two products will
remain byte-for-byte compatible. Compatibility is governed by advertised wire
revisions and conformance results.

## Tier and guarantee model

The terms Tier 0 and Tier 1 describe guarantees, not product quality.

| Property | Tier 0 standalone X11 | Tier 1 integrated Nobox |
| --- | --- | --- |
| Desktop discovery | EWMH root/client properties | Manager's live policy model |
| Management | EWMH requests followed by observation | Direct policy actions in the WM event loop |
| Stable public IDs | Provider-session handles over observed X11 clients | Native Nobox core identities |
| Events | Bounded diffs of observed X11 state | Manager-produced sequence stream |
| Consent | Configured grants in baseline | WM-owned consent plus configured grants |
| Human priority | Best effort only in optional input profile | WM input path has structural priority |
| Freshness | Re-observation and provider-local generations | Authoritative live generations and preconditions |
| Indicators | Standalone, spoofable/coverable X11 surface if added | WM-rendered Tier 1 indicators |
| Capture | Optional Composite/X11 profile | Authorized manager capture paths |
| Input | Optional XTEST/XInput profile | Window-addressed manager-mediated input |
| Hidden clients | Provider policy filters its own results | Manager makes them absent from the seat itself |
| Wayland enforcement | None | Future native compositor path |

Tier 0 responses must expose backend features and assurance level. Unsupported
operations return a typed `unsupported` result; they are not silently emulated
with global input or an arbitrary shell command. EWMH requests remain
requests: the provider observes the result and never claims the WM accepted an
operation merely because a client message was sent.

## Nobox launch-permission experience

Nobox already has two launch gates:

1. A companion must hold the `launch.desktop` capability or its `launch`
   bundle.
2. The global `[agent.launch]` policy must allow the requested desktop-entry
   identifier.

The settings UI must present both without conflating them. A companion grant
answers **who may request launches**; the application list answers **what may
be launched**.

The Agent Seat page gains an **Applications agents may launch** group with
three modes:

- **None** maps to `policy = "deny"`.
- **Selected applications** maps to `policy = "allow_listed"` and edits
  `allow`.
- **All installed except selected** maps to `policy = "allow_installed"` and
  edits `deny`.

The picker uses `nobox-desktop::ApplicationCatalog`, showing only bounded,
valid, visible XDG `Application` entries after precedence, localization,
desktop visibility, `TryExec`, and safe `Exec` parsing. It provides search,
stable category grouping, application name, icon where available, desktop ID,
and a visible user-installed marker. It must remain usable without loading all
rows as permanently realized GTK widgets.

User-installed entries remain disabled by default. Enabling them requires a
separate switch with direct language explaining that entries in the user's
writable application directory execute code. A selected user entry remains
ineffective while that switch is off; the UI shows that state rather than
silently removing the configured ID.

The editor preserves unknown-to-the-current-catalog IDs so temporarily
uninstalled applications do not disappear from configuration. It also honors
the existing maximum of 256 allow/deny entries and reports the bound before
changing the document. The **all installed except selected** mode represents
large catalogs without expanding them into an allow list.

Saving goes through typed `nobox-config::ConfigDocument` edits, complete
configuration validation, and the existing atomic persistence path. The UI
does not edit TOML text directly or maintain a second model of defaults.

## Coordinate and workflow-efficiency policy

Nobox does not collect a frequency map of pointer coordinates. Coordinate
popularity is not target identity: responsive layout, scrolling, browser zoom,
dialogs, application upgrades, and adversarial content can give the same point
a different meaning. Persistent coordinate history would add privacy-sensitive
state while weakening freshness.

The supported efficiency ladder is:

1. Use a structured desktop snapshot and events instead of reconstructing the
   desktop from pixels.
2. Use `client.semantic_find` with name, role, state, and current bounded
   geometry when the application exposes semantics.
3. Reuse session-scoped semantic handles only while their explicit tree
   generation remains current.
4. Use one grounded capture, or a coarse grid followed by a bounded crop, when
   semantics are unavailable or pixels are authoritative.
5. Couple input with bounded observation when verification is needed.

A future semantic-action proposal may accept a generation-stamped node handle
or selector and resolve its current actionable bounds immediately before
input. It proceeds only if the provider can reauthorize the client, prove the
tree and target current, preserve human preemption, and return stale rather
than guess. It is a new typed protocol operation, not a hidden coordinate
cache. Browser URLs and service identities remain outside the seat and may be
used by an agent harness through their exact data sources.

No persistent coordinate, image, page-content, or semantic-tree store is added
to Nobox, `nobox-agent`, the standalone provider, or the protocol library by
this roadmap.

## Milestone order

Every Nobox milestone follows the repository build/test workflow, increments
the patch version of each changed crate, updates relevant docs in the same
change, and is committed and pushed only after full verification. The
independent product defines the same discipline in its own repository and
versions itself independently.

### N0: finish the Nobox v2 hardening baseline

Goals:

- Complete B6 in [`agent-interface-v2.md`](agent-interface-v2.md), including
  remaining scaling, multiple-output, responsive-reflow, fallback, and release
  measurements.
- Preserve the successful real-harness Nobox workflow as dogfood evidence.
- Publish the proper Nobox source release before renaming crate identities.

Non-goals:

- Starting the independent Apache-2.0 repository by copying current sources.
- Adding Tier 0 code to Nobox.
- Expanding B6 into coordinate memory or workflow recording.

Exit:

- B6's definition of done is met, the complete prescribed suite passes, and a
  tagged source release documents the implemented wire revision and runtime
  behavior.

### N1: expose installed-application launch policy in Nobox Settings

Goals:

- Add the three launch-policy modes, searchable XDG picker, user-entry switch,
  and clear two-gate explanation described above.
- Add typed, format-preserving document operations for launch policy and
  membership edits.
- Reuse `nobox-desktop`; do not introduce a second desktop parser.

Verification:

- Unit tests cover mode changes, retained unknown IDs, entry bounds, user-entry
  behavior, comment preservation, and invalid complete documents.
- GUI-focused tests cover filtering and mapping row state to typed edits where
  practical.
- The nested-X agent test proves one selected application launches, one
  unselected application is refused, and a user-installed entry remains
  refused until the explicit switch is enabled.

Exit:

- A user can configure and verify launch permissions without Advanced TOML;
  reconfigure applies the same policy to live sessions; the full build and
  CTest suite pass.

### N2: rename Nobox's internal wire crate

Goals:

- Rename `crates/agent-seat-proto` and its Cargo package/import path to
  `nobox-agent-wire` using a history-preserving `git mv`.
- Update Nobox source, CMake, tests, installed documentation, architecture,
  contributor instructions, and package metadata.
- State explicitly that the crate is Nobox's GPL-2.0-only wire implementation.

Non-goals:

- Changing serialized names, protocol name `agent-seat`, wire revision,
  framing, behavior, socket location, or grant semantics.
- Publishing a compatibility crate under the old package name.
- Adding any dependency on the future independent repository.

Verification:

- A repository-wide search finds no active Cargo/package reference that still
  treats the old crate as extraction-ready.
- Existing unit, MCP, and nested-X agent tests pass without changing their
  wire fixtures except implementation/package names not present on the wire.

Exit:

- Nobox builds and behaves identically with an implementation name that cannot
  be mistaken for the independent product.

### E0: bootstrap the independent Apache-2.0 product

This milestone happens in a new repository, not in Nobox.

Goals:

- Create `github.com/ZaguanLabs/agent-seat-proto` directly under the
  ZaguanLabs GitHub owner; do not create it under a personal owner for later
  transfer.
- Configure ZaguanLabs organization ownership, administrator recovery,
  protected default branch, required review/status checks, release permissions,
  and the canonical security-contact path before accepting outside code.
- Add the Apache-2.0 license, contribution/DCO policy, provenance policy,
  security policy, code of conduct, and release process.
- Define product names, crate boundaries, MSRV, supported platforms, and the
  initial public specification structure.
- Record Nobox as prior art and a compatible implementation target without
  importing its source.

Non-goals:

- Copying Git history, Rust modules, schemas, tests, prose wholesale, or
  generated artifacts from Nobox.
- Claiming compatibility before black-box tests prove it.

Exit:

- The empty/skeletal product can be built and tested from source, its inbound
  licensing is unambiguous, every committed source file has documented
  provenance, and GitHub, package, documentation, and release metadata all
  identify `ZaguanLabs/agent-seat-proto` as canonical upstream.

### E1: independently implement the public wire and MCP boundary

Goals:

- Specify strict framing, bounds, message shapes, capabilities, errors,
  revision negotiation, backend features, and advertisement parsing.
- Implement the Apache-2.0 `agent-seat-proto` crate independently with typed
  IDs, exhaustive states, strict deserialization, and no display-server or
  policy dependency.
- Implement a generic MCP companion that can initialize and list tools without
  connecting to a desktop.
- Resolve a seat from an explicit argument, an environment override, or the
  X11 `_AGENT_SEAT` advertisement without a Nobox-specific default path.

Non-goals:

- Grant authority or policy in the MCP companion.
- Treating a similar JSON shape as proof of wire compatibility.

Exit:

- Unit and process tests cover malformed/oversized frames, strict schemas,
  revision refusal, lazy connection, and discovery on an isolated X server.

### T0: Tier 0 provider foundation and single-provider ownership

Goals:

- Start a private per-display UNIX socket only when explicitly enabled.
- Bind sessions to local peer credentials and enforce configured grants at the
  provider, never in the MCP translator.
- Advertise the exact protocol revision, socket, provider identity, and
  features through X11 discovery.
- Define single-provider ownership so a standalone daemon refuses to compete
  with an integrated seat. A real X11 selection is preferred for atomic
  ownership; a property-only race is not accepted as the final design.
- Withdraw ownership and remove the socket on clean shutdown; recover safely
  from stale runtime files without replacing a live provider.
- Keep all queues, frames, peers, and per-session state bounded.

Baseline policy:

- Disabled and deny-by-default.
- Strict standalone TOML under the product's own XDG configuration directory.
- Configured grants only; no consent window in the first Tier 0 release.
- No Nobox config parsing or migration.

Exit:

- In isolated Openbox and no-WM sessions, provider start/stop, stale recovery,
  duplicate refusal, peer identity, grant denial, slow-client disconnect, and
  daemon-crash isolation behave predictably. Openbox remains usable throughout.

### T1: EWMH observation and bounded event diffs

Goals:

- Build a bounded snapshot from EWMH desktops, work areas, client lists,
  stacking, active window, titles, types, states, and allowed actions.
- Map X11 resources to provider-session opaque handles; never expose raw XIDs.
- Represent missing or unsupported EWMH facts as unavailable rather than
  inventing values.
- Produce a monotonic provider sequence and bounded event stream by diffing
  observed X11 state, with explicit resynchronization after overflow.
- Apply scope and title-visibility policy before allocating response content.

Non-goals:

- Claiming these generations are authoritative inside the foreign WM.
- Polling without limits or promising event atomicity EWMH cannot provide.

Exit:

- Black-box tests under Openbox create, rename, move, minimize, map, and destroy
  simple clients and prove snapshots plus diffs converge without leaking
  filtered clients.

### T2: EWMH management

Goals:

- Support the subset of activate, close, workspace switch/send, state, and
  move/resize operations that the target WM advertises and EWMH can express.
- Validate scopes and freshness against a current observation before sending
  an EWMH request.
- Observe afterward and report requested, sent, observed, refused, timed out,
  and unsupported as distinct states.

Non-goals:

- XKillClient, bypassing `WM_DELETE_WINDOW`, or claiming the WM accepted a
  client message.
- Emulating an unsupported management operation through pointer/keyboard input.

Exit:

- Openbox integration tests cover every supported operation, stale target,
  unsupported atom, ignored request, client disappearance, and daemon failure.

### T3: policy-controlled desktop-entry launch

Goals:

- Independently implement bounded XDG discovery and safe desktop `Exec`
  parsing without a shell.
- Provide deny, allow-listed, and allow-installed policies plus an explicit
  user-entry switch.
- Return a launch token and correlate a resulting client when available,
  without promising correlation where the foreign WM/application supplies
  insufficient metadata.

Non-goals:

- Reusing `nobox-desktop` source or depending on Nobox.
- Arbitrary commands, shell evaluation, silently passing unsupported field
  codes, or treating installed entries as harmless data.

Exit:

- An Openbox test launches one allowed system fixture, refuses an unlisted
  fixture, refuses a user fixture by default, proves hostile metacharacters are
  not shell-evaluated, and keeps Openbox responsive after launch failure.

At this exit the Tier 0 core release is useful for observation, management,
and controlled launching. Capture and input remain absent and are reported as
unsupported.

### T4: optional X11 capture profile

This profile is not required for the Tier 0 core release.

Goals:

- Specify the weaker standalone X11 capture threat model before implementation.
- Capture a granted client or output only when scope and visibility checks can
  be reapplied at capture time.
- Stamp pixels with observed geometry, sequence, coordinate space, and an
  explicit Tier 0 assurance level.
- Use bounded images and grounded coordinate grids.

Stop condition:

- If Composite/X11 behavior cannot prevent an out-of-scope or hidden client
  from entering a requested capture, that capture mode is not shipped. The
  profile may remain client-visible-only or unsupported.

Exit:

- Nested tests cover obscured, off-workspace, destroyed, hidden, overlapping,
  oversized, and provider-failure cases without broadening grants.

### T5: optional best-effort X11 input profile

This profile is not required for the Tier 0 core release and must not reuse
Tier 1 wording for its guarantees.

Goals:

- Translate window-relative coordinates against freshly observed client
  geometry and refuse stale targets.
- Use XTEST only when explicitly configured and advertise it as best effort.
- Detect human activity through XInput where available, keep a suppression
  window, and stop pending work when interruption is observed.
- Distinguish events queued to X11 from application acceptance.
- Provide a local emergency stop that does not depend on the MCP companion.

Non-goals:

- A secure global input boundary, unspoofable consent/indicator UI, guaranteed
  attribution of every device event, or Tier 1 atomicity.
- Falling back to global screen coordinates when client-relative grounding is
  unavailable.

Stop condition:

- If injected and human input cannot be distinguished well enough to honor the
  documented suppression contract, mutation remains unsupported in that
  environment.

Exit:

- Openbox/Xephyr tests cover click, key, paced text, stale geometry, focus
  change, user interruption, partial progress, kill switch, and daemon death,
  with every result using best-effort delivery language.

### T6: optional semantic profile

Accessibility is deferred until the standalone provider, scoping model, and
capture/input profiles are stable.

Goals:

- Perform a new threat-model and client-to-accessible-root correlation study
  in the independent product.
- Use a disposable bounded helper with no policy authority if and only if
  hidden/out-of-scope equivalence can be preserved.
- Prefer a typed current semantic action over persistent coordinate memory.

Non-goals:

- Copying Nobox's helper, treating Nobox's evidence as sufficient for the new
  provider, or exposing a desktop-wide tree.

Exit:

- Independent fixtures prove safe correlation and failure behavior. Otherwise
  the profile remains unsupported and grounded capture remains the fallback.

### C0: black-box compatibility and release matrix

Goals:

- Maintain a published matrix of provider, companion, protocol revision,
  backend features, and tested window managers.
- Run each product's own conformance driver against released binaries of the
  other product where licensing and CI availability permit.
- Exercise advertisement, handshake, bounds, refusal shapes, feature
  negotiation, snapshot semantics, and supported operations solely through
  public process boundaries.
- Record incompatibility honestly and allocate a new revision when behavior
  cannot remain compatible.

Non-goals:

- Vendoring the other project's conformance implementation or making a release
  depend on an unreleased branch of the other repository.

Exit:

- A user can tell from the matrix whether a particular companion/provider pair
  is tested, compatible, partially supported, or unsupported.

### N3: independently adopt useful external ideas

This is a standing Nobox process after the independent product exists.

For each candidate idea:

1. Describe the user problem and observable behavior in a Nobox issue or
   design note without copying source.
2. Decide whether it belongs in Nobox policy, configuration, X11 realization,
   MCP translation, or nowhere.
3. Specify Nobox's typed contract and threat model.
4. Implement it freshly against Nobox's architecture and tests.
5. Increment changed crate patch versions and complete the normal Nobox
   verification/release discipline.

Ideas are not accepted merely to keep feature parity. Tier 1 remains smaller,
more authoritative, and policy-driven where that is better for Nobox.

## Verification strategy

### Nobox gates

Every Nobox milestone runs:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

Focused unit tests own pure policy, strict configuration, document editing,
wire bounds, and semantic freshness. Nested-X tests own socket lifecycle,
X11 realization, launch, capture/input provenance, human interruption, and
companion/provider failure. Real-desktop dogfood supplements but never replaces
repeatable tests.

### Independent product gates

The independent repository defines an equivalent source-first workflow. Its CI
must include Rust format/lint/test, strict wire fixtures, malformed and bounded
I/O, isolated X11 sessions, Openbox integration, slow/dead peer behavior, and
source-release assembly. Unsafe Rust is avoided; if that product ever chooses
otherwise, its own policy must document and test every safety invariant.

### Cross-product gates

Cross-product testing uses installed or released executables and public socket
behavior. A failure is an interoperability result, not permission to copy the
other implementation. Each product remains fully testable when the other is
absent.

## Release and versioning rules

- Nobox keeps its existing patch-per-milestone rule unless the maintainer
  explicitly requests a minor or major bump.
- Renaming `agent-seat-proto` to `nobox-agent-wire` changes the Nobox crate's
  package identity but not the wire revision. The milestone records the
  history and resulting package version explicitly.
- The independent product begins its own pre-1.0 semantic versions and does
  not inherit Nobox crate versions.
- Crate/package versions and Agent Seat wire revisions are different values.
  A patch release may preserve a wire revision; a breaking wire change always
  advertises a new revision.
- Each repository creates its own signed/annotated tag as project policy
  permits and a proper source release containing license, notices, changelog,
  build instructions, checksums, and the compatibility matrix relevant to
  that release.
- Independent-product source archives, crate metadata, documentation links,
  security notices, and release automation point to
  `https://github.com/ZaguanLabs/agent-seat-proto`.

## Risks and explicit stop conditions

- **License/provenance ambiguity:** stop if a proposed change requires copying
  source across the product boundary. Restate the behavior and independently
  design it instead.
- **Protocol collision:** stop if two incompatible shapes would advertise the
  same protocol revision. Allocate a new revision or extension.
- **Two providers on one display:** stop if ownership is property-race based.
  Define atomic X11 ownership before release.
- **Tier inflation:** stop if a standalone feature requires claiming WM-owned
  guarantees the daemon does not possess. Reduce the assurance level or omit
  the feature.
- **Hidden-client leakage:** stop capture, semantics, or error-detail work if
  an out-of-scope client can affect returned content distinguishably.
- **Human-priority failure:** stop Tier 0 input mutation in environments where
  documented interruption cannot be implemented.
- **Coordinate staleness:** refuse or re-observe; never make a historical
  coordinate cache the fallback.
- **Unbounded catalog/UI behavior:** paginate, search, virtualize, or refuse;
  never make the settings process or provider scale directly with arbitrary
  filesystem contents.
- **External-product unavailability:** Nobox work continues. No Nobox build,
  test, session, or release requires the independent repository.

## Overall definition of done

- The current Nobox implementation has a Nobox-specific crate identity and no
  documentation describes it as extraction-ready.
- Nobox and `agent-seat-proto` have separate repositories, licenses, histories,
  dependencies, contribution rules, and releases.
- `ZaguanLabs/agent-seat-proto` is the canonical GitHub upstream and release
  origin for the independent product.
- Neither product contains copied implementation source from the other.
- Nobox Settings can edit the complete installed-application launch policy
  safely and preserve the strict TOML document.
- Nobox completes the semantic/capture efficiency work without persistent
  coordinate or workflow history.
- A stock Openbox session can run the independent Tier 0 core provider for
  denied-by-default observation, supported EWMH management, and allowlisted
  desktop-entry launch.
- Optional Tier 0 capture, input, and semantics ship only when their documented
  weaker guarantees and stop conditions are met.
- Provider discovery cannot race two live seats on one X11 display.
- Every provider reports exact revision, features, and assurance; every
  companion refuses what it cannot speak.
- Compatibility claims are backed by black-box results and a published matrix.
- Failure of Nobox, a companion, a standalone provider, Openbox, or the
  independent product never becomes a hidden dependency failure in another
  product.
