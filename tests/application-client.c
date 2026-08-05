#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(int argc, char **argv) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;

    Window group = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 0, 0, 1, 1, 0, 0, 0);
    XClassHint group_class = {
        .res_name = "nobox-suite",
        .res_class = "RuleGroup",
    };
    XSetClassHint(display, group, &group_class);

    Window window = XCreateSimpleWindow(
        display, DefaultRootWindow(display), 80, 80, 360, 120, 0, 0, 0xffffff);
    const char *title = argc > 2 ? argv[2] : "nobox rule dialog";
    XStoreName(display, window, title);

    XClassHint class_hint = {
        .res_name = "nobox-editor",
        .res_class = "RuleClient",
    };
    XSetClassHint(display, window, &class_hint);
    if (argc > 3 && strcmp(argv[3], "positioned") == 0) {
        XSizeHints position_hints = {
            .flags = PPosition,
            .x = 80,
            .y = 80,
        };
        XSetWMNormalHints(display, window, &position_hints);
    }
    XWMHints *wm_hints = XAllocWMHints();
    if (wm_hints == NULL) return 3;
    wm_hints->flags = WindowGroupHint;
    wm_hints->window_group = group;
    XSetWMHints(display, window, wm_hints);
    XFree(wm_hints);

    Atom role = XInternAtom(display, "WM_WINDOW_ROLE", False);
    const char *role_value = argc > 1 ? argv[1] : "editor";
    XChangeProperty(display, window, role, XA_STRING, 8, PropModeReplace,
                    (const unsigned char *)role_value, (int)strlen(role_value));

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
    XDestroyWindow(display, group);
    XCloseDisplay(display);
    return 0;
}
