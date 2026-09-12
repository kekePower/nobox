# Nobox 0.3.4

Screenshot feedback is easier to notice: successful active-window captures
retain the thin black-and-white outline for 400 ms, and successful file or
clipboard captures request the desktop theme's shutter sound. `--no-flash`
and `--no-sound` control these independently. Stdout stays silent. Feedback
appears only after delivery and does not alter focus, live content, or saved
pixels. Optional `canberra-gtk-play` supplies the sound; missing, failed, or
stuck playback cannot fail a capture.

`nobox-screenshot -i` now opens a small native GTK window with Screen, Window,
and Selection choices, pointer inclusion, delay, and feedback switches.
`-i -w` and `-i -a` preselect a mode. Cancel or Escape closes the chooser without
capturing; save failures appear in an error window. The chooser withdraws
before the capture delay, allowing focus and underlying content to recover.
Running without options still captures the whole screen.

Escape during area selection also cancels quietly and erases the drag outline,
without writing a file, playing feedback, or opening an error window.

CMake enables the chooser when GTK 4.10 development files are present, with
`-DNOBOX_BUILD_SCREENSHOT_GUI=OFF` available for toolkit-free screenshot builds.
Direct Cargo builds opt in with `--features gui`. This optional interface adds
no toolkit dependency or failure path to the window manager.

The `nobox-screenshot` crate advances from 0.3.2 to 0.3.3. Other crate versions
are unchanged. This is a source-only release.

Verification: CMake/Ninja development and release builds, formatting, Clippy
with warnings denied, and 489 Rust test executions passed. All 75 CTest cases
passed across the full run and one focused rerun of the existing browser
accessibility test, which initially could not act on its video control.
The final release binary also passed all five screenshot integration tests
under Nobox and Openbox. Toolkit-free builds passed their unit and nested-X
capture checks. Both window-manager binaries retain no GTK/libadwaita runtime
dependency.
