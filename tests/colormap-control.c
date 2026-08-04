#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

static int resource(const char *value, unsigned long *result) {
    char *end = NULL;

    errno = 0;
    *result = strtoul(value, &end, 0);
    return errno == 0 && end != value && *end == '\0';
}

int main(int argc, char **argv) {
    Display *display;
    Window root;
    Atom property;
    unsigned long raw_window;
    int screen;

    if (argc < 2) {
        fprintf(stderr, "usage: colormap-control MODE [ARGS...]\n");
        return 2;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    screen = DefaultScreen(display);
    root = RootWindow(display, screen);
    property = XInternAtom(display, "WM_COLORMAP_WINDOWS", False);

    if (strcmp(argv[1], "list") == 0 && argc == 2) {
        int count = 0;
        Colormap *colormaps = XListInstalledColormaps(display, root, &count);
        int index;

        for (index = 0; index < count; ++index) {
            printf("0x%lx%s", colormaps[index], index + 1 == count ? "\n" : " ");
        }
        if (count == 0) putchar('\n');
        XFree(colormaps);
    } else if (strcmp(argv[1], "property") == 0 && argc >= 3 &&
               resource(argv[2], &raw_window)) {
        int count = argc - 3;
        Window *windows = calloc((size_t)(count == 0 ? 1 : count), sizeof(*windows));
        int index;

        if (windows == NULL) return 1;
        for (index = 0; index < count; ++index) {
            unsigned long value;
            if (!resource(argv[index + 3], &value)) return 2;
            windows[index] = (Window)value;
        }
        XChangeProperty(
            display,
            (Window)raw_window,
            property,
            XA_WINDOW,
            32,
            PropModeReplace,
            (unsigned char *)windows,
            count);
        free(windows);
    } else if (strcmp(argv[1], "repeat") == 0 && argc == 5 &&
               resource(argv[2], &raw_window)) {
        unsigned long raw_listed;
        unsigned long raw_count;
        Window *windows;
        size_t index;

        if (!resource(argv[3], &raw_listed) || !resource(argv[4], &raw_count) ||
            raw_count > 4096) {
            return 2;
        }
        windows = calloc((size_t)(raw_count == 0 ? 1 : raw_count), sizeof(*windows));
        if (windows == NULL) return 1;
        for (index = 0; index < (size_t)raw_count; ++index) {
            windows[index] = (Window)raw_listed;
        }
        XChangeProperty(
            display,
            (Window)raw_window,
            property,
            XA_WINDOW,
            32,
            PropModeReplace,
            (unsigned char *)windows,
            (int)raw_count);
        free(windows);
    } else if (strcmp(argv[1], "malformed") == 0 && argc == 3 &&
               resource(argv[2], &raw_window)) {
        unsigned long value = 1;
        XChangeProperty(
            display,
            (Window)raw_window,
            property,
            XA_CARDINAL,
            32,
            PropModeReplace,
            (unsigned char *)&value,
            1);
    } else if (strcmp(argv[1], "replace") == 0 && argc == 3 &&
               resource(argv[2], &raw_window)) {
        Colormap colormap = XCreateColormap(
            display, root, DefaultVisual(display, screen), AllocNone);

        XSetWindowColormap(display, (Window)raw_window, colormap);
        XSetCloseDownMode(display, RetainPermanent);
        printf("0x%lx\n", colormap);
    } else {
        fprintf(stderr, "invalid colormap-control invocation\n");
        XCloseDisplay(display);
        return 2;
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
