# Nobox 0.3.3

Fixes a login regression introduced in 0.3.2: choosing Nobox on Mageia could
start IceWM instead. The installed X11 desktop entry unnecessarily quoted its
absolute executable path. Mageia's Xsession looks up a single command before
shell parsing, treats the quotes as part of its name, and selects a fallback.

Ordinary installation paths now remain unquoted. The absolute launcher path
still prevents older installations on PATH from shadowing the selected build;
prefixes that need quoting retain it. Reinstall and log in again to apply the
correction. The current graphical session does not need to be restarted during
installation.

The staged-install regression checks Mageia-style session selection and runs
the generated desktop command on an isolated X server. The old 0.3.2 entry
fails this check before Nobox starts. The `nobox-x11` patch version advances to
0.3.3; other crate versions are unchanged. This is a source-only release.

Nested tests now isolate persistent state as well as runtime sockets. The
runtime-control test uses `--config`, which does not relocate session state;
without a private state directory it could overwrite the user's saved window
layout on shutdown. The shared helper now sets a private `XDG_STATE_HOME` and
clears inherited Nobox configuration and state overrides.

Verification: development and release builds, formatting, Clippy, and Rust
tests passed. All 73 CTest cases passed across the full run and a focused rerun
of the browser-accessibility case, which failed its first attempt. The actual
installed Mageia LightDM wrapper reproduced IceWM with the old command and
Nobox with the corrected command on an isolated Xvfb display. The installed
0.3.3 entry and an installation prefix containing spaces passed nested startup.
The runtime-control regression also verifies that both backends preserve a
simulated user's state file while saving their own state privately.
