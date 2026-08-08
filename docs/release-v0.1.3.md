# Nobox 0.1.3

Nobox 0.1.3 completes separation milestone N3 by adopting the frozen atomic
Agent Seat provider-ownership and discovery contract.

The integrated seat now claims `_AGENT_SEAT_S<screen>` on a dedicated owner
window before accepting peers. It publishes the same bounded three-field
advertisement on that window and the root, announces acquisition with
`MANAGER`, refuses a live competing provider, and treats selection loss as a
seat failure rather than a window-manager failure. Cleanup removes the root
property only while it still equals Nobox's own value.

`nobox-agent` now resolves `--socket`, then `AGENT_SEAT_SOCKET`, then a live X11
selection-bound root advertisement. It validates the current owner twice,
requires byte-identical owner/root properties with the specified type, format,
size, canonical protocol encoding, revision, and absolute local-socket bound,
and no longer synthesizes a Nobox runtime path.

The Agent Seat wire remains revision 2. Framing, grants, tools, and Tier 1
behavior are unchanged.

## Versions

- Nobox shared workspace crates: 0.1.3
- `nobox-agent`: 0.1.7
- `nobox-x11`: 0.1.8
- Agent Seat wire revision: 2 (unchanged)

## Verification

The release passed the complete developer gate:

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

The nested-X Agent Seat test additionally covers no owner, dedicated Nobox
ownership, explicit/environment/root precedence, stale and mismatched
advertisements, duplicate refusal, forced selection loss, unchanged-config
recovery, crash residue, clean withdrawal, and continued window management.
