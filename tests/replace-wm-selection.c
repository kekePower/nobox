#include <stdio.h>
#include <stdlib.h>
#include <sys/select.h>
#include <sys/time.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s CLIENT\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window client = (Window)strtoul(argv[1], NULL, 0);
    char name[32];
    snprintf(name, sizeof(name), "WM_S%d", DefaultScreen(display));
    Atom selection = XInternAtom(display, name, False);
    Window previous = XGetSelectionOwner(display, selection);
    if (previous == None) return 1;
    XSelectInput(display, previous, StructureNotifyMask);
    Window replacement = XCreateSimpleWindow(
        display, DefaultRootWindow(display), -10, -10, 1, 1, 0, 0, 0);
    XSetSelectionOwner(display, selection, replacement, CurrentTime);
    XSync(display, False);
    if (XGetSelectionOwner(display, selection) != replacement) return 1;

    for (int attempt = 0; attempt < 50; ++attempt) {
        while (XPending(display) != 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == DestroyNotify && event.xdestroywindow.window == previous) {
                if (XGetSelectionOwner(display, selection) != replacement) return 1;
                Window root;
                Window parent;
                Window *children = NULL;
                unsigned int child_count = 0;
                XWindowAttributes attributes;
                if (!XQueryTree(display, client, &root, &parent, &children, &child_count)
                    || parent != DefaultRootWindow(display)
                    || !XGetWindowAttributes(display, client, &attributes)
                    || attributes.map_state != IsViewable) return 1;
                if (children != NULL) XFree(children);
                puts("handover ok");
                XDestroyWindow(display, replacement);
                XCloseDisplay(display);
                return 0;
            }
        }
        fd_set readable;
        FD_ZERO(&readable);
        FD_SET(ConnectionNumber(display), &readable);
        struct timeval timeout = {.tv_sec = 0, .tv_usec = 100000};
        select(ConnectionNumber(display) + 1, &readable, NULL, NULL, &timeout);
    }
    fputs("previous WM did not destroy its selection owner\n", stderr);
    return 1;
}
