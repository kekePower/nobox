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

int main(int argc, char **argv) {
    if (argc < 2 || argc > 3) {
        fprintf(stderr, "usage: %s CLIENT_COUNT [--retry-map]\n", argv[0]);
        return 2;
    }
    int retry_map = argc == 3 && strcmp(argv[2], "--retry-map") == 0;
    if (argc == 3 && !retry_map) {
        fprintf(stderr, "unknown option: %s\n", argv[2]);
        return 2;
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
    while (client_list == None && monotonic_microseconds() < deadline) {
        client_list = XInternAtom(display, "_NET_CLIENT_LIST", True);
        if (client_list == None) {
            nanosleep(&interval, NULL);
        }
    }
    if (client_list == None) {
        fprintf(stderr, "the window manager did not publish _NET_CLIENT_LIST\n");
        free(windows);
        XCloseDisplay(display);
        return 1;
    }

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    for (unsigned long index = 0; index < requested; ++index) {
        int x = 12 + (int)(index % 10) * 31;
        int y = 12 + (int)((index / 10) % 10) * 24;
        windows[index] = XCreateSimpleWindow(
            display, root, x, y, 240U, 140U, 0U,
            BlackPixel(display, DefaultScreen(display)),
            WhitePixel(display, DefaultScreen(display)));
        char title[64];
        int length = snprintf(title, sizeof(title), "nobox-load-%lu", index);
        if (length > 0 && (size_t)length < sizeof(title)) {
            XStoreName(display, windows[index], title);
        }
        XMapWindow(display, windows[index]);
    }
    XSync(display, False);

    unsigned int polls = 0;
    while (managed_client_count(display, root, client_list) < requested) {
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
