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

The socket is taken from `--socket`, then `AGENT_SEAT_SOCKET`, then
`$XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock`. A manager also advertises
it on the root window, which is the mechanism a manager other than nobox would
use:

```sh
xprop -root _AGENT_SEAT
```

Handshake, discovery, and tool listing do not need the socket. The companion
resolves and connects to it only when a tool explicitly reaches for the seat.
Hosts that sanitize subprocess environments must nevertheless pass `DISPLAY`
and `XDG_RUNTIME_DIR`, or configure `--socket`, before desktop tools can work.

## What the harness gets

Nothing, until the user says otherwise. A companion whose executable no grant
names holds no capabilities at all: it connects, is told so, and every request
is refused. Set `[agent].policy = "ask"` in the nobox configuration to be asked
instead, and answer with `p` to store the grant.

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
| `launch` | Start an approved installed application, with a correlation token |
| `client_capture`, `output_capture` | Pixels, where only pixels answer |
| `client_pointer`, `client_key`, `client_type` | Window-addressed input |
| `client_activate`, `client_close`, `client_move_resize`, `client_set_state`, `client_send_to_workspace`, `workspace_switch` | Window management |

Prefer `desktop_snapshot` and the event stream over screenshots: they are
exact, cheap, and stamped with the sequence they correspond to. Carry the
highest sequence you have applied and pass it back as `after_seq`; a
`resync_required` event means the backlog was dropped and the world model must
be rebuilt.

Mutating tools accept an `expects` block naming the generation, geometry,
workspace, or focus you observed. The manager refuses with `stale_state` and
names the current generation rather than acting on an obsolete belief, which
costs one round trip instead of a wrong click.

Input is window-addressed: coordinates are relative to a window's own content
area, and a screen coordinate is not expressible. A call made while the user is
typing or clicking is refused as `interrupted` and reports which steps had
already committed.
