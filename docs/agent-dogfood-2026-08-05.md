# Agent-seat dogfood report: Zen and Google Gemini

Date: 2026-08-05
Environment: Mageia 11, X11, two 2560x1600 outputs, nobox 0.1.0,
`nobox-agent` 0.1.0, Zen Browser

## Executive summary

The agent seat has an unusually good security and desktop-state foundation.
It let me identify the correct Zen window without scraping pixels, activate it
with a freshness precondition, navigate to a new Gemini conversation, and
verify the result with a window-only capture. The protocol makes the safe path
the convenient path, which is the hardest part of this product to get right.

The task still failed. I could not send the greeting, despite every pointer,
typing, and Return call reporting a successful `committed` result. Captures
showed that Gemini's prompt remained empty. The most important product issue is
therefore not that input can fail; it is that the current result cannot
distinguish "input events were injected" from "the intended control received
the input." The agent was given strong positive evidence for an action that had
no user-visible effect.

There was also a harness interoperability failure before desktop control began.
The configured MCP server was not exposed as callable tools by the host, and a
resource probe reported that the server was "not ready for this step." I had to
start `nobox-agent` myself and speak its 2026-07-28 stateless JSON-RPC dialect
over stdio. An exact protocol-version mismatch is the likely explanation, but
that root cause was not proven during this run.

My overall view: the authority model and structured observation layer are
excellent; the harness boundary and input-verification story are not yet
reliable enough for unattended real work in complex GUI applications.

## The task

The requested flow was deliberately simple:

1. Find the existing Zen Browser window.
2. Open a fresh Google Gemini conversation.
3. Type a friendly greeting and submit it.
4. Wait for Gemini's response and return it to the user.

The greeting was:

> Hello, Gemini! I hope you're having a lovely day. I'm stopping by from
> ChatGPT via Stig's desktop to say hello. How are you doing today?

The run was stopped after repeated input attempts left the prompt empty, so no
Gemini response was obtained.

## What worked well

### Structured discovery was the right default

`desktop_subscribe` returned the outputs, workspaces, focus, stacking order,
and exact client descriptors in one response. I could select the Zen client by
class, role, and title instead of guessing from a screenshot. The descriptor's
client identity and generation made subsequent actions explicit and auditable.

This was substantially better than conventional desktop automation. A full
screen screenshot would have exposed more information, cost more tokens, and
still left window identity ambiguous.

### Freshness checks made activation feel safe

The first Zen activation included the observed generation, workspace, and
focus state. It committed successfully, and `client_get` then showed the
expected generation change and focused state. This is a strong interaction
contract: act against a stated belief, refuse if that belief is stale, and make
the caller refresh rather than guess.

### Window management and keyboard shortcuts were effective

Using `client_activate` was predictable. `Control+l`, typing the Gemini URL,
and Return navigated Zen to a fresh Gemini screen. Later,
`Control+Shift+k` reached Gemini's own Search chats command, which confirms
that injected keyboard shortcuts can reach the web application rather than
only the browser chrome.

### Window-only capture was genuinely useful

`client_capture` gave a precise 1790x1572 image of Zen without capturing the
other output. It verified that the new-conversation page had loaded and, more
importantly, disproved the apparent success of the typing calls. The separate
visible/obscured/output permissions are a good expression of the real privacy
difference between those operations.

### The safety model was understandable

The session grant clearly named its atoms, actions were attributed in the
window-manager log, input and capture had visible indicators, and the server's
discovery guidance explained `interrupted`, `session_frozen`,
`session_revoked`, and `no_such_client` as decisions rather than errors to
route around. This is thoughtful product design, not merely a permission list.

## What was difficult

### The MCP server was configured but unavailable to the host

The workstation configuration contained:

```toml
[mcp_servers.nobox]
command = "nobox-agent"
```

Even so, no nobox tools appeared in the host's callable tool catalog. Probing
MCP resources produced `MCP server 'nobox' was not ready for this step`.
Running `nobox-agent` directly worked immediately once each request supplied
the 2026-07-28 protocol metadata.

That creates a poor first-run experience: the user knows the server is
installed and configured, while the agent sees no actionable tool and no
diagnostic explaining why. The exact-version check in the companion makes a
protocol mismatch plausible, but the product currently leaves the operator to
infer that from source or reproduce the wire protocol manually.

### Captures are returned in an awkward shape for visual agents

The capture reply places PNG bytes as base64 inside structured JSON, and then
duplicates that JSON as text content. To inspect it, I had to decode the base64
to a temporary file and pass that file to an image viewer. This works, but it
is expensive and easy for a harness to truncate.

The capture should include a native MCP image content block with
`image/png`, while keeping width, height, source rectangle, and sequence in
structured metadata. The textual copy should not duplicate the base64 payload.

### Waiting for a web response has no good signal

The desktop event stream describes window-manager state, not content inside a
browser. A Gemini response can appear without changing the window title,
geometry, focus, or other observable client state. The only available fallback
is repeated pixel capture and visual comparison.

Subscriptions also filter by event kind but not by client. During the run, the
first event after navigation concerned an unrelated terminal title, so one
poll completed without saying anything about Zen. Client filtering would
reduce noise, although it would not solve the deeper lack of application-level
semantics.

### Session behavior became harder to reason about during capture

I used a persistent companion for actions and short-lived companion processes
to decode captures. After one such capture, the persistent companion reported
`connection closed` on its next call and reconnected successfully on the
following call. Later short-lived captures did not reproduce the exact same
failure consistently.

This needs a focused concurrency test. If multiple companions for the same
executable are intentionally exclusive, discovery should say so and a second
connection should receive a specific refusal. If they are meant to coexist,
one session ending must not disturb another.

## The ugly part: false confidence from input results

The critical sequence was:

1. Capture showed a clean Gemini new-chat screen.
2. `client_pointer` clicked inside the visible prompt and returned
   `committed: ["inject"]`.
3. `client_type` sent the full greeting and returned
   `committed: ["inject"]`.
4. `client_key` sent Return and returned `committed: ["inject"]`.
5. A new capture showed the prompt still empty and no conversation started.
6. A second click at a slightly different point followed by a single `a` also
   reported committed, while a capture again showed no text.

The coordinate choice was visually reasonable and inside the prompt. The run
does not prove whether the failure was coordinate translation, focus on a
browser descendant, toolkit event routing, or Gemini rejecting synthetic
input. It does prove that `committed` currently overstates what is known.

At the window-manager boundary, nobox can truthfully say that it injected a
sequence of low-level events. It cannot know that a contenteditable control
accepted them. The result should preserve that distinction. Suggested wording
and fields:

```json
{
  "reply": "injected",
  "delivery": "unverified",
  "events_requested": 2,
  "events_injected": 2,
  "top_level_client": 22020100,
  "focused_x11_window": "0x...",
  "sequence": 1024
}
```

`committed` remains appropriate for window-manager state changes such as
activation, geometry, or workspace movement, where nobox owns and can observe
the state transition. For input, `injected` is both accurate and more useful.

The repeated sequence value also added ambiguity. Several successful-looking
input calls returned the same sequence. That may be correct if the sequence is
a desktop-state barrier rather than an operation identity, but the distinction
should be explicit. An independent request or injection identifier would make
logs and retries easier to correlate.

## What I would improve next

### P0: make input outcomes honest and diagnosable

- Rename the input result from `committed` to `injected` or add an explicit
  `delivery: unverified` field.
- Report requested and injected event counts, the resolved top-level client,
  and the focused X11 descendant before and after injection.
- Add an optional diagnostic mode that returns the resolved root coordinate
  for a client-relative pointer action.
- Add real-browser integration coverage. A nested test should focus and type
  into a Firefox or Chromium content control, not only a simple X11 client.
- Test pointer focus followed by text input as one end-to-end scenario. This is
  the common operation, not two unrelated primitives.

### P0: provide an MCP compatibility and diagnosis path

- Support at least one widely deployed stable MCP revision in addition to the
  pinned 2026-07-28 revision, or ship a compatibility mode selected by the
  host's request metadata.
- Make protocol negotiation failures visible to the harness instead of leaving
  the server in a generic "not ready" state.
- Add `nobox-agent doctor` or `nobox-agent --self-test` that prints socket
  discovery, manager version, granted capabilities, supported MCP revisions,
  and a minimal `desktop_snapshot` result.
- Document a one-line raw JSON-RPC smoke test so a user can separate host
  incompatibility from socket, grant, or window-manager failures.

### P1: return captures as native images

- Emit an MCP image content block for the PNG.
- Keep capture metadata in structured content.
- Do not repeat base64 image data in text content.
- Consider an optional bounded thumbnail for quick visual verification, with
  the full-resolution capture still available when text must be read.

### P1: add observation filters and correlation

- Let `desktop_subscribe` and `events_poll` filter by client, workspace, or
  output in addition to event kind.
- Give every mutating or input request a correlation identifier echoed in the
  reply and log.
- Explain in the schema whether `sequence` is a state barrier, an event cursor,
  or evidence that the requested action changed observable state.

### P1: offer a semantic companion without polluting the WM core

Modern browser and GTK/Qt applications expose meaningful controls below the
top-level X11 client. The window manager should not learn DOM or toolkit
semantics, but the product would benefit from an optional AT-SPI companion or
protocol extension that can:

- list accessible controls within a granted client;
- identify the focused control and its role;
- focus a control by stable accessible identity;
- set or type text and observe the resulting value;
- subscribe to accessible text and busy-state changes.

This should be a separate, separately granted capability. The current
window-addressed protocol remains the secure authority and scoping layer; the
semantic bridge supplies the application-level evidence it cannot provide on
its own.

### P2: make coordinate work easier when semantics are unavailable

- Include an optional cursor marker or coordinate grid in diagnostic captures.
- Return the pointer's resolved client-relative position after a move/click.
- Offer a single `client_click_type` transaction that activates, focuses at a
  point, injects text, and reports each completed stage. It should still label
  application acceptance as unverified.

## Product principles worth preserving

The fixes above should not weaken what makes the agent seat distinctive:

- no global-coordinate input;
- the window manager remains the authority;
- per-executable, least-privilege grants;
- hidden and out-of-scope windows remain indistinguishable;
- human input has priority;
- capture permissions reflect how much the agent can actually see;
- structured state and freshness preconditions come before pixels;
- toolkit and accessibility integration stays outside the window-manager core.

Those choices are already better than most desktop automation systems. The
next step is to make interoperability failures explicit and to ensure that an
input result never claims more than nobox can observe.
