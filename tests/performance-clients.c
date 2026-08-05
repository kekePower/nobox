#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static uint64_t monotonic_microseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return 0;
    }
    return (uint64_t)now.tv_sec * UINT64_C(1000000) +
           (uint64_t)now.tv_nsec / UINT64_C(1000);
}

static unsigned long managed_client_count(Display *display, Window root,
                                          Atom client_list) {
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long item_count = 0;
    unsigned long bytes_after = 0;
    unsigned char *value = NULL;
    int status = XGetWindowProperty(
        display, root, client_list, 0, 4096, False, XA_WINDOW, &actual_type,
        &actual_format, &item_count, &bytes_after, &value);
    if (value != NULL) {
        XFree(value);
    }
    if (status != Success || actual_type != XA_WINDOW || actual_format != 32) {
        return 0;
    }
    return item_count;
}

static Window active_window(Display *display, Window root, Atom active_property) {
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long item_count = 0;
    unsigned long bytes_after = 0;
    unsigned char *value = NULL;
    Window active = None;
    int status = XGetWindowProperty(
        display, root, active_property, 0, 1, False, XA_WINDOW, &actual_type,
        &actual_format, &item_count, &bytes_after, &value);
    if (status == Success && actual_type == XA_WINDOW && actual_format == 32 &&
        item_count == 1 && value != NULL) {
        memcpy(&active, value, sizeof(active));
    }
    if (value != NULL) {
        XFree(value);
    }
    return active;
}

int main(int argc, char **argv) {
    if (argc < 2 || argc > 4) {
        fprintf(stderr, "usage: %s CLIENT_COUNT [--retry-map] [--positioned]\n", argv[0]);
        return 2;
    }
    int retry_map = 0;
    int positioned = 0;
    for (int index = 2; index < argc; ++index) {
        if (strcmp(argv[index], "--retry-map") == 0) {
            retry_map = 1;
        } else if (strcmp(argv[index], "--positioned") == 0) {
            positioned = 1;
        } else {
            fprintf(stderr, "unknown option: %s\n", argv[index]);
            return 2;
        }
    }
    char *end = NULL;
    errno = 0;
    unsigned long requested = strtoul(argv[1], &end, 10);
    if (errno != 0 || end == argv[1] || *end != '\0' || requested == 0 ||
        requested > 500) {
        fprintf(stderr, "CLIENT_COUNT must be between 1 and 500\n");
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    Window *windows = calloc(requested, sizeof(*windows));
    if (windows == NULL) {
        XCloseDisplay(display);
        return 1;
    }

    Window root = DefaultRootWindow(display);
    uint64_t started = monotonic_microseconds();
    uint64_t deadline = started + UINT64_C(10000000);
    struct timespec interval = {.tv_sec = 0, .tv_nsec = 1000000};
    Atom client_list = None;
    Atom active_property = None;
    while ((client_list == None || active_property == None) &&
           monotonic_microseconds() < deadline) {
        client_list = XInternAtom(display, "_NET_CLIENT_LIST", True);
        active_property = XInternAtom(display, "_NET_ACTIVE_WINDOW", True);
        if (client_list == None || active_property == None) {
            nanosleep(&interval, NULL);
        }
    }
    if (client_list == None || active_property == None) {
        fprintf(stderr, "the window manager did not publish its EWMH client state\n");
        free(windows);
        XCloseDisplay(display);
        return 1;
    }

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    for (unsigned long index = 0; index < requested; ++index) {
        int x = positioned ? 10 + (int)(index % 5) * 250
                           : 12 + (int)(index % 10) * 31;
        int y = positioned ? 10 + (int)((index / 5) % 5) * 150
                           : 12 + (int)((index / 10) % 10) * 24;
        unsigned int width = 240U;
        unsigned int height = 140U;
        windows[index] = XCreateSimpleWindow(
            display, root, x, y, width, height, 0U,
            BlackPixel(display, DefaultScreen(display)),
            WhitePixel(display, DefaultScreen(display)));
        char title[64];
        int length = snprintf(title, sizeof(title), "nobox-load-%lu", index);
        if (length > 0 && (size_t)length < sizeof(title)) {
            XStoreName(display, windows[index], title);
        }
        if (positioned) {
            XSizeHints hints = {.flags = PPosition, .x = x, .y = y};
            XSetWMNormalHints(display, windows[index], &hints);
        }
        XMapWindow(display, windows[index]);
    }
    XSync(display, False);

    unsigned int polls = 0;
    while (managed_client_count(display, root, client_list) < requested ||
           active_window(display, root, active_property) != windows[requested - 1]) {
        if (monotonic_microseconds() >= deadline) {
            fprintf(stderr, "window manager did not publish %lu clients\n", requested);
            free(windows);
            XCloseDisplay(display);
            return 1;
        }
        if (retry_map && polls % 10 == 9) {
            for (unsigned long index = 0; index < requested; ++index) {
                XUnmapWindow(display, windows[index]);
                XMapWindow(display, windows[index]);
            }
            XSync(display, False);
        }
        polls += 1;
        nanosleep(&interval, NULL);
    }
    uint64_t finished = monotonic_microseconds();
    printf("manage_us=%lu\n", (unsigned long)(finished - started));
    fflush(stdout);

    while (running) {
        pause();
    }

    for (unsigned long index = 0; index < requested; ++index) {
        XDestroyWindow(display, windows[index]);
    }
    XSync(display, False);
    free(windows);
    XCloseDisplay(display);
    return 0;
}
