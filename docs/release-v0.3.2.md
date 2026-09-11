# Nobox 0.3.2

Active-window screenshots now show a two-pixel black-and-white outline for
180 ms after successful file or clipboard delivery. Four input-transparent
strips leave the center untouched. `--no-flash` disables feedback, and stdout
capture is always visually silent. The saved image contains no outline.

Frame exposure now repaints the border before the title. This fixes decoration
damage after an outline or another overlapping window disappears.

Installed X11 session entries now select the absolute launcher belonging to
their installation prefix. This fixes a source-install trap where an older
`/usr/local/bin/nobox` could shadow a newer `/usr/bin/nobox` even after reboot.
An older running manager can retain `gnome-screenshot -w` as its Alt+Print
default; explicit screenshot commands and Reconfigure update that session,
while the corrected launcher takes effect at the next login.

The `nobox-screenshot` and `nobox-x11` crates advance to 0.3.2. Other crate
versions are unchanged. This is a source-only release.

The shared Nobox/Openbox regression checks decorated and fullscreen clients,
direct and Alt+Print capture, `--no-flash`, and stdout. It checks live pixels,
visibility events, focus, outline geometry and input shapes, and byte-identical
PNG output before and after feedback. The installation regression verifies
that an overridden install prefix is used in the X11 login entry.

Verification: CMake/Ninja development and release builds, formatting, Clippy
with warnings denied, and 488 Rust tests passed. All 73 CTest cases passed
across the full run and focused reruns. The full run exposed an existing
Openbox/Xnest fixture teardown race; waiting for withdrawal before client-ID
reuse passed five consecutive workspace-comparison runs. Release binaries also
passed all three screenshot integration cases.
