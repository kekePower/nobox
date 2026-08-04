#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <X11/Xlib.h>
#include <X11/extensions/Xfixes.h>

static int number(const char *value, long *result) {
    char *end = NULL;

    errno = 0;
    *result = strtol(value, &end, 0);
    return errno == 0 && end != value && *end == '\0';
}

int main(int argc, char **argv) {
    Display *display;
    XFixesCursorImage *image;
    Window root;
    Window child;
    long raw_window;
    long x;
    long y;
    int root_x;
    int root_y;
    int event_base;
    int error_base;
    uint64_t hash = UINT64_C(1469598103934665603);
    size_t index;
    size_t pixels;

    if (argc != 4 || !number(argv[1], &raw_window) || !number(argv[2], &x) ||
        !number(argv[3], &y)) {
        fprintf(stderr, "usage: cursor-image WINDOW X Y\n");
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    if (!XFixesQueryExtension(display, &event_base, &error_base)) {
        fprintf(stderr, "XFixes is unavailable\n");
        XCloseDisplay(display);
        return 77;
    }
    root = DefaultRootWindow(display);
    if (!XTranslateCoordinates(
            display,
            (Window)raw_window,
            root,
            (int)x,
            (int)y,
            &root_x,
            &root_y,
            &child)) {
        fprintf(stderr, "window is not on the default screen\n");
        XCloseDisplay(display);
        return 1;
    }
    XWarpPointer(display, None, root, 0, 0, 0, 0, root_x, root_y);
    XSync(display, False);
    image = XFixesGetCursorImage(display);
    if (image == NULL) {
        fprintf(stderr, "could not read the active cursor image\n");
        XCloseDisplay(display);
        return 1;
    }
    pixels = (size_t)image->width * (size_t)image->height;
    for (index = 0; index < pixels; ++index) {
        hash ^= (uint32_t)image->pixels[index];
        hash *= UINT64_C(1099511628211);
    }
    printf(
        "%u %u %u %u %016llx\n",
        image->width,
        image->height,
        image->xhot,
        image->yhot,
        (unsigned long long)hash);
    XFree(image);
    XCloseDisplay(display);
    return 0;
}
