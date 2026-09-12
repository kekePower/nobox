# nobox-screenshot

`nobox-screenshot` is Nobox's small X11 screenshot utility. Its familiar
command-line options follow `gnome-screenshot` 41, while `--format` and
`--quality` make compact JPEG captures explicit and testable.

```sh
# Lossless PNG in the Pictures directory (the default)
nobox-screenshot

# Choose Screen / Window / Selection, pointer, delay, and capture feedback
nobox-screenshot -i

# Active window as JPEG at the recommended agent-oriented starting point
nobox-screenshot --window --format jpeg --quality 75 --file window.jpg

# Drag out an area, include the cursor, and write image bytes to stdout
nobox-screenshot --area --include-pointer --quality 70 --stdout > area.jpg
```

Supported compatibility options are `--clipboard`, `--window`, `--area`,
`--include-border`, `--remove-border`, `--include-pointer`, `--delay`,
`--border-effect`, `--interactive`, `--file`, `--version`, and `--display`.
As in `gnome-screenshot` 41, border switches and effects are deprecated and do
not alter the capture. `--interactive` (`-i`) opens a small native GTK options
window. Use `-i -w` or `-i -a` to preselect Window or Selection. The chooser
honors pointer, delay (0–3600 seconds), and feedback flags; file, format,
quality, clipboard, and stdout options still control delivery. Cancel or Escape
closes the chooser without capturing. Save failures appear in an error window.
Escape during area selection also exits quietly, erasing any drag outline.

The chooser closes before the delay starts, with at least 250 ms for redraw and
focus restoration. Window mode captures the window active when that delay ends;
Selection starts the drag selector after the delay. `--display` puts both the
chooser and capture on the requested X display.

CMake enables the chooser when GTK 4.10 development files are available.
`-DNOBOX_BUILD_SCREENSHOT_GUI=OFF` keeps a toolkit-free screenshot binary; `-i`
then explains how to enable the chooser. Direct Cargo builds opt in with
`cargo build -p nobox-screenshot --features gui`. The window manager never
depends on GTK or the screenshot process.

After a successful active-window file or clipboard capture, a black-and-white
outline marks the captured perimeter for 400 ms. Four thin, input-transparent
windows leave the center untouched; the application is never hidden or made
transparent. The outline appears after delivery, so it is absent from the saved
image. `--no-flash` suppresses it.
Feedback requires Shape 1.1 and remains best-effort if unavailable.

Successful file and clipboard captures also request the desktop theme's
`screen-capture` shutter sound through optional `canberra-gtk-play`.
`--no-sound` suppresses sound independently of the outline. Missing audio
support, muted sound, and player failures never fail the capture; a stuck player
is stopped after two seconds. No sound plays on failed delivery. Stdout capture
always suppresses both sound and outline. The chooser exposes both controls;
the outline control applies only to Window captures.

`--quality` accepts 1 through 100 and controls JPEG quantization. Supplying it
without `--format` or a filename extension selects JPEG; PNG remains the
lossless default. `--format png --quality ...` is rejected so a requested
size/quality change can never be silently ignored. JPEG quality 75 is the
default and the recommended first measurement point; UI text and model
accuracy should still be evaluated against each real workload.

JPEG quality reduces a JPEG's file and Base64 size, transfer time, and memory.
JPEG is often smaller for photographic, video, or gradient-heavy captures, but
PNG can be smaller for sparse text and flat-color UI. In a nested Nobox test,
the same terminal capture was 29,053 bytes at JPEG 60, 34,712 bytes at JPEG 80,
and only 21,300 bytes as PNG; all text remained readable at both JPEG levels.
Measure representative screens instead of assuming one format wins.

Encoding quality does not by itself reduce vision-model image tokens when the
decoded dimensions and model detail mode stay the same. Crop or resize captures
when token reduction is the goal. OpenAI's current
[image-input accounting](https://developers.openai.com/api/docs/guides/images-vision#calculating-costs)
uses decoded dimensions and detail-dependent patches or tiles rather than the
encoded byte count.

Clipboard output uses the standard X11 `CLIPBOARD_MANAGER` persistence
handoff. It fails clearly when no clipboard manager is running or when an
encoded image exceeds the server's single-request bound. Use a file or stdout
in minimal sessions.

This first release captures X11 desktops. Native Wayland capture requires a
future compositor-owned protocol path; the command refuses instead of taking
an incomplete XWayland-only screenshot.
