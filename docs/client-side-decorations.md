# Client-side decorations on X11

GTK applications — including Firefox-family browsers such as Zen and Floorp —
can draw their own window decorations instead of using nobox's frame. This
page records how that interacts with a nobox session, because the resulting
behavior differs from an Openbox session on the same machine and the most
visible symptom looks like a window-manager bug when it is not one.

## How applications decide

Firefox-family browsers resolve whether "hide the title bar" is available
from the `XDG_CURRENT_DESKTOP` environment variable at startup:

- Under a session whose xsession desktop entry has no `DesktopNames` (stock
  Openbox), the variable is unset and the browser refuses client-side
  decorations entirely: it keeps normal WM decorations and never draws its
  own frame, regardless of the `browser.tabs.inTitlebar` preference.
- nobox's session entry sets `DesktopNames=nobox`, so display managers export
  `XDG_CURRENT_DESKTOP=nobox`. The browser then treats the desktop as
  CSD-capable, sets `_MOTIF_WM_HINTS` to remove all WM decorations, and draws
  GTK client-side decorations.

GTK only renders shadowed, borderless CSD when a compositor owns
`_NET_WM_CM_S0`. Without one it paints a solid fallback frame (the `solid-csd`
style class) in GTK theme colors *inside* the client window. nobox reports
`_NET_FRAME_EXTENTS = 0,0,0,0` for such windows and cannot remove the painted
frame; it is application content.

Diagnosis: capture the client window itself (`import -window <id>`) and
inspect the edge pixels, and read `_MOTIF_WM_HINTS`. A light multi-pixel band
inside the client capture together with `decorations = 0x0` identifies GTK
solid-CSD fallback.

## Remedies

- `MOZ_GTK_TITLEBAR_DECORATION=system` (environment) switches Firefox-family
  browsers to their legacy self-drawn titlebar path: the browser requests
  border-only Motif decorations (`decorations = 0x2`) and hides its titlebar
  without GTK CSD, so no fallback frame appears and no compositor is needed.
  nobox honors the hint with a frame border and no titlebar. This is legacy
  browser machinery and may disappear in future releases.
- `browser.tabs.inTitlebar = 0` keeps full WM decorations; behavior then
  matches an Openbox session with the variable unset.
- Running a compositor lets GTK use shadowed CSD. Note that compositing also
  activates latent transparency configured in other clients (terminal
  opacity, panel alpha) and animation defaults, which may be unwanted on
  otherwise lean desktops.

## Roadmap note

GTK gates shadowed CSD on the window manager advertising `_GTK_FRAME_EXTENTS`
in `_NET_SUPPORTED` in addition to compositor presence. nobox does not yet
advertise or honor `_GTK_FRAME_EXTENTS`; if compositor-based sessions become a
supported target, nobox should advertise the hint and subtract the declared
shadow insets in placement, maximize, and snapping geometry, and this gating
should be verified against current GTK at that time.
