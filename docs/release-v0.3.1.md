# Nobox 0.3.1

Nobox 0.3.1 fixes application-frame flicker when switching X11 workspaces with
many windows loaded. Incoming frames now map from top to bottom before outgoing
frames hide from bottom to top. Layer enforcement places lower frames beneath
their final higher siblings, avoiding temporary raises that exposed covered
applications.

The regression reproduced 600 transient exposures across 30 workspace switches
before the fix. Nobox and Openbox now both report zero exposures across 90
switches with 38 clients, including ordinary, maximized, and fullscreen covering
windows, a sticky client, and a minimized client. The test records intermediate
X11 visibility events and checks final frame visibility and restored focus.

This source release also includes the changes accumulated since 0.3.0:

- `nobox-screenshot` provides full-screen, active-window, area, pointer, delay,
  file, stdout, and clipboard capture, with PNG/JPEG and JPEG quality controls.
- The default screenshot command uses `nobox-screenshot`.
- Source installation packages end-user documentation without the internal
  roadmap and historical acceptance and release-note archive.

The `nobox-x11`, `nobox-config`, and `nobox-screenshot` crates are version 0.3.1;
other existing crates remain at 0.3.0. Installation remains source-based.

Verification completed through the CMake/Ninja developer presets: build,
formatting, Clippy with warnings denied, and 487 passing Rust tests. All 71
CTest cases passed across the full suite and focused reruns after correcting
extension probes that falsely skipped available XTest and Sync checks.
