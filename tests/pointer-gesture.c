#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>

static void settle(void) {
    const struct timespec delay = {.tv_sec = 0, .tv_nsec = 100000000L};
    nanosleep(&delay, NULL);
}

static int parse_number(const char *source, long minimum, long maximum,
                        long *result) {
    char *end = NULL;
    errno = 0;
    long value = strtol(source, &end, 0);
    if (errno != 0 || end == source || *end != '\0'
        || value < minimum || value > maximum) return 0;
    *result = value;
    return 1;
}

static void click(Display *display, unsigned int button) {
    XTestFakeButtonEvent(display, button, True, 0);
    XTestFakeButtonEvent(display, button, False, 0);
    XSync(display, False);
    settle();
}

int main(int argc, char **argv) {
    if (argc != 8 && argc != 9) {
        fprintf(stderr,
                "usage: %s WINDOW BUTTON click|double|drag X Y DX DY [super]\n",
                argv[0]);
        return 2;
    }
    if (argc == 9 && strcmp(argv[8], "super") != 0) return 2;
    long raw_window;
    long raw_button;
    long origin_x;
    long origin_y;
    long dx;
    long dy;
    if (!parse_number(argv[1], 1, 0xffffffffL, &raw_window)
        || !parse_number(argv[2], Button1, Button5, &raw_button)
        || !parse_number(argv[4], -32768, 32767, &origin_x)
        || !parse_number(argv[5], -32768, 32767, &origin_y)
        || !parse_number(argv[6], -32768, 32767, &dx)
        || !parse_number(argv[7], -32768, 32767, &dy)) return 2;
    if (strcmp(argv[3], "click") != 0
        && strcmp(argv[3], "double") != 0
        && strcmp(argv[3], "drag") != 0) return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 1;
    Window child;
    int root_x;
    int root_y;
    if (!XTranslateCoordinates(display, (Window)raw_window,
                               DefaultRootWindow(display),
                               (int)origin_x, (int)origin_y,
                               &root_x, &root_y, &child)) {
        XCloseDisplay(display);
        return 1;
    }
    XTestFakeMotionEvent(display, DefaultScreen(display), root_x, root_y, 0);
    XSync(display, False);
    settle();
    KeyCode modifier = 0;
    if (argc == 9) {
        modifier = XKeysymToKeycode(display, XK_Super_L);
        if (modifier == 0) {
            XCloseDisplay(display);
            return 1;
        }
        XTestFakeKeyEvent(display, modifier, True, 0);
        XSync(display, False);
        settle();
    }

    if (strcmp(argv[3], "click") == 0) {
        click(display, (unsigned int)raw_button);
    } else if (strcmp(argv[3], "double") == 0) {
        click(display, (unsigned int)raw_button);
        click(display, (unsigned int)raw_button);
    } else {
        XTestFakeButtonEvent(display, (unsigned int)raw_button, True, 0);
        XSync(display, False);
        settle();
        XTestFakeRelativeMotionEvent(display, (int)dx, (int)dy, 0);
        XSync(display, False);
        settle();
        XTestFakeButtonEvent(display, (unsigned int)raw_button, False, 0);
        XSync(display, False);
        settle();
    }
    if (modifier != 0) {
        XTestFakeKeyEvent(display, modifier, False, 0);
        XSync(display, False);
    }
    XCloseDisplay(display);
    return 0;
}
