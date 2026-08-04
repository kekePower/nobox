#define _POSIX_C_SOURCE 200809L

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    int skip_taskbar = 0;
    int skip_pager = 0;
    int urgent = 0;
    const char *title = "nobox-presentation-client";
    for (int argument = 1; argument < argc; ++argument) {
        if (strcmp(argv[argument], "--skip-taskbar") == 0) {
            skip_taskbar = 1;
        } else if (strcmp(argv[argument], "--skip-pager") == 0) {
            skip_pager = 1;
        } else if (strcmp(argv[argument], "--urgent") == 0) {
            urgent = 1;
        } else if (strcmp(argv[argument], "--title") == 0 && argument + 1 < argc) {
            title = argv[++argument];
        } else {
            fprintf(stderr,
                    "usage: %s [--skip-taskbar] [--skip-pager] [--urgent] "
                    "[--title TITLE]\n",
                    argv[0]);
            return 2;
        }
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not connect to the X server\n", stderr);
        return 1;
    }
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 80, 80, 320, 160, 1,
        BlackPixel(display, 0), WhitePixel(display, 0));
    XStoreName(display, window, title);
    XClassHint class_hint = {.res_name = "presentation", .res_class = "NoboxTest"};
    XSetClassHint(display, window, &class_hint);
    Atom delete_window = XInternAtom(display, "WM_DELETE_WINDOW", False);
    XSetWMProtocols(display, window, &delete_window, 1);

    Atom states[2];
    int state_count = 0;
    if (skip_taskbar) {
        states[state_count++] = XInternAtom(display, "_NET_WM_STATE_SKIP_TASKBAR", False);
    }
    if (skip_pager) {
        states[state_count++] = XInternAtom(display, "_NET_WM_STATE_SKIP_PAGER", False);
    }
    if (state_count > 0) {
        Atom property = XInternAtom(display, "_NET_WM_STATE", False);
        XChangeProperty(display, window, property, XA_ATOM, 32, PropModeReplace,
                        (unsigned char *)states, state_count);
    }
    if (urgent) {
        XWMHints hints = {.flags = XUrgencyHint};
        XSetWMHints(display, window, &hints);
    }

    XMapWindow(display, window);
    XFlush(display);
    printf("0x%lx\n", window);
    fflush(stdout);
    XEvent event;
    while (1) {
        XNextEvent(display, &event);
        if (event.type == ClientMessage
            && (Atom)event.xclient.data.l[0] == delete_window) {
            break;
        }
    }
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
