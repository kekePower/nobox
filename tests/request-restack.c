#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xlib.h>

static int parse_number(const char *text, unsigned long *value) {
    char *end = NULL;
    errno = 0;
    *value = strtoul(text, &end, 0);
    return errno == 0 && end != text && *end == '\0';
}

int main(int argc, char **argv) {
    unsigned long raw_window;
    unsigned long raw_sibling;
    unsigned long raw_mode;
    Display *display;

    if (argc != 5 || !parse_number(argv[2], &raw_window)
        || !parse_number(argv[3], &raw_sibling)
        || !parse_number(argv[4], &raw_mode) || raw_mode > Opposite) {
        fprintf(stderr, "usage: request-restack configure|ewmh WINDOW SIBLING MODE\n");
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }

    if (strcmp(argv[1], "configure") == 0) {
        XWindowChanges changes = {0};
        unsigned int mask = CWStackMode;
        changes.stack_mode = (int)raw_mode;
        if (raw_sibling != None) {
            changes.sibling = (Window)raw_sibling;
            mask |= CWSibling;
        }
        XConfigureWindow(display, (Window)raw_window, mask, &changes);
    } else if (strcmp(argv[1], "ewmh") == 0) {
        XEvent event = {0};
        event.xclient.type = ClientMessage;
        event.xclient.display = display;
        event.xclient.window = (Window)raw_window;
        event.xclient.message_type = XInternAtom(display, "_NET_RESTACK_WINDOW", False);
        event.xclient.format = 32;
        event.xclient.data.l[0] = 2;
        event.xclient.data.l[1] = (long)raw_sibling;
        event.xclient.data.l[2] = (long)raw_mode;
        XSendEvent(display, DefaultRootWindow(display), False,
                   SubstructureRedirectMask | SubstructureNotifyMask, &event);
    } else {
        fprintf(stderr, "unknown restack protocol: %s\n", argv[1]);
        XCloseDisplay(display);
        return 2;
    }
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
