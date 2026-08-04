#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s WINDOW SIZE|malformed|delete\n", argv[0]);
        return 2;
    }
    char *window_end = NULL;
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &window_end, 0);
    if (errno != 0 || window_end == argv[1] || *window_end != '\0') return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Atom icon_atom = XInternAtom(display, "_NET_WM_ICON", False);
    if (strcmp(argv[2], "delete") == 0) {
        XDeleteProperty(display, (Window)raw_window, icon_atom);
    } else if (strcmp(argv[2], "malformed") == 0) {
        unsigned long values[] = {ULONG_MAX, ULONG_MAX};
        XChangeProperty(display, (Window)raw_window, icon_atom, XA_CARDINAL, 32,
                        PropModeReplace, (unsigned char *)values, 2);
    } else {
        char *size_end = NULL;
        errno = 0;
        unsigned long size = strtoul(argv[2], &size_end, 10);
        if (errno != 0 || size_end == argv[2] || *size_end != '\0' || size == 0 || size > 64) {
            XCloseDisplay(display);
            return 2;
        }
        size_t pixel_count = (size_t)size * (size_t)size;
        unsigned long *values = calloc(pixel_count + 2U, sizeof(*values));
        if (values == NULL) {
            XCloseDisplay(display);
            return 2;
        }
        values[0] = size;
        values[1] = size;
        for (size_t index = 0; index < pixel_count; ++index) {
            values[index + 2U] = 0xffaa3377UL;
        }
        XChangeProperty(display, (Window)raw_window, icon_atom, XA_CARDINAL, 32,
                        PropModeReplace, (unsigned char *)values, (int)(pixel_count + 2U));
        free(values);
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
