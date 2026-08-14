# Connecting an agent harness

This is the practical side of [the Agent Seat Protocol](agent-protocol.md):
how to turn the seat on, how to point an MCP harness at it, and how to give
that harness exactly as much of your desktop as you meant to.

Two separate things have to be true. The **seat** lives inside nobox and is off
until you enable it. The **companion**, `nobox-agent`, is a small process your
harness starts; it translates MCP into the seat's protocol and enforces
nothing on its own.

## 1. Turn the seat on

```toml
# ~/.config/nobox/config.toml
[agent]
enabled = true
policy = "ask"
```

The settings application has the same controls under **Agent seat**, including
the list of companions that hold a grant and a searchable installed-application
picker for the independent launch policy. Remember that both gates apply: a
companion needs the launch capability, and the requested desktop entry must be
allowed by the application list. Use **Save and apply** after changing either
gate; a checked application is only part of the live policy after the running
window manager has accepted that reload.

Reload nobox (`kill -HUP $(pidof nobox)`, or the `reconfigure` action) and
check that the seat came up. Enabling and disabling take effect on reload, so
this never needs a restart:

```sh
xprop -root _AGENT_SEAT
```

That property is the discovery mechanism: it names the protocol, its version,
and the socket path. If it is absent, the seat is not running — look for
`agent seat listening` in nobox's log.

## 2. Register the companion with your harness

Most MCP hosts take a command to run. `nobox-agent` needs no arguments in an
ordinary desktop session:

```sh
nobox-agent --print-mcp-config
```

This prints a copyable generic MCP registration without connecting to the
desktop or requiring a socket:

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

The companion finds the seat from `--socket`, then `AGENT_SEAT_SOCKET`, then a
live selection-bound `_AGENT_SEAT` property on the selected X11 root. It does
not synthesize a Nobox filesystem path. Root discovery needs the same `DISPLAY`
as the session; a service, container, or different user should receive the
socket explicitly instead of relying on X11 discovery:

```json
{
  "mcpServers": {
    "nobox": {
      "command": "nobox-agent",
      "args": ["--socket", "/run/user/1000/nobox/agent-seat-0.sock"]
    }
  }
}
```

Codex intentionally starts MCP servers with a restricted environment. Pass the
desktop variables through explicitly:

```toml
[mcp_servers.nobox]
command = "nobox-agent"
env_vars = ["DISPLAY"]
```

This is not needed for MCP initialization, discovery, or `tools/list`; those
remain available even without a running window manager. It is needed when a
tool actually connects to the desktop. An explicit `--socket` is the more
predictable choice when the host runs outside the graphical session.

### Seat tools and non-visual facts

The seat owns observation and mutation of the graphical session: window state,
pixels, pointer and keyboard input, and window-management actions. Exact facts
that are not represented by the desktop—URLs, service or channel identities,
feeds, APIs, files, builds, and version-control state—should come from ordinary
exact data sources instead of OCR. This boundary never permits another route to
reveal a hidden or out-of-scope window, bypass a seat refusal, or mutate the GUI
behind the seat.

## 3. Grant it something

A companion no grant names holds nothing at all. It connects, is told so, and
every request is refused. That is deliberate: installing a harness is not
consenting to it.

With `policy = "ask"`, the first connection raises a dialog nobox draws
itself, listing what is being asked for in plain terms. It holds the keyboard
while it is up:

- `y` — allow for this session only
- `p` — allow and remember, which writes a grant into your config file and
  applies it immediately, so the next connection is not asked again
- `n` or Escape — deny

A companion that asks for nothing is never asked about: the dialog exists to
put a request in front of a person, and an empty request has nothing to show.
`nobox-agent` asks for all six bundles, and you narrow it by answering, or by
writing a grant yourself.

To skip the dialog, write the grant yourself. It binds to the companion's
executable, so start by finding it:

```sh
command -v nobox-agent
```

```toml
[[agent.grants]]
label = "my harness"
executable = "/usr/bin/nobox-agent"
uid = 1000
capabilities = ["observe"]
```

Start with `observe` and add more only when you want it. The bundles are
`observe`, `accessibility`, `capture`, `input`, `manage`, and `launch`, and
individual atoms such as `observe.accessibility`, `manage.activate`, or
`capture.client_visible` work too. Nothing is implied: a session with
`observe` cannot read application semantics or move a window, and one with
`manage` cannot read a title unless it also has `observe.titles`.

Scoping a grant to one application makes every other window invisible to that
session rather than merely inert:

```toml
scope = { class = "Firefox" }
```

## 4. Check that it works

Before involving a host at all, ask the companion itself:

```sh
nobox-agent doctor
```

It walks the same path a host would, one stage at a time — which socket it
resolved and whether it exists, whether the manager answered, what the grant
came back as, how many tools that yields — and says what to do about whichever
stage failed. Run it first whenever a host reports that the server would not
start, because that message names the host rather than the cause.

To separate MCP startup from the seat entirely, this one-line modern discovery
probe also works with the desktop variables removed:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}' | env -u DISPLAY -u XDG_RUNTIME_DIR nobox-agent
```

It must return a JSON-RPC result naming the supported revisions. A failure here
is in MCP startup; a successful response followed by a failing `seat_status`
is socket, environment, manager, or grant configuration.

Nothing about the MCP handshake depends on any of it. A host can start this
companion, agree a revision, and list tools with the window manager switched
off entirely; the seat is reached on the first tool call, not at startup. That
is deliberate: a handshake that waited on a socket — or on a person answering a
consent dialog — would time out and be reported as a broken server.

Ask the harness what it can see. A good first request is the equivalent of
"list my windows", which uses `desktop_snapshot`. In nobox's log every request
is attributed:

```
INFO agent session greeted session=1 uid=1000 pid=4242
     executable=Some("/usr/bin/nobox-agent") harness=nobox-agent
     granted=[ObserveStructure, ObserveTitles] scoped=false
INFO agent request served session=1 tool="desktop.snapshot"
```

`server/discover` is static so a host can probe it without reaching the desktop
or raising a consent dialog. The `seat_status` tool performs that explicit
connection and reports the live grant, scope, manager, and backend features.

For a control or content item with an accessible name, use
`client_semantic_find` before capture. Keep its opaque continuation unchanged
when paging. Use `client_semantic_tree` only when nearby structure is needed,
and refresh with `client_semantic_root` after `stale_tree`. Roles are portable
categories rather than DOM types: a browser video may be a focusable `group`.
Choose an actionable match from its name, states, and non-empty
content-relative bounds. Run semantic tools sequentially: helper work is
single-flight, so concurrent excess fails closed as `semantic_unavailable`. If
semantics are unavailable, ambiguous, or omit the geometry needed for input,
use a grounded client capture; never guess a point. If capture says the window
is not rendered, restore or activate that same client first and re-observe it;
never substitute another window or reuse an old coordinate.

For a large pixel target, request a 100-pixel capture grid, identify the coarse
cell from its baked-in labels, then recapture that cell as a smaller `rect`
with a 50-pixel grid before clicking. Coordinates come from those labels plus
the reported origin, never from scaling the image as displayed by the harness.

Send a complete passage through one `client_type` call, with `\n` characters
for all line and paragraph breaks. `client_key` Return is for submitting or
activating a control, not for constructing multiline text one round trip at a
time. Text injection is paced between characters and remains preemptible.
Writes beyond 4,096 Unicode scalars use the exact-text transfer path instead;
one call accepts at most 32 KiB of UTF-8 and 16,384 scalars.

In `expects`, name only the facts an action depends on. `generation` covers
every descriptor-visible change, including a title update; geometry, workspace,
and focus are narrower checks for actions that do not depend on the title.

An `observe` block does not need a capture. Use event-only observation when an
input may close its target, such as accepting a transient dialog. If pixels of
the surviving application are needed, set `capture.client` to that stable
parent; capture scope, visibility, and sensitivity are rechecked at sample time.

## Keeping control

- **Stop everything now**: press the kill chord, Control + Alt + Escape by
  default. It freezes every session immediately and is handled ahead of all
  agent traffic, so it works even while a session is flooding the socket.
  Press it again to resume; the grant survives a freeze.
- **Take a capability back**: remove or edit the grant and reload — the
  settings application's **Agent seat** page lists stored grants with a remove
  button. Live sessions are re-evaluated immediately, not at their next
  connection.
- **Turn the seat off entirely**: set `enabled = false` and reload. The socket
  closes, the advertisement is withdrawn, and every session ends at once.
- **Keep a window private**: mark it in an application rule. A hidden window
  is absent from every answer, and asking about it returns exactly what asking
  about a window that never existed returns.

```toml
[[applications]]
match = { class = "Keepassxc" }
agent_visibility = "hidden"
```

- **Know when it is active**: a marker sits in the corner of the primary
  output while any session holds input or capture. A window that receives
  agent input is highlighted during the action and for 1.5 seconds afterward.
  Both are drawn by the window manager and cannot be covered or dismissed
  through the protocol.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| The host says the server "failed to start", or the handshake closed | Run `nobox-agent doctor`. Older companions refused `initialize`, or reached for the seat during it and timed out; both are fixed, so upgrade first |
| The host shows no tools at all | Older nobox: the companion only spoke the stateless revision and refused `initialize`. It now answers both, so upgrade the companion |
| The model does not reach for the seat unless told to | Confirm the discovery or `initialize` response carries `instructions`, then check the host's settings and transcript. Hosts may ignore this optional MCP guidance, so the individual tool descriptions also retain the essential routing cues |
| `_AGENT_SEAT` is absent | The seat is off, Nobox has not been reloaded since enabling it, or another provider owns the screen |
| "no live agent seat is advertised" | The host omitted `DISPLAY`, the selected screen has no provider, or its owner/root properties do not match; pass `DISPLAY` or an explicit socket |
| "cannot reach the agent seat at …" | The selected provider is gone or its socket is inaccessible; check selection ownership and both `_AGENT_SEAT` properties, then retry discovery |
| Every tool answers `denied` | No grant names this executable; check `command -v nobox-agent` against the `executable` in your config |
| `launch` answers `launch_denied` for a checked application | Both gates must be live: grant the companion launch access, choose **Save and apply**, and retry after Settings confirms the reload request |
| Tools answer `interrupted` | You were typing. The person at the keyboard has priority; the harness should wait |
| Tools answer `session_frozen` | The kill chord was pressed. Press it again to resume |
| A window is missing from snapshots | It is hidden by an application rule, or outside a scoped grant |
| Capturing a window says `unsupported` | It is minimized, so nothing is rendered anywhere, or it is covered and this server has no Composite extension |
| Semantic tools say `denied` | Add the independent `accessibility` bundle or `observe.accessibility` atom; ordinary `observe` and `capture` do not imply it |
| Semantic tools say `semantic_unavailable` | The toolkit/runtime did not expose a uniquely provable local tree, the client is sensitive, or the bounded helper failed; use grounded capture if granted |

## What the harness is told

The companion's `server/discover` and legacy `initialize` responses carry the
same compact, static instructions for the model. Their first 512 bytes identify
the live GUI boundary, name the snapshot and subscription entry points, and
explain when pixels are needed. The rest covers freshness preconditions and
refusals without repeating every tool description. Hosts are allowed to ignore
server instructions, so each tool description remains useful on its own; on a
host that supports them, you do not need to repeat this workflow in your prompt.
