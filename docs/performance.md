# Performance evidence

“Smaller and faster than Openbox” is a target to measure, not a label nobox can
give itself. The release build therefore has an opt-in, reproducible comparison:

```sh
cmake --preset release
cmake --build --preset performance
```

This builds only what the benchmark needs. To install nobox as well, build the
release preset itself: `cmake --build --preset release`.

The target runs the locally built release executable and the system `openbox`
on separate, newly created nested X servers. Each measurement starts a fresh
window manager, waits until it demonstrably manages and focuses an ordinary
client, removes that probe, records idle RSS, then maps 50 equal ordinary clients
from one X11 connection. It waits for both the complete EWMH client list and the
expected final active window before recording workload latency and loaded RSS.
Five raw runs and their arithmetic means are printed as tab-separated records;
no generated report is left in the source tree.

The defaults can be changed at configure time without editing the script:

```sh
cmake --preset release \
  -DNOBOX_PERFORMANCE_RUNS=10 \
  -DNOBOX_PERFORMANCE_CLIENTS=100 \
  -DNOBOX_PERFORMANCE_WORKLOAD=smart
cmake --build --preset performance
```

`smart` maps ordinary unpositioned 240×140 clients and includes each manager's
placement policy. `positioned` supplies deterministic ICCCM positions; it is an
isolation aid for distinguishing placement from framing, focus, and protocol
cost. `smart` is the default end-user comparison.

The workload is deliberately end-to-end. It includes X11 metadata discovery,
policy, smart placement, frame creation, rendering requests, focus, stacking,
and EWMH publication. The startup number means “first client managed,” not the
earlier and weaker condition that a support property exists. Openbox publishes
that property before it is ready to process a new map on this system, which is
why the report uses a disposable managed-client probe.

## Reading the result

The report keeps executable size and resolved shared-object size separate. A
Rust executable contains more of its implementation, while the dynamically
linked Openbox package delegates much more to system libraries. Raw executable
bytes therefore answer only one part of “small”; resolved dependency bytes are
also not per-process private memory because shared pages can be shared. RSS is
reported alongside both, and is the most direct runtime footprint observation
available from `/proc` in this harness.

Latency and RSS vary with the host, X server, allocator, kernel, package build,
and background load. The raw runs expose that variation. The report intentionally
has no relative pass/fail threshold: CI noise must not turn performance evidence
into a flaky correctness gate. Regressions should be investigated by comparing
the same host and build conditions.

## Current local observation

On 2026-08-05, using Xnest on Linux 6.18.35, the stripped nobox 0.1.0 release
build and the distribution's Openbox 3.6.1 produced this result over five smart
50-client runs:

| Metric | nobox | Openbox 3.6.1 |
|---|---:|---:|
| Executable | 3,941,224 B | 403,896 B |
| Resolved shared objects | 3 / 2,792,368 B | 69 / 42,963,728 B |
| First-client readiness, mean | 35.7 ms | 54.9 ms |
| Idle RSS, mean | 5,641 KiB | 29,136 KiB |
| 50-client RSS, mean | 5,986 KiB | 29,654 KiB |
| 50-client management and final focus, mean | 29.5 ms | 140.5 ms |

This supports lower runtime-memory, faster-startup, and faster equal-workload
claims on that host, but not a smaller-executable claim. The executable/dependency
split remains visible rather than selecting only favorable size metrics.
`SESSION_MANAGER` was absent for this default-path run. The optional XSMP
companion is a separate 17,864-byte executable on this build and is not a
shared-object dependency or process in an ordinary X11 session.
The default manager now has four threads: main, signal forwarding, runtime
deadlines, and child reaping. The reaper blocks without polling while no
executed children are outstanding, preventing zombie accumulation without
adding idle wakeups.

A later hardening pass removed steady-state work the 50-client benchmark does
not isolate: pointer-motion reports are coalesced per burst like Openbox, a
move drag no longer reissues identical decoration-child requests (previously
about twenty per motion event), policy restacking computes each client's
effective layer once per pass instead of once per candidate comparison with no
per-visit allocations, the EWMH client lists are rewritten only when their
content changes, geometry scoring uses single hardware multiplies instead of
128-bit soft arithmetic, desktop-entry discovery parses without per-key
allocations, and the panel batches its per-window property reads into one
pipelined round trip and skips repaints whose content is unchanged. Event-loop
output is now flushed once per event burst rather than once per event.

The earlier improvement came from backend lifecycle work rather than weakening
policy:
initial X11 metadata requests are issued as one pipeline; focus changes repaint
only the old and new frames; a new client is inserted relative to an already
verified policy order with a full-restack fallback; redundant colormap installs
are skipped; and expose sequences draw only their final event. Consecutive new
maps retain only their final eligible focus request until the event queue drains,
with a 256-event starvation bound. Direct user input cancels that pending focus,
so a startup burst cannot steal focus after the user has acted. The measured
endpoint requires that final focus to be complete.

These numbers are a dated observation, not a permanent benchmark result. Run
the target again after compiler, dependency, Openbox package, or policy changes.

## Native Wayland profile

The Wayland profile measures its own backend rather than comparing unlike X11
and compositor workloads:

```sh
cmake --build --preset release
NOBOX_XSERVER=xvfb \
  cmake --build build/release --target wayland-performance-report
```

Each run starts a fresh nested compositor, records ready-socket latency and
idle `/proc` resources, then maps one SHM client and measures 120 explicitly
requested frame callbacks. It records loaded RSS, thread/file-descriptor
counts, and p50/p95/maximum callback latency before requesting a clean exit and
proving socket cleanup. Like the X11 report, it leaves no generated source-tree
artifact and has no noise-sensitive pass threshold.

On 2026-08-15, the debug build on Linux 6.18.39 under Xvfb produced five runs:

| Metric | Arithmetic mean |
| --- | ---: |
| Ready socket | 584.9 ms |
| Idle RSS | 160,646 KiB |
| Loaded RSS | 183,990 KiB |
| Threads | 70 |
| File descriptors | 37 |
| Frame callback p50 | 2.301 ms |
| Frame callback p95 | 3.139 ms |
| Per-run maximum, mean | 4.477 ms |

The first run paid additional cold-start cost (911.9 ms); the remaining four
were 484.6–520.5 ms. Debug-build RSS and nested-X11 callback latency are useful
regression baselines, not direct-session performance claims. Real KMS timing is
recorded only by the guarded hardware acceptance procedure.

On 2026-08-18, after the first physical-session corrections in
`nobox-wayland` 0.2.64, the same debug/Xvfb method produced:

| Metric | Arithmetic mean |
| --- | ---: |
| Ready socket | 449.3 ms |
| Idle RSS | 162,761 KiB |
| Loaded RSS | 186,314 KiB |
| Threads | 70 |
| File descriptors | 38 |
| Frame callback p50 | 1.818 ms |
| Frame callback p95 | 2.365 ms |
| Per-run maximum, mean | 3.486 ms |

The corrected scene order keeps overlays above client surfaces and the cursor
above overlays. To preserve effective opaque-region culling, the small overlay
primitives are treated as nonopaque while they are composited in that explicit
front-to-back order. The ten-cycle managed-shell liveness regression and this
profile remained responsive after the correction. These remain nested debug
observations, not direct KMS performance claims.

The same five-run debug/Xvfb profile on 2026-08-18 for the installed
`nobox-wayland` 0.2.66 build produced:

| Metric | Arithmetic mean |
| --- | ---: |
| Ready socket | 454.0 ms |
| Idle RSS | 163,038 KiB |
| Loaded RSS | 186,610 KiB |
| Threads | 71 |
| File descriptors | 44 |
| Frame callback p50 | 1.748 ms |
| Frame callback p95 | 2.010 ms |
| Per-run maximum, mean | 2.842 ms |

All five runs completed their 120 requested callbacks, clean exit, and socket
cleanup. This refresh covers the live multi-output selector correction without
changing the evidence boundary: only the guarded physical run may establish
KMS timing or device-lifecycle behavior.

The subsequent 2026-08-18 LightDM dogfood session found that the direct backend
was unconditionally requesting another redraw at every KMS vblank. With two
2560x1600 outputs, the compositor held roughly 64% CPU at rest and libinput
reported 26–44 ms processing delays. `nobox-wayland` 0.2.67 removes that idle
frame chain: a vblank retires the submitted frame and its callbacks, while a
new client commit, cursor move, overlay change, or other scene damage requests
the next frame. This is an evidence-backed scheduling correction, but its
physical idle-CPU result remains unclaimed until the installed build is tested
again; the nested profile does not substitute for that measurement.
