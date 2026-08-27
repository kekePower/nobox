# nobox-screenshot

`nobox-screenshot` is Nobox's small X11 screenshot utility. Its familiar
command-line options follow `gnome-screenshot` 41, while `--format` and
`--quality` make compact JPEG captures explicit and testable.

```sh
# Lossless PNG in the Pictures directory (the default)
nobox-screenshot

# Active window as JPEG at the recommended agent-oriented starting point
nobox-screenshot --window --format jpeg --quality 75 --file window.jpg

# Drag out an area, include the cursor, and write image bytes to stdout
nobox-screenshot --area --include-pointer --quality 70 --stdout > area.jpg
```

Supported compatibility options are `--clipboard`, `--window`, `--area`,
`--include-border`, `--remove-border`, `--include-pointer`, `--delay`,
`--border-effect`, `--interactive`, `--file`, `--version`, and `--display`.
As in `gnome-screenshot` 41, border switches and effects are deprecated and do
not alter the capture. `--interactive` starts the same drag selector as
`--area`; Nobox intentionally has no screenshot toolkit dialog.

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
