#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static const unsigned long root_pixel = 0x123456;

static int set_parent_relative_chain(Display *display, Window window,
                                     Window root) {
    Window current = window;
    for (unsigned int depth = 0; depth < 50 && current != root; ++depth) {
        Window query_root;
        Window parent;
        Window *children = NULL;
        unsigned int child_count = 0;

        XSetWindowBackgroundPixmap(display, current, ParentRelative);
        if (!XQueryTree(display, current, &query_root, &parent, &children,
                        &child_count)) {
            return 0;
        }
        if (children != NULL) XFree(children);
        current = parent;
    }
    return current == root;
}

static unsigned long first_pixel(Display *display, Window window) {
    XImage *image = XGetImage(display, window, 0, 0, 1, 1, AllPlanes, ZPixmap);
    if (image == NULL) return ~0UL;
    unsigned long pixel = XGetPixel(image, 0, 0);
    XDestroyImage(image);
    return pixel;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window root = DefaultRootWindow(display);
    XSetWindowBackground(display, root, root_pixel);
    XClearWindow(display, root);

    XSetWindowAttributes attributes = {
        .background_pixmap = ParentRelative,
        .backing_store = Always,
        .event_mask = StructureNotifyMask | ExposureMask,
    };
    Window window = XCreateWindow(
        display, root, 100, 100, 160, 100, 0, CopyFromParent, InputOutput,
        CopyFromParent, CWBackPixmap | CWBackingStore | CWEventMask, &attributes);

    Atom motif = XInternAtom(display, "_MOTIF_WM_HINTS", False);
    unsigned long motif_hints[5] = {1UL << 1, 0, 0, 0, 0};
    XChangeProperty(display, window, motif, motif, 32, PropModeReplace,
                    (const unsigned char *)motif_hints, 5);
    XWMHints wm_hints = {.flags = InputHint, .input = False};
    XSetWMHints(display, window, &wm_hints);
    XStoreName(display, window, "nobox pseudo-transparent regression");
    XMapWindow(display, window);
    XFlush(display);

    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == ReparentNotify && event.xreparent.window == window &&
            event.xreparent.parent != root) {
            break;
        }
    }
    XWindowAttributes window_attributes = {0};
    for (unsigned int attempt = 0; attempt < 100; ++attempt) {
        if (XGetWindowAttributes(display, window, &window_attributes) &&
            window_attributes.map_state == IsViewable) {
            break;
        }
        usleep(10000);
    }
    if (window_attributes.map_state != IsViewable) {
        fputs("reparented window did not become viewable\n", stderr);
        return 1;
    }
    if (!set_parent_relative_chain(display, window, root)) {
        fputs("could not set the ParentRelative window chain\n", stderr);
        return 1;
    }
    XClearWindow(display, window);
    XSync(display, False);
    unsigned long initial_pixel = first_pixel(display, window) & 0xffffffUL;
    if (initial_pixel != root_pixel) {
        fprintf(stderr,
                "initial ParentRelative background was 0x%06lx, expected "
                "0x%06lx\n",
                initial_pixel, root_pixel);
        return 1;
    }

    wm_hints.flags |= XUrgencyHint;
    XSetWMHints(display, window, &wm_hints);
    XFlush(display);
    usleep(500000);

    XClearWindow(display, window);
    XSync(display, False);
    unsigned long pixel = first_pixel(display, window) & 0xffffffUL;
    printf("pixel=0x%06lx expected=0x%06lx\n", pixel, root_pixel);

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return pixel == root_pixel ? 0 : 1;
}
