#define _POSIX_C_SOURCE 200809L

#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

int main(int argc, char **argv) {
    int shift = 0;
    int alt = 0;
    int cancel = 0;
    long repeat = 1;
    long hold_ms = 0;
    int argument = 1;
    while (argument < argc - 1) {
        if (strcmp(argv[argument], "--shift") == 0) {
            shift = 1;
            ++argument;
        } else if (strcmp(argv[argument], "--alt") == 0) {
            alt = 1;
            ++argument;
        } else if (strcmp(argv[argument], "--cancel") == 0) {
            cancel = 1;
            ++argument;
        } else if (strcmp(argv[argument], "--repeat") == 0 && argument + 1 < argc - 1) {
            char *end = NULL;
            repeat = strtol(argv[argument + 1], &end, 10);
            if (end == argv[argument + 1] || *end != '\0' || repeat < 1 || repeat > 100) {
                fputs("repeat must be between 1 and 100\n", stderr);
                return 2;
            }
            argument += 2;
        } else if (strcmp(argv[argument], "--hold-ms") == 0 && argument + 1 < argc - 1) {
            char *end = NULL;
            hold_ms = strtol(argv[argument + 1], &end, 10);
            if (end == argv[argument + 1] || *end != '\0' || hold_ms < 0 || hold_ms > 5000) {
                fputs("hold time must be between 0 and 5000 milliseconds\n", stderr);
                return 2;
            }
            argument += 2;
        } else {
            fprintf(stderr, "usage: press-key [--alt] [--shift] [--cancel] [--repeat N] [--hold-ms N] KEYSYM\n");
            return 2;
        }
    }
    if (argument != argc - 1) {
        fprintf(stderr, "usage: press-key [--alt] [--shift] [--cancel] [--repeat N] [--hold-ms N] KEYSYM\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    KeySym symbol = XStringToKeysym(argv[argument]);
    KeyCode key = XKeysymToKeycode(display, symbol);
    KeyCode super = XKeysymToKeycode(display, XK_Super_L);
    KeyCode alt_key = XKeysymToKeycode(display, XK_Alt_L);
    KeyCode shift_key = XKeysymToKeycode(display, XK_Shift_L);
    KeyCode escape_key = XKeysymToKeycode(display, XK_Escape);
    KeyCode modifier = alt ? alt_key : super;
    if (symbol == NoSymbol || key == 0 || modifier == 0 || (shift && shift_key == 0) ||
        (cancel && escape_key == 0)) {
        fputs("requested keysym is unavailable\n", stderr);
        XCloseDisplay(display);
        return 1;
    }
    XTestFakeKeyEvent(display, modifier, True, 0);
    if (shift) {
        XTestFakeKeyEvent(display, shift_key, True, 0);
    }
    for (long press = 0; press < repeat; ++press) {
        XTestFakeKeyEvent(display, key, True, 10);
        XTestFakeKeyEvent(display, key, False, 10);
        XSync(display, False);
        const struct timespec pause = {.tv_sec = 0, .tv_nsec = 50000000L};
        nanosleep(&pause, NULL);
    }
    if (cancel) {
        XTestFakeKeyEvent(display, escape_key, True, 0);
        XTestFakeKeyEvent(display, escape_key, False, 0);
        XSync(display, False);
    }
    if (hold_ms > 0) {
        const struct timespec hold = {
            .tv_sec = hold_ms / 1000,
            .tv_nsec = (hold_ms % 1000) * 1000000L,
        };
        nanosleep(&hold, NULL);
    }
    if (shift) {
        XTestFakeKeyEvent(display, shift_key, False, 0);
    }
    XTestFakeKeyEvent(display, modifier, False, 0);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
