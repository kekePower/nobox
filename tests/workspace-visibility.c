#define _POSIX_C_SOURCE 200809L

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { DESKTOPS = 3, PER_DESKTOP = 12, CLIENTS = DESKTOPS * PER_DESKTOP + 2 };
static Display *display;
static Window root, clients[CLIENTS], frames[CLIENTS];
static Atom desktop_atom, current_atom, active_atom;
static Atom covering_states[2];
static int covering_state_count;
static unsigned int flashes, unexpected_maps;

static void pause_briefly(void) {
    struct timespec delay = {.tv_nsec = 10000000};
    nanosleep(&delay, NULL);
}

static unsigned long property(Window window, Atom atom) {
    Atom type;
    int format;
    unsigned long count, remaining, value = ULONG_MAX;
    unsigned char *data = NULL;
    if (XGetWindowProperty(display, window, atom, 0, 1, False, AnyPropertyType,
                           &type, &format, &count, &remaining, &data) == Success
        && format == 32 && count == 1) {
        value = *(unsigned long *)data;
    }
    if (data) XFree(data);
    return value;
}

static void request(Window window, Atom atom, long value) {
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = window;
    event.xclient.message_type = atom;
    event.xclient.format = 32;
    event.xclient.data.l[0] = value;
    XSendEvent(display, root, False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
}

static int covered_client(int index) {
    return index >= DESKTOPS * PER_DESKTOP || index % PER_DESKTOP != PER_DESKTOP - 1;
}

static void collect_events(int checking, int desktop) {
    XSync(display, False);
    while (XPending(display)) {
        XEvent event;
        XNextEvent(display, &event);
        if (!checking) continue;
        for (int index = 0; index < CLIENTS; ++index) {
            if (event.xany.window != frames[index]) continue;
            if (event.type == VisibilityNotify && covered_client(index)
                && event.xvisibility.state != VisibilityFullyObscured) {
                ++flashes;
                if (flashes <= 4)
                    fprintf(stderr, "covered frame %d became visible on desktop %d (state %d)\n",
                            index, desktop, event.xvisibility.state);
            }
            if (event.type == MapNotify
                && (index == CLIENTS - 1
                    || (index < DESKTOPS * PER_DESKTOP && index / PER_DESKTOP != desktop))) {
                ++unexpected_maps;
            }
        }
    }
}

static void switch_desktop(int desktop, int checking) {
    request(root, current_atom, desktop);
    Window top = clients[desktop * PER_DESKTOP + PER_DESKTOP - 1];
    for (int attempt = 0; attempt < 300; ++attempt) {
        collect_events(checking, desktop);
        if (property(root, current_atom) == (unsigned long)desktop
            && property(root, active_atom) == top) {
            /* Include focus/layer enforcement queued after EWMH publication. */
            for (int settle = 0; settle < 5; ++settle) {
                pause_briefly();
                collect_events(checking, desktop);
            }
            for (int index = 0; index < CLIENTS; ++index) {
                XWindowAttributes attrs;
                int visible = index == CLIENTS - 2
                    || (index < DESKTOPS * PER_DESKTOP && index / PER_DESKTOP == desktop);
                if (!XGetWindowAttributes(display, frames[index], &attrs)
                    || (attrs.map_state == IsViewable) != visible) {
                    fprintf(stderr, "frame %d has incorrect final visibility on desktop %d\n",
                            index, desktop);
                    exit(1);
                }
            }
            return;
        }
        pause_briefly();
    }
    fprintf(stderr, "desktop %d did not restore its covering client\n", desktop);
    exit(1);
}

static void create_client(int index, unsigned long desktop) {
    Window window = XCreateSimpleWindow(display, root, 80, 80, 400, 300, 0, 0, 0x406080);
    clients[index] = window;
    XStoreName(display, window, "workspace-visibility");
    XSizeHints hints = {.flags = USPosition | USSize, .x = 80, .y = 80,
                       .width = 400, .height = 300};
    XSetWMNormalHints(display, window, &hints);
    XChangeProperty(display, window, desktop_atom, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)&desktop, 1);
    if (!covered_client(index) && covering_state_count) {
        XChangeProperty(display, window, XInternAtom(display, "_NET_WM_STATE", False),
                        XA_ATOM, 32, PropModeReplace,
                        (unsigned char *)covering_states, covering_state_count);
    }
    XMapWindow(display, window);
    XFlush(display);
    for (int attempt = 0; attempt < 300; ++attempt) {
        Window tree_root, parent, *children = NULL;
        unsigned int count;
        XWindowAttributes attrs;
        XQueryTree(display, window, &tree_root, &parent, &children, &count);
        if (children) XFree(children);
        if (parent != root && XGetWindowAttributes(display, window, &attrs)
            && attrs.map_state == IsViewable
            && property(root, active_atom) == window) {
            frames[index] = parent;
            XSelectInput(display, parent, VisibilityChangeMask | StructureNotifyMask);
            return;
        }
        pause_briefly();
    }
    fprintf(stderr, "client %d did not become managed and focused\n", index);
    exit(1);
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "normal") && strcmp(argv[1], "maximized")
                      && strcmp(argv[1], "fullscreen"))) return 2;
    display = XOpenDisplay(NULL);
    if (!display) return 1;
    root = DefaultRootWindow(display);
    desktop_atom = XInternAtom(display, "_NET_WM_DESKTOP", False);
    current_atom = XInternAtom(display, "_NET_CURRENT_DESKTOP", False);
    active_atom = XInternAtom(display, "_NET_ACTIVE_WINDOW", False);
    if (strcmp(argv[1], "maximized") == 0) {
        covering_states[0] = XInternAtom(display, "_NET_WM_STATE_MAXIMIZED_HORZ", False);
        covering_states[1] = XInternAtom(display, "_NET_WM_STATE_MAXIMIZED_VERT", False);
        covering_state_count = 2;
    } else if (strcmp(argv[1], "fullscreen") == 0) {
        covering_states[0] = XInternAtom(display, "_NET_WM_STATE_FULLSCREEN", False);
        covering_state_count = 1;
    }
    request(root, current_atom, 0);

    /* Sticky and minimized clients must stay covered/hidden throughout. */
    create_client(CLIENTS - 2, UINT32_MAX);
    create_client(CLIENTS - 1, 0);
    XIconifyWindow(display, clients[CLIENTS - 1], DefaultScreen(display));
    XFlush(display);
    for (int desktop = 0; desktop < DESKTOPS; ++desktop) {
        request(root, current_atom, desktop);
        for (int attempt = 0; attempt < 300; ++attempt) {
            if (property(root, current_atom) == (unsigned long)desktop) break;
            pause_briefly();
        }
        for (int index = 0; index < PER_DESKTOP; ++index)
            create_client(desktop * PER_DESKTOP + index, desktop);
    }
    switch_desktop(0, 0);
    collect_events(0, 0);
    for (int iteration = 0; iteration < 30; ++iteration)
        switch_desktop((iteration + 1) % DESKTOPS, 1);

    printf("%s workspace visibility: %u transient exposures, %u unexpected maps across 30 switches (%d clients)\n",
           argv[1], flashes, unexpected_maps, CLIENTS);
    XCloseDisplay(display);
    return flashes || unexpected_maps ? 1 : 0;
}
