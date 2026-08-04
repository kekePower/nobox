#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 5) {
        fprintf(stderr,
                "usage: desktop-layout ORIENTATION COLUMNS ROWS CORNER\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    int screen = DefaultScreen(display);
    Window root = RootWindow(display, screen);
    Window owner = XCreateSimpleWindow(display, root, -1, -1, 1, 1, 0, 0, 0);
    char selection_name[64];
    snprintf(selection_name, sizeof(selection_name), "_NET_DESKTOP_LAYOUT_S%d", screen);
    Atom selection = XInternAtom(display, selection_name, False);
    XSetSelectionOwner(display, selection, owner, CurrentTime);
    if (XGetSelectionOwner(display, selection) != owner) {
        fputs("could not own desktop-layout selection\n", stderr);
        XCloseDisplay(display);
        return 1;
    }

    unsigned long values[4] = {
        strtoul(argv[1], NULL, 0),
        strtoul(argv[2], NULL, 0),
        strtoul(argv[3], NULL, 0),
        strtoul(argv[4], NULL, 0),
    };
    Atom property = XInternAtom(display, "_NET_DESKTOP_LAYOUT", False);
    XChangeProperty(display, root, property, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)values, 4);
    XSync(display, False);
    printf("0x%lx\n", owner);
    fflush(stdout);
    for (;;) {
        pause();
    }
}
