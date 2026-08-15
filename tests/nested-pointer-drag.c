#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <X11/Xlib.h>
#include <X11/extensions/XTest.h>

static void settle(void) {
    const struct timespec delay = {.tv_sec = 0, .tv_nsec = 400000000L};
    nanosleep(&delay, NULL);
}

static int parse_coordinate(const char *text, int *result) {
    char *end = NULL;
    errno = 0;
    long value = strtol(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' ||
        value < -32768 || value > 32767) {
        return 0;
    }
    *result = (int)value;
    return 1;
}

static Window largest_viewable_child(Display *display) {
    Window root = DefaultRootWindow(display);
    Window returned_root = None;
    Window returned_parent = None;
    Window *children = NULL;
    unsigned int child_count = 0;
    if (!XQueryTree(display, root, &returned_root, &returned_parent,
                    &children, &child_count)) {
        return None;
    }
    Window largest = None;
    unsigned long largest_area = 0;
    for (unsigned int index = 0; index < child_count; ++index) {
        XWindowAttributes attributes;
        if (!XGetWindowAttributes(display, children[index], &attributes) ||
            attributes.map_state != IsViewable || attributes.width <= 0 ||
            attributes.height <= 0) {
            continue;
        }
        unsigned long area =
            (unsigned long)attributes.width * (unsigned long)attributes.height;
        if (area > largest_area) {
            largest = children[index];
            largest_area = area;
        }
    }
    if (children != NULL) {
        XFree(children);
    }
    return largest;
}

int main(int argc, char **argv) {
    if (argc != 6 ||
        (strcmp(argv[1], "motion") != 0 && strcmp(argv[1], "click") != 0 &&
         strcmp(argv[1], "move") != 0 && strcmp(argv[1], "resize") != 0)) {
        fprintf(stderr, "usage: %s motion|click|move|resize X Y DX DY\n", argv[0]);
        return 2;
    }
    int x;
    int y;
    int dx;
    int dy;
    if (!parse_coordinate(argv[2], &x) || !parse_coordinate(argv[3], &y) ||
        !parse_coordinate(argv[4], &dx) || !parse_coordinate(argv[5], &dy)) {
        return 2;
    }

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        return 1;
    }
    Window nested = largest_viewable_child(display);
    Window root = DefaultRootWindow(display);
    Window child = None;
    int root_x;
    int root_y;
    if (nested == None ||
        !XTranslateCoordinates(display, nested, root, x, y, &root_x, &root_y,
                               &child)) {
        XCloseDisplay(display);
        return 1;
    }
    XTestFakeMotionEvent(display, DefaultScreen(display), root_x, root_y, 0);
    XSync(display, False);
    settle();
    unsigned int button = strcmp(argv[1], "move") == 0 ||
                                  strcmp(argv[1], "click") == 0
        ? Button1
        : strcmp(argv[1], "resize") == 0 ? Button3
        : 0;
    if (button != 0) {
        XTestFakeButtonEvent(display, button, True, 0);
        XSync(display, False);
        settle();
    }
    if (dx != 0 || dy != 0) {
        XTestFakeMotionEvent(display, DefaultScreen(display),
                             root_x + dx, root_y + dy, 0);
        XSync(display, False);
        settle();
    }
    if (button != 0) {
        XTestFakeButtonEvent(display, button, False, 0);
        XSync(display, False);
        settle();
    }
    XCloseDisplay(display);
    return 0;
}
