#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    int shift = argc == 3 && strcmp(argv[1], "--shift") == 0;
    if ((!shift && argc != 2) || (shift && argc != 3)) {
        fprintf(stderr, "usage: press-key [--shift] KEYSYM\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open X display\n", stderr);
        return 1;
    }
    KeySym symbol = XStringToKeysym(argv[shift ? 2 : 1]);
    KeyCode key = XKeysymToKeycode(display, symbol);
    KeyCode super = XKeysymToKeycode(display, XK_Super_L);
    KeyCode shift_key = XKeysymToKeycode(display, XK_Shift_L);
    if (symbol == NoSymbol || key == 0 || super == 0 || (shift && shift_key == 0)) {
        fputs("requested keysym is unavailable\n", stderr);
        XCloseDisplay(display);
        return 1;
    }
    XTestFakeKeyEvent(display, super, True, 0);
    if (shift) {
        XTestFakeKeyEvent(display, shift_key, True, 0);
    }
    XTestFakeKeyEvent(display, key, True, 0);
    XTestFakeKeyEvent(display, key, False, 0);
    if (shift) {
        XTestFakeKeyEvent(display, shift_key, False, 0);
    }
    XTestFakeKeyEvent(display, super, False, 0);
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
