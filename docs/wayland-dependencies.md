# Wayland dependency record

This record accompanies W0 of the
[Wayland roadmap](wayland-roadmap.md). It describes the exact dependency and
host-library boundary actually compiled by the experimental backend. Update it
whenever a Smithay feature is enabled or a Wayland dependency changes.

## Rust dependencies

`Cargo.lock` pins Smithay 0.7.0. The workspace dependency disables Smithay's
default features and enables exactly:

| Feature | W0 use |
| --- | --- |
| `wayland_frontend` | Wayland display, compositor/subcompositor, SHM, output, and socket support |
| `use_system_lib` | System `libwayland-server` rather than the Rust-only server backend |
| `renderer_pixman` | Unsafe-free deterministic clear-color proof frame |
| `desktop` | Reserved now for the native shell types introduced in W2; it adds no dependencies in Smithay 0.7 |

W0 does not enable `backend_winit`, `backend_x11`, DRM, GBM, libinput, libseat,
XWayland, Vulkan, or GLES. The proof window is transported to isolated X11
with the workspace's existing x11rb dependency. This guarantees that the test
never falls back to the developer's host Wayland compositor and lets Nobox
remain free of `unsafe` blocks.

Smithay's low-level EGL display and GLES renderer constructors are unsafe in
0.7.0. Nobox will not wrap those calls locally. Before W4 selects an
accelerated renderer, it must identify a safe released API or obtain an
explicitly approved alternative; W0 does not prejudge that decision.

## System requirements

The W0 build requires development headers and pkg-config metadata for:

| pkg-config module/tool | Purpose |
| --- | --- |
| `wayland-server` | Native server backend selected by `use_system_lib` |
| `pixman-1` | Smithay software renderer |
| X11 protocol server (x11rb is pure Rust) | Nested proof transport |
| `Xvfb` or `Xephyr` and `xdpyinfo` | Isolated integration test |

The development host also has xkbcommon, libinput, libudev, libseat, libdrm,
GBM, EGL, GLES, XWayland, accessible DRM nodes, and an active logind graphical
seat. Those are inventory for later milestones, not W0 dependencies.

## License review

The normal dependency closure is reproducible with:

```sh
cargo tree --package nobox-wayland --edges normal --prefix none \
  --format '{p} {l}' | sort -u
```

At W0 the closure contains 102 unique package/version/license records. Every
package declares a license. Smithay, Wayland crates, Pixman bindings, and
calloop are MIT licensed; x11rb is `MIT OR Apache-2.0`; most utility crates are
MIT, Apache-2.0, or offer a permissive choice. The closure also contains
Apache-2.0-only packages through Smithay, including `cgmath` and `approx`.

That last fact matters because Nobox is GPL-2.0-only. The
[Free Software Foundation's compatibility guidance](https://www.gnu.org/licenses/license-compatibility.html)
considers Apache-2.0 incompatible with GPLv2 for a distributed combined
program. Nobox's supported distribution remains source-only, as the repository
guidance already requires; no binary release may be added on the strength of
this report. Before binary distribution or a licensing change is considered,
obtain a project-owner decision and qualified license review. This record is an
engineering inventory, not legal advice.

RustSec was run against `Cargo.lock` on 2026-08-14 with `cargo-audit 0.22.2`
and advisory database revision current that day. The full workspace lock has
two high-severity `quick-xml 0.36.2` advisories,
`RUSTSEC-2026-0194` and `RUSTSEC-2026-0195`, through the existing optional
AT-SPI semantic helper (`atspi` -> `zbus_xml`). That version is not in the
`nobox-wayland` dependency closure; Wayland resolves `quick-xml 0.41.0`.

The Wayland closure does include Smithay's `cgmath 0.18.0`, which RustSec marks
unmaintained and warns has an unsound `swap_columns` method
(`RUSTSEC-2026-0196` and `RUSTSEC-2026-0197`), plus the unmaintained
`paste 1.0.15` procedural macro (`RUSTSEC-2024-0436`). Smithay 0.7 contains no
call to `swap_columns`; Nobox neither calls nor exposes it. These warnings are
accepted for the W0 experimental proof, tracked as Smithay upgrade inputs, and
must be reconsidered before W9. Changes to `Cargo.lock` invalidate this audit.

## Verification commands

```sh
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cmake -S . -B build/wayland-w0 -G Ninja \
  -DNOBOX_BUILD_WAYLAND=ON
cmake --build build/wayland-w0
/usr/bin/ctest --test-dir build/wayland-w0 \
  -R '^wayland-w0-nested$' --output-on-failure
```

The CTest probe validates the exact four-global W0 protocol surface and ten
clean compositor lifecycles under Xvfb or Xephyr.
