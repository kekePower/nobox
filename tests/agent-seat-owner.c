#define _POSIX_C_SOURCE 200809L

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

enum { ADVERTISEMENT_LIMIT = 256 };

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static Time server_time(Display *display, Window window) {
    Atom marker = XInternAtom(display, "_NOBOX_AGENT_SEAT_TEST_TIME", False);
    XSelectInput(display, window, PropertyChangeMask);
    XChangeProperty(display, window, marker, XA_INTEGER, 8,
                    PropModeAppend, NULL, 0);
    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == PropertyNotify && event.xproperty.atom == marker) {
            return event.xproperty.time;
        }
    }
}

static int advertisement(const char *socket, unsigned char *value) {
    static const char protocol[] = "agent-seat";
    static const char revision[] = "2";
    size_t protocol_length = sizeof(protocol) - 1;
    size_t revision_length = sizeof(revision) - 1;
    size_t socket_length = strlen(socket);
    size_t length = protocol_length + revision_length + socket_length + 2;
    if (length > ADVERTISEMENT_LIMIT) return -1;
    memcpy(value, protocol, protocol_length);
    value[protocol_length] = '\0';
    memcpy(value + protocol_length + 1, revision, revision_length);
    value[protocol_length + revision_length + 1] = '\0';
    memcpy(value + protocol_length + revision_length + 2, socket, socket_length);
    return (int)length;
}

static int property_equals(Display *display, Window window, Atom property,
                           Atom utf8, const unsigned char *expected,
                           int expected_length) {
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long items = 0;
    unsigned long remaining = 0;
    unsigned char *value = NULL;
    int status = XGetWindowProperty(display, window, property, 0,
                                    ADVERTISEMENT_LIMIT / 4 + 1, False, AnyPropertyType,
                                    &actual_type, &actual_format, &items, &remaining, &value);
    int equal = status == Success && actual_type == utf8 && actual_format == 8
        && remaining == 0 && items == (unsigned long)expected_length
        && value != NULL && memcmp(value, expected, items) == 0;
    if (value != NULL) XFree(value);
    return equal;
}

static int set_root(Display *display, Atom property, Atom utf8, const char *socket) {
    unsigned char value[ADVERTISEMENT_LIMIT];
    int length = advertisement(socket, value);
    if (length < 0) return 2;
    XChangeProperty(display, DefaultRootWindow(display), property, utf8, 8,
                    PropModeReplace, value, length);
    XSync(display, False);
    return 0;
}

static int hold(Display *display, int replace, Atom selection, Atom property,
                Atom utf8, Atom manager, const char *owner_socket,
                const char *root_socket) {
    Window root = DefaultRootWindow(display);
    Window window = XCreateSimpleWindow(display, root, -10, -10, 1, 1, 0, 0, 0);
    Time acquired = server_time(display, window);
    unsigned char owner_value[ADVERTISEMENT_LIMIT];
    unsigned char root_value[ADVERTISEMENT_LIMIT];
    int owner_length = advertisement(owner_socket, owner_value);
    int root_length = advertisement(root_socket, root_value);
    if (owner_length < 0 || root_length < 0) return 2;

    XGrabServer(display);
    Window previous = XGetSelectionOwner(display, selection);
    if (!replace && previous != None) {
        XUngrabServer(display);
        XDestroyWindow(display, window);
        XSync(display, False);
        return 3;
    }
    XSetSelectionOwner(display, selection, window, acquired);
    if (XGetSelectionOwner(display, selection) != window) {
        XUngrabServer(display);
        XDestroyWindow(display, window);
        XSync(display, False);
        return 4;
    }
    XChangeProperty(display, window, property, utf8, 8, PropModeReplace,
                    owner_value, owner_length);
    XChangeProperty(display, root, property, utf8, 8, PropModeReplace,
                    root_value, root_length);
    XClientMessageEvent announcement = {
        .type = ClientMessage,
        .display = display,
        .window = root,
        .message_type = manager,
        .format = 32,
    };
    announcement.data.l[0] = (long)acquired;
    announcement.data.l[1] = (long)selection;
    announcement.data.l[2] = (long)window;
    XSendEvent(display, root, False, StructureNotifyMask, (XEvent *)&announcement);
    XUngrabServer(display);
    XSync(display, False);

    printf("0x%lx\n", window);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    int lost = 0;
    while (running && !lost) {
        while (XPending(display) != 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == SelectionClear && event.xselectionclear.selection == selection) {
                lost = 1;
            }
        }
        if (running && !lost) {
            fd_set readable;
            FD_ZERO(&readable);
            FD_SET(ConnectionNumber(display), &readable);
            struct timeval timeout = {.tv_sec = 0, .tv_usec = 100000};
            select(ConnectionNumber(display) + 1, &readable, NULL, NULL, &timeout);
        }
    }

    XGrabServer(display);
    if (property_equals(display, root, property, utf8, root_value, root_length)) {
        XDeleteProperty(display, root, property);
    }
    XDestroyWindow(display, window);
    XUngrabServer(display);
    XSync(display, False);
    return lost ? 5 : 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s owner|set-root|delete-root|hold|replace [SOCKET [ROOT_SOCKET]]\n",
                argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    int screen = DefaultScreen(display);
    char selection_name[64];
    snprintf(selection_name, sizeof(selection_name), "_AGENT_SEAT_S%d", screen);
    Atom selection = XInternAtom(display, selection_name, False);
    Atom property = XInternAtom(display, "_AGENT_SEAT", False);
    Atom utf8 = XInternAtom(display, "UTF8_STRING", False);
    Atom manager = XInternAtom(display, "MANAGER", False);
    int result = 0;

    if (strcmp(argv[1], "owner") == 0) {
        Window owner = XGetSelectionOwner(display, selection);
        if (owner == None) result = 1;
        else printf("0x%lx\n", owner);
    } else if (strcmp(argv[1], "delete-root") == 0) {
        XDeleteProperty(display, DefaultRootWindow(display), property);
        XSync(display, False);
    } else if (strcmp(argv[1], "set-root") == 0 && argc == 3) {
        result = set_root(display, property, utf8, argv[2]);
    } else if ((strcmp(argv[1], "hold") == 0 || strcmp(argv[1], "replace") == 0)
               && (argc == 3 || argc == 4)) {
        result = hold(display, strcmp(argv[1], "replace") == 0, selection,
                      property, utf8, manager, argv[2], argc == 4 ? argv[3] : argv[2]);
    } else {
        result = 2;
    }
    XCloseDisplay(display);
    return result;
}
