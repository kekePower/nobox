# Nobox v0.1.1 source release

Nobox v0.1.1 is the hardened Agent Seat Protocol revision 2 baseline and the
last Nobox release before the planned internal wire-crate rename. It remains a
source-only X11 window-manager release; Tier 0 and the independent
`agent-seat-proto` product are not part of it.

## Release outcomes

- `nobox-settings` exposes deny-all, selected-only, and
  all-installed-except-selected launch modes over the bounded XDG application
  catalog. User-installed entries require a separate opt-in, and unknown
  configured IDs are preserved.
- The manager, neutral wire crate, MCP companion, and disposable semantic
  helper implement Agent Seat wire revision 2 with bounded semantic root,
  tree, and search operations, machine-correctable errors, human preemption,
  runtime revocation, and native MCP image delivery.
- Supported Firefox-family semantics are tested at 150% CSS scaling across a
  wide/narrow/wide responsive reflow. GTK and Qt root/search paths are each
  repeated three times. Chromium repeats the fixed-deadline unavailable path
  and grounded capture; Electron remains safe-unavailable live evidence rather
  than advertised semantic support.
- Canvas-only content has no fabricated semantic identity: a bounded search
  returns no match and the workflow takes one grounded capture. A hidden-
  workspace client must be restored before capture.
- Client capture now redirects through Composite when any requested pixels lie
  outside the root, fixing capture of otherwise visible edge windows after a
  resize.

## Measurement summary

All isolated measurements use a private D-Bus session and nested X server. The
tests inspect typed geometry and byte counts, not image content.

| Runtime/task | Semantic work | Grounded capture |
| --- | --- | --- |
| Firefox video, 3 reflows | 2 calls, 705--757 JSON B, 2,401 ms; bounds `658 -> 318 -> 658` | 1 call, 49,867--54,840 PNG B, 247--366 ms |
| Firefox canvas-only, 3 runs | 1 call, 66 JSON B, 1,200 ms, no match | 1 call, 49,867--54,564 PNG B, 247--348 ms |
| GTK, 3 runs | 2 calls, 532 JSON B, 2,401 ms | 1 call, 56,124 PNG B, 205--208 ms |
| Qt, 3 runs | 2 calls, 536 JSON B, 2,401 ms | 1 call, 5,363 PNG B, 78--83 ms |
| Chrome, 3 runs | 1 call, 111 JSON B, 1,200 ms, unavailable | 1 call, 25,620 PNG B, 211--219 ms |

Read-only live dogfood repeated the unavailable-then-capture path on both
2560x1600 outputs, including the non-zero `(2560,0)` origin. The MCP-native
structured capture metadata was 190 bytes; PNG size varied with the displayed
page. A hidden-workspace Beeper window repeated the unavailable result and the
typed not-rendered capture refusal without exposing application text.

## Compatibility

Agent Seat wire revision 2 is intentionally incompatible with wire revision 1.
There is no silent downgrade, duplicated payload, or prose compatibility
adapter. A companion and provider must agree on revision 2 or refuse the
connection. This is separate from MCP initialization: `nobox-agent` still
accepts its documented deployed MCP handshake revisions and translates them to
the one current seat contract.

Existing Nobox configuration remains valid. Agent access is still disabled by
default, existing grants remain executable-bound, and launch policy remains
deny-by-default unless the user selects another mode.

## Build and verify

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
/usr/bin/ctest --preset dev --output-on-failure
```

The GitHub tag and release archives are source artifacts. Binary distribution
remains outside the supported scope.
