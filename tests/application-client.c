#include <signal.h>
#include <stdio.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 80, 80, 360, 120, 0, 0, 0xffffff);
    XStoreName(display, window, "nobox rule dialog");

    XClassHint class_hint = {
        .res_name = "nobox-editor",
        .res_class = "RuleClient",
    };
    XSetClassHint(display, window, &class_hint);

    Atom role = XInternAtom(display, "WM_WINDOW_ROLE", False);
    const char role_value[] = "editor";
    XChangeProperty(display, window, role, XA_STRING, 8, PropModeReplace,
                    (const unsigned char *)role_value, sizeof(role_value) - 1);

    Atom type_property = XInternAtom(display, "_NET_WM_WINDOW_TYPE", False);
    Atom dialog = XInternAtom(display, "_NET_WM_WINDOW_TYPE_DIALOG", False);
    XChangeProperty(display, window, type_property, XA_ATOM, 32, PropModeReplace,
                    (const unsigned char *)&dialog, 1);

    XMapWindow(display, window);
    XFlush(display);
    printf("0x%lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
