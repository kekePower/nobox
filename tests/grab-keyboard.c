#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    char *end = NULL;
    unsigned long raw_window;
    long milliseconds;
    Display *display;
    struct timespec duration;
    int status;

    if (argc != 3) {
        fprintf(stderr, "usage: grab-keyboard WINDOW MILLISECONDS\n");
        return 2;
    }
    errno = 0;
    raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') {
        fprintf(stderr, "invalid window identifier: %s\n", argv[1]);
        return 2;
    }
    errno = 0;
    milliseconds = strtol(argv[2], &end, 10);
    if (errno != 0 || end == argv[2] || *end != '\0' ||
        milliseconds < 1 || milliseconds > 10000) {
        fprintf(stderr, "invalid grab duration: %s\n", argv[2]);
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    status = XGrabKeyboard(
        display, (Window)raw_window, False, GrabModeAsync, GrabModeAsync, CurrentTime);
    if (status != GrabSuccess) {
        fprintf(stderr, "keyboard grab failed with status %d\n", status);
        XCloseDisplay(display);
        return 1;
    }
    XSync(display, False);
    puts("grabbed");
    fflush(stdout);
    duration.tv_sec = milliseconds / 1000;
    duration.tv_nsec = (milliseconds % 1000) * 1000000L;
    while (nanosleep(&duration, &duration) != 0 && errno == EINTR) {}
    XUngrabKeyboard(display, CurrentTime);
    XSync(display, False);
    puts("released");
    XCloseDisplay(display);
    return 0;
}
#define _POSIX_C_SOURCE 200809L
