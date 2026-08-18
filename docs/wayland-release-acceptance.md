# Wayland release acceptance

Status: automated W9 acceptance complete on 2026-08-16; an initial direct
LightDM dogfood run completed on 2026-08-18, while the guarded real-hardware
record remains the sole release blocker.

This record maps the nine end-result statements in
[wayland-roadmap.md](wayland-roadmap.md) to evidence without treating a nested
compositor as proof of DRM/KMS lifecycle.

| Result | Evidence |
| --- | --- |
| Explicit dual backend | CLI refusal/doctor tests, distinct installed X11 and Wayland session entries, and staged smoke runs of both backends. |
| Nested and direct mechanics | Nested renderer/protocol suite plus read-only TTY doctor; final DRM, hotplug, suspend, VT, mixed-output, cursor, and cleanup proof is the outstanding guarded hardware record. |
| Shared policy | Core unit tests and the ten-cycle managed-shell test cover native actions, menus, rules, workspaces, restoration, panel behavior, and output fallback; XWayland lifecycle uses the same identities/actions. |
| Daily protocols | Exact advertised versions, hostile resource limits, toolkit clients, selection/DND, activation, input extensions, scaling/presentation, idle, and lock are exercised by `wayland-managed-shell`. |
| Optional XWayland | Clean no-XWayland build plus runtime disable/re-enable, crash/restart, selection/DND, groups, live modal add/remove focus redirection, activation, focus, and cleanup tests. |
| Separate panel | Layer-shell/foreign-toplevel/workspace protocols, interaction, failed replacement retention, crash isolation, and recovery are in the managed-shell fixture. |
| Agent Seat parity | Foundation and accessibility tests cover stock MCP discovery, grants/consent/revoke, structure/events, management, launch, capture, input, human preemption, freeze, flood shedding, and native AT-SPI correlation. |
| Failure isolation | Protocol exhaustion, invalid imports/locks, renderer fallback, panel failure, XWayland crash, Agent Seat flood/helper failure, repeated shutdown, and socket cleanup remain green. Direct seat/device recovery awaits the hardware record. |
| Build/install/diagnosis | Default dual-backend, X11-only, and no-XWayland builds; TTY/nested doctors; source audit; staged install of binaries, both sessions, config, and documentation; nested smoke from the staged prefix. |

## Automated commands

```sh
cmake --preset dev
cmake --build --preset dev
cmake --build --preset check
NOBOX_XSERVER=xvfb /usr/bin/ctest --preset dev --output-on-failure

cmake -S . -B build/w9-x11-only -G Ninja \
  -DNOBOX_BUILD_WAYLAND=OFF -DNOBOX_BUILD_XWAYLAND=OFF
cmake --build build/w9-x11-only
cmake --build build/w9-x11-only --target check
NOBOX_XSERVER=xvfb \
  /usr/bin/ctest --test-dir build/w9-x11-only --output-on-failure

cmake -S . -B build/w9-wayland-no-xwayland -G Ninja \
  -DNOBOX_BUILD_WAYLAND=ON -DNOBOX_BUILD_XWAYLAND=OFF
cmake --build build/w9-wayland-no-xwayland
cmake --build build/w9-wayland-no-xwayland --target check
NOBOX_XSERVER=xvfb \
  /usr/bin/ctest --test-dir build/w9-wayland-no-xwayland --output-on-failure

NOBOX_XSERVER=xvfb \
  cmake --build --preset dev --target wayland-performance-report
python3 tests/wayland-release-audit.py .
cargo audit
```

The 2026-08-16 source audit confirmed the exact reviewed Smithay source and
feature allowlist, 230 licensed normal dependency records, no unsafe Nobox Rust,
redacted Wayland tracing fields, and the explicit installed TTY command. A
fresh `cargo-audit` 0.22.2 run found and prompted removal of the vulnerable
`quick-xml 0.36.2` lockfile path; the final run reports zero vulnerabilities
and four documented informational warnings. Performance method and observations
are in [performance.md](performance.md).

The 2026-08-18 post-fix gate passed the default 65-test suite, the 56-test
X11-only configuration, and the 64-test Wayland-without-XWayland configuration;
only the four documented missing-Xvfb environment skips appeared where
applicable. `cargo-audit` 0.22.2 scanned 385 dependencies against 1,217
advisories with zero vulnerabilities and the same four allowed unmaintained or
unsound informational warnings. A fresh five-run nested profile for
`nobox-wayland` 0.2.64 is recorded in [performance.md](performance.md).

## Remaining hardware record

The 2026-08-18 preliminary run proved that LightDM can start Nobox directly on
the development workstation's NVIDIA GeForce GTX 1660 SUPER and two physical
outputs. It also found actionable menu-output, overlay-order, cursor-theme,
VT-switch, and XWayland-readiness defects. The session remained alive until a
remote LightDM restart. These observations and the intentionally reduced next
run are recorded in [wayland-hardware-acceptance.md](wayland-hardware-acceptance.md);
they do not claim hotplug, suspend/resume, rollback, or device-recovery proof.

Run [wayland-hardware-acceptance.md](wayland-hardware-acceptance.md) from a
disposable local VT with two physical outputs. The record must name its date,
host, GPU, connectors, kernel, and retained path and must finish every human and
automated checkpoint. Until that exists:

- `BackendCapabilities::direct_session` remains false;
- this document remains pre-release despite green nested/install evidence; and
- no statement claims that nested X11 proves DRM master, VT, suspend, hotplug,
  KMS rollback, or device cleanup.
