# Nobox 0.1.2

Nobox 0.1.2 completes separation milestone N2. It renames Nobox's internal
GPL-2.0-only Agent Seat wire implementation from `agent-seat-proto` to
`nobox-agent-wire` and renames its private integration-test probe accordingly.

This is an implementation-identity change, not a protocol change. The
`agent-seat` protocol name, wire revision 2, serialized values, framing,
socket location, grants, and behavior are unchanged. No compatibility crate is
published under the former name, and Nobox has no source or build dependency
on the future independent Apache-2.0 `ZaguanLabs/agent-seat-proto` product.

## Versions

- Nobox shared workspace crates: 0.1.2
- `nobox-config`: 0.1.2
- `nobox-agent`: 0.1.6
- `nobox-agent-wire`: 0.1.6
- `nobox-x11`: 0.1.7
- Agent Seat wire revision: 2 (unchanged)

## Verification

The release passed the complete developer gate:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

The source rename was also checked against the v0.1.1 implementation to verify
that protocol constants and serialized Rust definitions changed only in their
internal package/import spelling.
