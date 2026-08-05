#include <X11/Xlib.h>
#include <X11/extensions/XTest.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static Window parse_window(const char *text) {
    char *end = NULL;
    unsigned long value = strtoul(text, &end, 0);
    if (text[0] == '\0' || end == NULL || end[0] != '\0') {
        fprintf(stderr, "invalid window id: %s\n", text);
        exit(2);
    }
    return (Window)value;
}

static void move_to_window(Display *display, Window window) {
    Window root;
    int x;
    int y;
    unsigned int width;
    unsigned int height;
    unsigned int border;
    unsigned int depth;
    if (!XGetGeometry(display, window, &root, &x, &y, &width, &height,
                      &border, &depth)) {
        fprintf(stderr, "could not query window geometry\n");
        exit(1);
    }
    Window child;
    int root_x;
    int root_y;
    if (!XTranslateCoordinates(display, window, DefaultRootWindow(display),
                               (int)(width / 2), (int)(height / 2), &root_x,
                               &root_y, &child)) {
        fprintf(stderr, "could not translate window coordinates\n");
        exit(1);
    }
    XTestFakeMotionEvent(display, DefaultScreen(display), root_x, root_y,
                         CurrentTime);
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: button-input WINDOW move|press|release\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not open display\n");
        return 1;
    }
    Window window = parse_window(argv[1]);
    if (strcmp(argv[2], "move") == 0) {
        move_to_window(display, window);
    } else if (strcmp(argv[2], "press") == 0) {
        XTestFakeButtonEvent(display, 1, True, CurrentTime);
    } else if (strcmp(argv[2], "release") == 0) {
        XTestFakeButtonEvent(display, 1, False, CurrentTime);
    } else {
        fprintf(stderr, "unknown operation: %s\n", argv[2]);
        XCloseDisplay(display);
        return 2;
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
