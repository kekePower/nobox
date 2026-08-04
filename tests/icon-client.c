#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 80, 80, 300, 140, 0, 0, 0xffffff);
    XStoreName(display, window, "nobox icon regression");
    Atom icon_atom = XInternAtom(display, "_NET_WM_ICON", False);
    size_t pixel_count = 32U * 32U;
    unsigned long *values = calloc(pixel_count + 2U, sizeof(*values));
    if (values == NULL) {
        XCloseDisplay(display);
        return 2;
    }
    values[0] = 32;
    values[1] = 32;
    for (size_t index = 0; index < pixel_count; ++index) {
        values[index + 2U] = 0xff11aa44UL;
    }
    XChangeProperty(display, window, icon_atom, XA_CARDINAL, 32, PropModeReplace,
                    (unsigned char *)values, (int)(pixel_count + 2U));
    free(values);

    XMapWindow(display, window);
    XSync(display, False);
    printf("%#lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
