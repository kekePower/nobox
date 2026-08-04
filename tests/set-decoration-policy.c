#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s WINDOW POLICY\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)strtoul(argv[1], NULL, 0);

    if (strncmp(argv[2], "motif-", 6) == 0) {
        Atom motif = XInternAtom(display, "_MOTIF_WM_HINTS", False);
        unsigned long decorations = 0;
        if (strcmp(argv[2], "motif-border") == 0) decorations = 1U << 1;
        else if (strcmp(argv[2], "motif-all") == 0) decorations = 1U << 0;
        else if (strcmp(argv[2], "motif-none") != 0) return 2;
        unsigned long hints[5] = {1U << 1, 0, decorations, 0, 0};
        XChangeProperty(display, window, motif, motif, 32, PropModeReplace,
                        (unsigned char *)hints, 5);
    } else {
        Atom property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
        const char *name = strcmp(argv[2], "desktop") == 0
            ? "_NET_WM_WINDOW_TYPE_DESKTOP"
            : strcmp(argv[2], "normal") == 0
                ? "_NET_WM_WINDOW_TYPE_NORMAL"
                : NULL;
        if (name == NULL) return 2;
        Atom type = XInternAtom(display, name, False);
        XChangeProperty(display, window, property, XA_ATOM, 32, PropModeReplace,
                        (unsigned char *)&type, 1);
    }
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
