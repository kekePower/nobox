#define _POSIX_C_SOURCE 200809L

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static Time server_time(Display *display, Window window) {
    Atom marker = XInternAtom(display, "_NOBOX_SELECTION_TEST_TIME", False);
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

static int own_selections(Display *display, const char *text) {
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), -10, -10, 1, 1, 0, 0, 0);
    Atom clipboard = XInternAtom(display, "CLIPBOARD", False);
    Atom primary = XA_PRIMARY;
    Atom utf8 = XInternAtom(display, "UTF8_STRING", False);
    Atom targets = XInternAtom(display, "TARGETS", False);
    Atom timestamp = XInternAtom(display, "TIMESTAMP", False);
    Time acquired = server_time(display, window);
    XSetSelectionOwner(display, clipboard, window, acquired);
    XSetSelectionOwner(display, primary, window, acquired);
    XSync(display, False);
    if (XGetSelectionOwner(display, clipboard) != window
        || XGetSelectionOwner(display, primary) != window) return 1;
    printf("owner 0x%lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == SelectionClear) {
            fprintf(stderr, "selection 0x%lx was unexpectedly cleared\n",
                    event.xselectionclear.selection);
            return 1;
        }
        if (event.type != SelectionRequest) continue;
        XSelectionRequestEvent *request = &event.xselectionrequest;
        Atom property = request->property == None ? request->target : request->property;
        int converted = 0;
        if (request->target == utf8 || request->target == XA_STRING) {
            XChangeProperty(display, request->requestor, property, request->target,
                            8, PropModeReplace, (const unsigned char *)text,
                            (int)strlen(text));
            converted = 1;
        } else if (request->target == targets) {
            Atom supported[] = {targets, timestamp, utf8, XA_STRING};
            XChangeProperty(display, request->requestor, property, XA_ATOM, 32,
                            PropModeReplace, (unsigned char *)supported, 4);
            converted = 1;
        } else if (request->target == timestamp) {
            unsigned long value = acquired;
            XChangeProperty(display, request->requestor, property, XA_INTEGER, 32,
                            PropModeReplace, (unsigned char *)&value, 1);
            converted = 1;
        }
        XSelectionEvent reply = {
            .type = SelectionNotify,
            .display = display,
            .requestor = request->requestor,
            .selection = request->selection,
            .target = request->target,
            .property = converted ? property : None,
            .time = request->time,
        };
        XSendEvent(display, request->requestor, False, NoEventMask, (XEvent *)&reply);
        XFlush(display);
    }
    XDestroyWindow(display, window);
    return 0;
}

static int read_property(
    Display *display,
    Window window,
    Atom property,
    Atom type,
    unsigned char **data,
    unsigned long *items) {
    Atom actual_type;
    int format;
    unsigned long remaining;
    return XGetWindowProperty(display, window, property, 0, 1024, True, type,
                              &actual_type, &format, items, &remaining, data) == Success
        && actual_type == type && remaining == 0;
}

static int wait_for_selection(Display *display, Atom selection, Atom target,
                              Atom property, Window requestor) {
    XConvertSelection(display, selection, target, property, requestor, CurrentTime);
    XFlush(display);
    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == SelectionNotify
            && event.xselection.selection == selection
            && event.xselection.target == target) {
            return event.xselection.property != None;
        }
    }
}

static int request_selection(Display *display, const char *name, const char *mode) {
    Atom selection = XInternAtom(display, name, False);
    Window owner = XGetSelectionOwner(display, selection);
    if (strcmp(mode, "owner") == 0) {
        if (owner == None) return 1;
        printf("0x%lx\n", owner);
        return 0;
    }
    if (owner == None) return 1;

    Window requestor = XCreateSimpleWindow(
        display, DefaultRootWindow(display), -10, -10, 1, 1, 0, 0, 0);
    Atom property = XInternAtom(display, "_NOBOX_SELECTION_TEST_RESULT", False);
    Atom target;
    if (strcmp(mode, "text") == 0) {
        target = XInternAtom(display, "UTF8_STRING", False);
    } else if (strcmp(mode, "targets") == 0) {
        target = XInternAtom(display, "TARGETS", False);
    } else if (strcmp(mode, "timestamp") == 0) {
        target = XInternAtom(display, "TIMESTAMP", False);
    } else if (strcmp(mode, "multiple") == 0) {
        target = XInternAtom(display, "MULTIPLE", False);
    } else {
        return 2;
    }

    if (target != XInternAtom(display, "MULTIPLE", False)) {
        if (!wait_for_selection(display, selection, target, property, requestor)) return 1;
        unsigned char *data = NULL;
        unsigned long items = 0;
        Atom type = target == XInternAtom(display, "TARGETS", False)
            ? XA_ATOM
            : target == XInternAtom(display, "TIMESTAMP", False)
                ? XA_INTEGER : target;
        if (!read_property(display, requestor, property, type, &data, &items)) return 1;
        if (strcmp(mode, "text") == 0) {
            printf("%.*s\n", (int)items, data);
        } else if (strcmp(mode, "targets") == 0) {
            Atom *atoms = (Atom *)data;
            Atom targets = XInternAtom(display, "TARGETS", False);
            Atom multiple = XInternAtom(display, "MULTIPLE", False);
            Atom timestamp = XInternAtom(display, "TIMESTAMP", False);
            int found_targets = 0;
            int found_multiple = 0;
            int found_timestamp = 0;
            for (unsigned long i = 0; i < items; ++i) {
                found_targets |= atoms[i] == targets;
                found_multiple |= atoms[i] == multiple;
                found_timestamp |= atoms[i] == timestamp;
            }
            if (!found_targets || !found_multiple || !found_timestamp) return 1;
            puts("targets ok");
        } else {
            if (items != 1 || *(unsigned long *)data == CurrentTime) return 1;
            printf("timestamp %lu\n", *(unsigned long *)data);
        }
        XFree(data);
    } else {
        Atom atom_pair = XInternAtom(display, "ATOM_PAIR", False);
        Atom targets = XInternAtom(display, "TARGETS", False);
        Atom timestamp = XInternAtom(display, "TIMESTAMP", False);
        Atom unsupported = XInternAtom(display, "_NOBOX_UNSUPPORTED_TARGET", False);
        Atom targets_result = XInternAtom(display, "_NOBOX_TARGETS_RESULT", False);
        Atom timestamp_result = XInternAtom(display, "_NOBOX_TIMESTAMP_RESULT", False);
        Atom unsupported_result = XInternAtom(display, "_NOBOX_UNSUPPORTED_RESULT", False);
        Atom pairs[] = {
            targets, targets_result,
            timestamp, timestamp_result,
            unsupported, unsupported_result,
        };
        XChangeProperty(display, requestor, property, atom_pair, 32,
                        PropModeReplace, (unsigned char *)pairs, 6);
        if (!wait_for_selection(display, selection, target, property, requestor)) return 1;
        unsigned char *data = NULL;
        unsigned long items = 0;
        if (!read_property(display, requestor, property, atom_pair, &data, &items)
            || items != 6) return 1;
        Atom *results = (Atom *)data;
        int valid = results[0] == targets && results[1] == targets_result
            && results[2] == timestamp && results[3] == timestamp_result
            && results[4] == None && results[5] == unsupported_result;
        XFree(data);
        if (!valid) return 1;
        if (!read_property(display, requestor, targets_result, XA_ATOM, &data, &items)) return 1;
        XFree(data);
        if (!read_property(display, requestor, timestamp_result, XA_INTEGER, &data, &items)
            || items != 1) return 1;
        XFree(data);
        puts("multiple ok");
    }
    XDestroyWindow(display, requestor);
    return 0;
}

int main(int argc, char **argv) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    int result;
    if (argc == 3 && strcmp(argv[1], "own") == 0) {
        result = own_selections(display, argv[2]);
    } else if (argc == 4 && strcmp(argv[1], "request") == 0) {
        result = request_selection(display, argv[2], argv[3]);
    } else {
        fprintf(stderr, "usage: selection-client own TEXT | request SELECTION owner|text|targets|timestamp|multiple\n");
        result = 2;
    }
    XCloseDisplay(display);
    return result;
}
