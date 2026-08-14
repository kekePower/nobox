# Wayland dependency record

This record accompanies the
[Wayland roadmap](wayland-roadmap.md). It describes the exact dependency and
host-library boundary compiled by the managed nested backend through the W3
desktop-policy foundation.
Update it whenever a Smithay feature is enabled or a Wayland dependency
changes.

## Rust dependencies

`Cargo.lock` pins Smithay 0.7.0. The workspace dependency disables Smithay's
default features and enables exactly:

| Feature | Current use |
| --- | --- |
| `wayland_frontend` | Wayland display, compositor/subcompositor, SHM, output, xdg-shell, seat, and socket support |
| `use_system_lib` | System `libwayland-server` rather than the Rust-only server backend |
| `desktop` | Toplevel/popup scene management and focus-aware surface lookup |
| `backend_winit` | Safe nested window/input integration for the primary renderer |
| `renderer_gl` | GLES2 client-surface and server-decoration rendering |
| `renderer_pixman` | Deterministic software fallback and forced regression path |

W2 does not enable Smithay's `backend_x11`, DRM, GBM, libinput, libseat,
XWayland, or Vulkan features. Both renderer paths are transported to isolated
X11; the GLES2 path uses Smithay's safe winit entry point and the Pixman path
uses the workspace's existing x11rb dependency. Nobox itself remains free of
`unsafe` blocks.

W3 directly uses `wayland-protocols 0.32.13` with its client, server, and
staging modules for `xdg-activation`, `ext-foreign-toplevel-list`, and
`ext-workspace-v1`. It uses `wayland-protocols-wlr 0.3.12` with client support
for the deterministic layer-shell probe; Smithay owns the corresponding server
dispatch. These additions enable no new Smithay backend or renderer feature.

Smithay's low-level EGL display and GLES renderer constructors remain unsafe in
0.7.0, so Nobox does not call them. W2 reaches GLES2 only through Smithay's
safe `backend_winit` initialization. W4 must retain this rule when selecting a
direct DRM/KMS renderer or obtain an explicitly approved alternative.

## System requirements

The W2 build requires development headers and pkg-config metadata for:

| pkg-config module/tool | Purpose |
| --- | --- |
| `wayland-server` | Native server backend selected by `use_system_lib` |
| `pixman-1` | Smithay software renderer |
| `xkbcommon` | Seat keyboard maps and state |
| `egl`, `glesv2` | Safe Smithay/winit GLES2 path |
| X11 client libraries and protocol server | Nested winit/x11rb transport |
| `Xvfb` or `Xephyr`, XTEST, and `xdpyinfo` | Isolated input/render integration test |

The development host also has xkbcommon, libinput, libudev, libseat, libdrm,
GBM, XWayland, accessible DRM nodes, and an active logind graphical seat. Those
remain inventory for later milestones, not W2 dependencies.

## License review

The normal dependency closure is reproducible with:

```sh
cargo tree --package nobox-wayland --edges normal --prefix none \
  --format '{p} {l}' | sort -u
```

At the W3 desktop-policy foundation the closure contains 175 unique
package/version/license records. Every
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

RustSec was run against the W0 `Cargo.lock` on 2026-08-14 with `cargo-audit`
0.22.2 and advisory database revision current that day. The full workspace lock had
two high-severity `quick-xml 0.36.2` advisories,
`RUSTSEC-2026-0194` and `RUSTSEC-2026-0195`, through the existing optional
AT-SPI semantic helper (`atspi` -> `zbus_xml`). That version is not in the
`nobox-wayland` dependency closure; Wayland resolves `quick-xml 0.41.0`.

The Wayland closure does include Smithay's `cgmath 0.18.0`, which RustSec marks
unmaintained and warns has an unsound `swap_columns` method
(`RUSTSEC-2026-0196` and `RUSTSEC-2026-0197`), plus the unmaintained
`paste 1.0.15` procedural macro (`RUSTSEC-2024-0436`). Smithay 0.7 contains no
call to `swap_columns`; Nobox neither calls nor exposes it. These warnings are
accepted for the managed nested backend, tracked as Smithay upgrade inputs, and
must be reconsidered before W9. W2 changed `Cargo.lock`, so this historical
audit is no longer a current lockfile audit; `cargo-audit` is not presently
installed on the development host and a fresh audit remains mandatory before
W9.

## Verification commands

```sh
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cmake -S . -B build/wayland-w0 -G Ninja \
  -DNOBOX_BUILD_WAYLAND=ON
cmake --build build/wayland-w0
/usr/bin/ctest --test-dir build/wayland-w0 \
  -R '^wayland-managed-shell$' --output-on-failure
```

The CTest probe validates the managed shell's exact protocol globals, forced
GLES2 and Pixman rendering, layer-shell configure/render/unmap behavior,
serial-authorized activation, atomic workspace publication and switching,
malformed-client isolation, and ten clean compositor lifecycles under Xvfb or
Xephyr.
