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
the list of companions that hold a grant.

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

The companion finds the seat from `--socket`, then `AGENT_SEAT_SOCKET`, then
`$XDG_RUNTIME_DIR/nobox/agent-seat-<display>.sock`. It therefore needs the same
`DISPLAY` and `XDG_RUNTIME_DIR` as your session. If your harness runs
elsewhere — a service unit, a container, a different user — pass the socket
explicitly instead of hoping the environment matches:

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
`nobox-agent` asks for all five bundles, and you narrow it by answering, or by
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
`observe`, `capture`, `input`, `manage`, and `launch`, and individual atoms
such as `manage.activate` or `capture.client_visible` work too. Nothing is
implied: a session with `observe` cannot move a window, and one with
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

`server/discover` also reports the live grant, so a host that shows server
details will display exactly what the seat issued.

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
  output while any session holds input or capture, and the window being typed
  into is highlighted. Both are drawn by the window manager and cannot be
  covered or dismissed through the protocol.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| The host says the server "failed to start", or the handshake closed | Run `nobox-agent doctor`. Older companions refused `initialize`, or reached for the seat during it and timed out; both are fixed, so upgrade first |
| The host shows no tools at all | Older nobox: the companion only spoke the stateless revision and refused `initialize`. It now answers both, so upgrade the companion |
| The model does not reach for the seat unless told to | The host is not passing the server's instructions to the model. Check `nobox-agent doctor` and the host's own settings; the instructions ship in the `initialize` result |
| `_AGENT_SEAT` is absent | The seat is off, or nobox has not been reloaded since enabling it |
| "cannot reach the agent seat at …" | Wrong `DISPLAY`/`XDG_RUNTIME_DIR` for the harness, or the seat is off; pass `--socket` |
| Every tool answers `denied` | No grant names this executable; check `command -v nobox-agent` against the `executable` in your config |
| Tools answer `interrupted` | You were typing. The person at the keyboard has priority; the harness should wait |
| Tools answer `session_frozen` | The kill chord was pressed. Press it again to resume |
| A window is missing from snapshots | It is hidden by an application rule, or outside a scoped grant |
| Capturing a window says `unsupported` | It is minimized, so nothing is rendered anywhere, or it is covered and this server has no Composite extension |

## What the harness is told

The companion's `server/discover` response carries instructions written for
the model: prefer structured state over screenshots, carry the sequence
cursor, use freshness preconditions, and treat `interrupted`, `session_frozen`,
and `no_such_client` as decisions rather than obstacles to route around. You do
not need to repeat any of that in your own prompt.
