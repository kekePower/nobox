#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc < 3 || argc > 5) {
        fprintf(stderr,
                "usage: %s WINDOW partial|legacy|both|clear [DEPTH ...]\n",
                argv[0]);
        return 2;
    }
    const char *mode = argv[2];
    if ((strcmp(mode, "clear") == 0 && argc != 3)
        || (strcmp(mode, "partial") == 0 && argc != 4)
        || (strcmp(mode, "legacy") == 0 && argc != 4)
        || (strcmp(mode, "both") == 0 && argc != 5)
        || (strcmp(mode, "clear") != 0 && strcmp(mode, "partial") != 0
            && strcmp(mode, "legacy") != 0 && strcmp(mode, "both") != 0)) {
        fprintf(stderr,
                "usage: %s WINDOW partial|legacy|both|clear [DEPTH ...]\n",
                argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)strtoul(argv[1], NULL, 0);
    Atom partial = XInternAtom(display, "_NET_WM_STRUT_PARTIAL", False);
    Atom legacy = XInternAtom(display, "_NET_WM_STRUT", False);
    if (strcmp(mode, "clear") == 0) {
        XDeleteProperty(display, window, partial);
        XDeleteProperty(display, window, legacy);
    } else {
        unsigned long depth = strtoul(argv[3], NULL, 10);
        if (strcmp(mode, "partial") == 0 || strcmp(mode, "both") == 0) {
            unsigned long values[12] = {0, 0, depth, 0, 0, 0, 0, 0, 0, 799, 0, 0};
            XChangeProperty(display, window, partial, XA_CARDINAL, 32,
                            PropModeReplace, (unsigned char *)values, 12);
            if (strcmp(mode, "both") == 0) {
                unsigned long legacy_depth = strtoul(argv[4], NULL, 10);
                unsigned long legacy_values[4] = {0, 0, legacy_depth, 0};
                XChangeProperty(display, window, legacy, XA_CARDINAL, 32,
                                PropModeReplace,
                                (unsigned char *)legacy_values, 4);
            }
        } else {
            unsigned long values[4] = {0, 0, depth, 0};
            XDeleteProperty(display, window, partial);
            XChangeProperty(display, window, legacy, XA_CARDINAL, 32,
                            PropModeReplace, (unsigned char *)values, 4);
        }
    }
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
