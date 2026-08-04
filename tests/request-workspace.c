#include <X11/Xlib.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned long desktop_number(const char *value) {
    if (strcmp(value, "all") == 0) {
        return UINT32_MAX;
    }
    return strtoul(value, NULL, 0);
}

int main(int argc, char **argv) {
    if ((argc != 3 || strcmp(argv[1], "current") != 0) &&
        (argc != 4 || strcmp(argv[1], "move") != 0)) {
        fprintf(stderr,
                "usage: request-workspace current INDEX | move WINDOW INDEX|all\n");
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    Window root = DefaultRootWindow(display);
    Window window = root;
    const char *atom_name = "_NET_CURRENT_DESKTOP";
    const char *desktop = argv[2];
    if (strcmp(argv[1], "move") == 0) {
        window = strtoul(argv[2], NULL, 0);
        atom_name = "_NET_WM_DESKTOP";
        desktop = argv[3];
    }

    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = window;
    event.xclient.message_type = XInternAtom(display, atom_name, False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = (long)desktop_number(desktop);
    event.xclient.data.l[1] = strcmp(argv[1], "move") == 0 ? 2 : CurrentTime;
    if (XSendEvent(display, root, False,
                   SubstructureRedirectMask | SubstructureNotifyMask,
                   &event) == 0) {
        fputs("could not send workspace request\n", stderr);
        XCloseDisplay(display);
        return 1;
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
