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
    const struct timespec delay = {.tv_sec = 0, .tv_nsec = 120000000};
    nanosleep(&delay, NULL);
}

static int send_request(Display *display, Window window, long x, long y,
                        long direction, long button) {
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = window;
    event.xclient.message_type =
        XInternAtom(display, "_NET_WM_MOVERESIZE", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = x;
    event.xclient.data.l[1] = y;
    event.xclient.data.l[2] = direction;
    event.xclient.data.l[3] = button;
    event.xclient.data.l[4] = 1;
    return XSendEvent(display, DefaultRootWindow(display), False,
                      SubstructureRedirectMask | SubstructureNotifyMask,
                      &event) != 0;
}

static int fake_key(Display *display, KeySym symbol, unsigned int modifiers) {
    KeyCode key = XKeysymToKeycode(display, symbol);
    KeyCode control = XKeysymToKeycode(display, XK_Control_L);
    KeyCode shift = XKeysymToKeycode(display, XK_Shift_L);
    if (key == 0 || (modifiers & ControlMask && control == 0)
        || (modifiers & ShiftMask && shift == 0)) return 0;
    if (modifiers & ControlMask) XTestFakeKeyEvent(display, control, True, 0);
    if (modifiers & ShiftMask) XTestFakeKeyEvent(display, shift, True, 0);
    XTestFakeKeyEvent(display, key, True, 0);
    XTestFakeKeyEvent(display, key, False, 0);
    if (modifiers & ShiftMask) XTestFakeKeyEvent(display, shift, False, 0);
    if (modifiers & ControlMask) XTestFakeKeyEvent(display, control, False, 0);
    XSync(display, False);
    settle();
    return 1;
}

static int pointer_operation(Display *display, Window window, const char *name,
                             int root_x, int root_y, unsigned int width,
                             unsigned int height) {
    long direction;
    int dx;
    int dy;
    int cancel;
    if (strcmp(name, "pointer-move") == 0) {
        direction = 8;
        root_x += 10;
        root_y += 10;
        dx = 48;
        dy = 32;
        cancel = 0;
    } else if (strcmp(name, "pointer-resize") == 0) {
        direction = 4;
        root_x += (int)width - 1;
        root_y += (int)height - 1;
        dx = 40;
        dy = 24;
        cancel = 0;
    } else if (strcmp(name, "pointer-cancel") == 0) {
        direction = 0;
        dx = -40;
        dy = -24;
        cancel = 1;
    } else {
        return 0;
    }

    XTestFakeMotionEvent(display, DefaultScreen(display), root_x, root_y, 0);
    XTestFakeButtonEvent(display, Button1, True, 0);
    XSync(display, False);
    if (!send_request(display, window, root_x, root_y, direction, Button1)) {
        XTestFakeButtonEvent(display, Button1, False, 0);
        XSync(display, False);
        return 0;
    }
    XSync(display, False);
    settle();
    XTestFakeRelativeMotionEvent(display, dx, dy, 0);
    XSync(display, False);
    settle();
    if (cancel && !send_request(display, window, 0, 0, 11, 0)) {
        XTestFakeButtonEvent(display, Button1, False, 0);
        XSync(display, False);
        return 0;
    }
    XTestFakeButtonEvent(display, Button1, False, 0);
    XSync(display, False);
    settle();
    return 1;
}

static int keyboard_operation(Display *display, Window window, const char *name) {
    if (strcmp(name, "keyboard-move") == 0) {
        if (!send_request(display, window, 0, 0, 10, 0)) return 0;
        XSync(display, False);
        settle();
        return fake_key(display, XK_Right, 0)
            && fake_key(display, XK_Right, 0)
            && fake_key(display, XK_Down, 0)
            && fake_key(display, XK_Return, 0);
    }
    if (strcmp(name, "keyboard-resize") == 0) {
        if (!send_request(display, window, 0, 0, 9, 0)) return 0;
        XSync(display, False);
        settle();
        return fake_key(display, XK_Right, 0)
            && fake_key(display, XK_Right, 0)
            && fake_key(display, XK_Down, 0)
            && fake_key(display, XK_Down, 0)
            && fake_key(display, XK_Return, 0);
    }
    if (strcmp(name, "keyboard-cancel") == 0) {
        if (!send_request(display, window, 0, 0, 10, 0)) return 0;
        XSync(display, False);
        settle();
        return fake_key(display, XK_Left, 0)
            && fake_key(display, XK_Up, 0)
            && fake_key(display, XK_Escape, 0);
    }
    if (strcmp(name, "keyboard-fine") == 0) {
        if (!send_request(display, window, 0, 0, 10, 0)) return 0;
        XSync(display, False);
        settle();
        return fake_key(display, XK_Right, ControlMask)
            && fake_key(display, XK_Down, ControlMask)
            && fake_key(display, XK_Return, 0);
    }
    if (strcmp(name, "keyboard-edge") == 0) {
        if (!send_request(display, window, 0, 0, 10, 0)) return 0;
        XSync(display, False);
        settle();
        return fake_key(display, XK_Right, ShiftMask)
            && fake_key(display, XK_Return, 0);
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr,
                "usage: %s WINDOW pointer-move|pointer-resize|pointer-cancel|"
                "keyboard-move|keyboard-resize|keyboard-cancel|keyboard-fine|"
                "keyboard-edge\n",
                argv[0]);
        return 2;
    }
    char *end = NULL;
    errno = 0;
    unsigned long raw_window = strtoul(argv[1], &end, 0);
    if (errno != 0 || end == argv[1] || *end != '\0') return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)raw_window;
    Window root;
    int x;
    int y;
    unsigned int width;
    unsigned int height;
    unsigned int border;
    unsigned int depth;
    if (!XGetGeometry(display, window, &root, &x, &y, &width, &height,
                      &border, &depth)) {
        XCloseDisplay(display);
        return 1;
    }
    Window child;
    int root_x;
    int root_y;
    if (!XTranslateCoordinates(display, window, DefaultRootWindow(display),
                               0, 0, &root_x, &root_y, &child)) {
        XCloseDisplay(display);
        return 1;
    }
    int success = strncmp(argv[2], "pointer-", 8) == 0
        ? pointer_operation(display, window, argv[2], root_x, root_y,
                            width, height)
        : keyboard_operation(display, window, argv[2]);
    XCloseDisplay(display);
    return success ? 0 : 2;
}
