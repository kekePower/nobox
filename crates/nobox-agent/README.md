# nobox-agent

An MCP companion for an [Agent Seat Protocol](../../docs/agent-protocol.md)
socket. It speaks stateless MCP revision 2026-07-28 and the handshake revisions
2025-11-25, 2025-06-18, 2025-03-26, and 2024-11-05 on stdio toward an agent
harness, and the Agent Seat Protocol toward a window manager.

The companion is a translator with no authority. Every request it forwards is
validated again by the window manager against the session's grant, so nothing
in this process is a security boundary. It is a reference client for any window
manager implementing the socket, not only nobox.

## Setting up a harness

[docs/agent-harness.md](../../docs/agent-harness.md) is the full walkthrough,
including turning the seat on and granting capabilities. The short version:
point the harness at the binary, which needs no arguments in an ordinary
session:

```sh
nobox-agent --print-mcp-config
```

That prints the following copyable generic MCP registration without connecting
to the desktop or requiring `DISPLAY`:

```json
{
  "mcpServers": {
    "nobox": { "command": "nobox-agent" }
  }
}
```

Claude Code can add it directly:

```sh
claude mcp add nobox -- nobox-agent
```

The socket is taken from `--socket`, then `AGENT_SEAT_SOCKET`, then a live
selection-bound `_AGENT_SEAT` property on the selected X11 root. Root discovery
requires `_AGENT_SEAT_S<screen>` to have an owner and that owner and the root to
carry identical bounded values; stale or mismatched properties are ignored.
There is no conventional filesystem fallback:

```sh
xprop -root _AGENT_SEAT
```

Handshake, discovery, and tool listing do not need the socket. The companion
resolves and connects to it only when a tool explicitly reaches for the seat.
Hosts that sanitize subprocess environments must pass `DISPLAY` for root
discovery, or configure `--socket`/`AGENT_SEAT_SOCKET`, before desktop tools can
work.

## What the harness gets

Nothing, until the user says otherwise. A companion whose executable no grant
names holds no capabilities at all: it connects, is told so, and every request
is refused. Set `[agent].policy = "ask"` in the nobox configuration to be asked
instead, and answer with `p` to store the grant.

Launching has a second, independent gate. In **nobox preferences → Agent
seat**, grant the companion launch access, select the applications it may
start, then choose **Save and apply**. A checked application is not part of the
live policy until the running window manager accepts that reload. The status
line confirms the request or explains how to apply the saved file later.

`server/discover` is deliberately static so a host's startup probe cannot wait
on a socket or consent dialog. Call `seat_status` to connect and report what the
window manager actually granted this session.

## Tools

| Tool | What it does |
| --- | --- |
| `seat_status` | Connect and report manager, grant, scope, and backend features |
| `desktop_snapshot` | The whole desktop as structured state |
| `desktop_subscribe` | Start an event stream and get the snapshot it continues from |
| `events_poll` | Retrieve events after a sequence number |
| `client_get` | One window's descriptor, with its generation counter |
| `client_semantic_root` | Start or refresh one bounded, generation-stamped semantic tree |
| `client_semantic_find` | Return only bounded name/role/state matches from a client tree |
| `client_semantic_tree` | Page a bounded subtree when nearby structure is needed |
| `launch` | Start an approved installed application, with a correlation token |
| `client_capture`, `output_capture` | Pixels, where only pixels answer; client captures can add a coordinate grid and both tools can crop before encoding |
| `client_pointer`, `client_key`, `client_type` | Window-addressed input, optionally followed by one bounded observation |
| `client_activate`, `client_close`, `client_move_resize`, `client_set_state`, `client_send_to_workspace`, `workspace_switch` | Window management |

Prefer `desktop_snapshot` and the event stream over screenshots: they are
exact, cheap, and stamped with the sequence they correspond to. Carry the
highest sequence you have applied and pass it back as `after_seq`; a
`resync_required` event means the backlog was dropped and the world model must
be rebuilt.

Mutating tools accept an `expects` block naming the generation, geometry,
workspace, or focus you observed. Name only the facts an action actually
depends on: `generation` covers every descriptor-visible change, including a
cosmetic title update, while geometry, workspace, and focus are narrower. The
manager refuses with `stale_state` rather than acting on an obsolete belief.

Prefer `client_semantic_find` over capture when a target has an accessible
name. Refine its typed results using role, states, and non-empty
content-relative bounds; portable roles are not DOM element names, so a browser
video may appear as a focusable `group`. Reuse opaque continuations unchanged,
and call `client_semantic_root` again after `stale_tree`. Semantics require the
independent `accessibility` grant. Run semantic tools sequentially: helper work
is single-flight, so concurrent excess fails closed as `semantic_unavailable`.
If the result is unavailable, ambiguous, or lacks actionable bounds, take a
grounded client capture instead of guessing a coordinate. If the target is on
a hidden workspace, restore or activate that client and re-observe it before
capture; an old coordinate is not a substitute for current grounding.

Input is window-addressed: coordinates are relative to a window's own content
area, and a screen coordinate is not expressible. A call made while the user is
typing or clicking is refused as `interrupted` and reports which steps had
already committed. `client_type` validates the complete string before making
the window visible or injecting input, so an `invalid_argument` cannot leave a
partial prefix behind. Text available on the active layout uses paced character
strokes up to 4,096 Unicode scalars. Longer writes and other printable UTF-8
use a target-scoped selection offer and one paste chord; this temporarily
displaces the current X11 clipboard owner without reading or restoring its
contents, and serves the text only to the target's X11 client. One call accepts
at most 32 KiB of UTF-8 and 16,384 Unicode scalars. The selection remains alive
for a 250 ms quiet period after a completed conversion so rich clients can
finish follow-up requests, without extending the two-second absolute deadline.

When a multimodal model needs to read a click point from pixels, pass
`grid: { spacing: 100 }` to `client_capture`. The returned PNG carries
high-contrast lines and numeric labels in the exact coordinates
`client_pointer` accepts. The structured result's `grid.origin_x` and
`grid.origin_y` say which content coordinate image pixel `(0, 0)` represents,
so the same rule works for cropped captures. For a large window, first use a
100-pixel grid to identify the coarse cell, then capture that cell as a smaller
`rect` with a 50-pixel grid. Read the labels and origin from that crop; do not
scale coordinates from the harness's resized rendering of a full-window image.

Write a complete coherent passage, including its `\n` line and paragraph
breaks, in one `client_type` call. Do not spend separate `client_key` calls on
Return just to format text. The manager validates the whole passage first.
Layout-representable text is paced as complete character strokes through the
event loop so a rich editor can keep up and a person can preempt a long write
between characters. Exact UTF-8 that the layout cannot produce uses the bounded
selection fallback above. The operation stops with `stale_state` if the target
client loses keyboard focus, rather than continuing into another window.

To combine input and the ordinary check afterward, attach an `observe` block.
Omit `capture` when correlated events are enough:

```json
{"minimum_ms": 50, "quiet_ms": 150, "maximum_ms": 1500}
```

Request pixels only when they answer something the events do not:

```json
{
  "capture": {"rect": {"x": 0, "y": 0, "width": 400, "height": 200}},
  "minimum_ms": 50,
  "quiet_ms": 150,
  "maximum_ms": 1500
}
```

The manager injects first, keeps processing the live seat, waits for the
bounded quiet policy, and returns one action ID plus a capped slice of temporally
correlated desktop events. A requested capture adds one final PNG; its optional
`client` can name a stable parent when the input may close a transient dialog.
`delivery` remains `unverified`:
the capture is evidence from after the action, not proof that the application
accepted it or that the action caused what appears in the image. A person using
the keyboard or pointer interrupts pending observation immediately. Any final
capture is authorized again when it is taken, and a capture refusal is returned
as a structured sample without pretending the earlier injection did not occur.

Invalid MCP tool arguments return JSON-RPC `-32602` with a machine correction
in `error.data`. Read `path`, `expected`, `received`, and `retryable`; do not
parse `message`. The path is a JSON Pointer relative to the tool arguments.
For example, an unknown `/window` field reports `expected.kind: "absent"`,
while a string at `/grid/spacing` reports the accepted integer bounds. Seat
refusals use the same error shape in tool `structuredContent`, with
`current_generation` for stale state and `committed` for partial operations.

Use the seat for graphical-session state, pixels, and mutation. Exact facts the
seat does not represent—such as URLs, service or channel identities, feed/API
data, files, builds, and version control—belong to ordinary exact data sources;
there is no benefit in OCR-ing them from a window. That does not permit using
another route to reveal a hidden or out-of-scope window, bypass a refusal, or
mutate the graphical session behind the seat.
