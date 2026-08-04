#include <X11/Xlib.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int parse_window(const char *value, Window *window) {
    char *end = NULL;
    errno = 0;
    unsigned long parsed = strtoul(value, &end, 0);
    if (errno != 0 || end == value || *end != '\0') return 0;
    *window = (Window)parsed;
    return 1;
}

static int32_t parse_i32(const char *value, int *valid) {
    char *end = NULL;
    errno = 0;
    long parsed = strtol(value, &end, 0);
    *valid = errno == 0 && end != value && *end == '\0'
        && parsed >= INT32_MIN && parsed <= INT32_MAX;
    return *valid ? (int32_t)parsed : 0;
}

static uint32_t parse_u32(const char *value, int *valid) {
    char *end = NULL;
    errno = 0;
    unsigned long parsed = strtoul(value, &end, 0);
    *valid = errno == 0 && end != value && *end == '\0' && parsed <= UINT32_MAX;
    return *valid ? (uint32_t)parsed : 0;
}

int main(int argc, char **argv) {
    Window window;
    if ((argc != 3 && argc != 9) || !parse_window(argv[2], &window)) {
        fprintf(stderr,
                "usage: %s close WINDOW | geometry WINDOW GRAVITY FLAGS X Y WIDTH HEIGHT\n",
                argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 1;
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = window;
    event.xclient.format = 32;
    if (strcmp(argv[1], "close") == 0 && argc == 3) {
        event.xclient.message_type = XInternAtom(display, "_NET_CLOSE_WINDOW", False);
        event.xclient.data.l[0] = CurrentTime;
        event.xclient.data.l[1] = 2;
    } else if (strcmp(argv[1], "geometry") == 0 && argc == 9) {
        int valid = 0;
        uint32_t gravity = parse_u32(argv[3], &valid);
        if (!valid || gravity > 255) {
            XCloseDisplay(display);
            return 2;
        }
        uint32_t flags = 0;
        if (strchr(argv[4], 'x') != NULL) flags |= 1U << 8;
        if (strchr(argv[4], 'y') != NULL) flags |= 1U << 9;
        if (strchr(argv[4], 'w') != NULL) flags |= 1U << 10;
        if (strchr(argv[4], 'h') != NULL) flags |= 1U << 11;
        int32_t x = parse_i32(argv[5], &valid);
        if (!valid) {
            XCloseDisplay(display);
            return 2;
        }
        int32_t y = parse_i32(argv[6], &valid);
        if (!valid) {
            XCloseDisplay(display);
            return 2;
        }
        uint32_t width = parse_u32(argv[7], &valid);
        if (!valid) {
            XCloseDisplay(display);
            return 2;
        }
        uint32_t height = parse_u32(argv[8], &valid);
        if (!valid) {
            XCloseDisplay(display);
            return 2;
        }
        event.xclient.message_type = XInternAtom(display, "_NET_MOVERESIZE_WINDOW", False);
        event.xclient.data.l[0] = (long)(gravity | flags | (2U << 12));
        event.xclient.data.l[1] = x;
        event.xclient.data.l[2] = y;
        event.xclient.data.l[3] = width;
        event.xclient.data.l[4] = height;
    } else {
        XCloseDisplay(display);
        return 2;
    }
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
