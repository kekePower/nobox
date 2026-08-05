# Performance evidence

“Smaller and faster than Openbox” is a target to measure, not a label nobox can
give itself. The release build therefore has an opt-in, reproducible comparison:

```sh
cmake --preset release
cmake --build --preset performance
```

The target runs the locally built release executable and the system `openbox`
on separate, newly created nested X servers. Each measurement starts a fresh
window manager, waits until it demonstrably manages an ordinary client, removes
that probe, records idle RSS, then maps 50 equal ordinary clients from one X11
connection. It waits for the complete EWMH client list before recording workload
latency and loaded RSS. Five raw runs and their arithmetic means are printed as
tab-separated records; no generated report is left in the source tree.

The defaults can be changed at configure time without editing the script:

```sh
cmake --preset release \
  -DNOBOX_PERFORMANCE_RUNS=10 \
  -DNOBOX_PERFORMANCE_CLIENTS=100
cmake --build --preset performance
```

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
build and the distribution's Openbox 3.6.1 showed a mixed result over five
50-client runs:

| Metric | nobox | Openbox 3.6.1 |
|---|---:|---:|
| Executable | 3,379,112 B | 403,896 B |
| Resolved shared objects | 3 / 2,792,368 B | 69 / 42,963,728 B |
| First-client readiness, mean | 29.5 ms | 55.1 ms |
| Idle RSS, mean | 5,083 KiB | 28,254 KiB |
| 50-client RSS, mean | 6,599 KiB | 28,756 KiB |
| 50-client management, mean | 332.7 ms | 116.9 ms |

This supports a lower runtime-memory and faster-startup claim on that host, but
not a smaller-executable or faster-bulk-management claim. The bulk gap is a
real optimization target, not something hidden by selecting only favorable
metrics. Nobox's least-overlap placement scorer now abandons candidates once
they cannot beat the current best; further profiling must distinguish remaining
placement work from the many initial X11 metadata round trips.

These numbers are a dated observation, not a permanent benchmark result. Run
the target again after compiler, dependency, Openbox package, or policy changes.
