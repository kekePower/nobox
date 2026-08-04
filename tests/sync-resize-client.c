#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/sync.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "responsive") != 0
                      && strcmp(argv[1], "stalled") != 0)) {
        fprintf(stderr, "usage: sync-resize-client responsive|stalled\n");
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    int sync_event;
    int sync_error;
    if (!XSyncQueryExtension(display, &sync_event, &sync_error)) {
        XCloseDisplay(display);
        return 77;
    }

    XSyncValue initial;
    XSyncIntToValue(&initial, 41);
    XSyncCounter counter = XSyncCreateCounter(display, initial);
    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 120, 100, 320, 180, 0, 0, 0xffffff);
    Atom protocols = XInternAtom(display, "WM_PROTOCOLS", False);
    Atom sync_request = XInternAtom(display, "_NET_WM_SYNC_REQUEST", False);
    Atom sync_counter = XInternAtom(display, "_NET_WM_SYNC_REQUEST_COUNTER", False);
    Atom supported[] = {sync_request};
    unsigned long counter_property = counter;
    XSetWMProtocols(display, window, supported, 1);
    XChangeProperty(display, window, sync_counter, XA_CARDINAL, 32,
                    PropModeReplace, (unsigned char *)&counter_property, 1);
    XSizeHints hints = {.flags = PPosition, .x = 120, .y = 100};
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, argv[1]);
    XSelectInput(display, window, StructureNotifyMask);
    XMapWindow(display, window);
    XFlush(display);
    printf("window 0x%lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    uint64_t pending = 0;
    int reported_initial = 0;
    while (running) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == MapNotify && !reported_initial) {
            XSyncValue value;
            if (!XSyncQueryCounter(display, counter, &value)) return 1;
            uint64_t current = ((uint64_t)(uint32_t)XSyncValueHigh32(value) << 32)
                | (uint32_t)XSyncValueLow32(value);
            printf("initial %llu\n", (unsigned long long)current);
            fflush(stdout);
            reported_initial = 1;
        } else if (event.type == ClientMessage
            && event.xclient.message_type == protocols
            && event.xclient.format == 32
            && (Atom)event.xclient.data.l[0] == sync_request) {
            uint32_t low = (uint32_t)event.xclient.data.l[2];
            uint32_t high = (uint32_t)event.xclient.data.l[3];
            pending = ((uint64_t)high << 32) | low;
            printf("request %llu\n", (unsigned long long)pending);
            fflush(stdout);
        } else if (event.type == ConfigureNotify) {
            printf("configure %d %d\n", event.xconfigure.width, event.xconfigure.height);
            fflush(stdout);
            if (pending != 0 && strcmp(argv[1], "responsive") == 0) {
                XSyncValue value;
                XSyncIntsToValue(&value, (uint32_t)pending,
                                 (int32_t)(pending >> 32));
                XSyncSetCounter(display, counter, value);
                XFlush(display);
                printf("ack %llu\n", (unsigned long long)pending);
                fflush(stdout);
                pending = 0;
            }
        }
    }

    XDestroyWindow(display, window);
    XSyncDestroyCounter(display, counter);
    XCloseDisplay(display);
    return 0;
}
