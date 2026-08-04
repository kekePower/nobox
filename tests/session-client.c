#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
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
    if (argc != 5) {
        fprintf(stderr, "usage: %s SESSION_ID TITLE X Y\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    int x = atoi(argv[3]);
    int y = atoi(argv[4]);
    Window root = DefaultRootWindow(display);
    Window leader = XCreateSimpleWindow(display, root, -10, -10, 1, 1, 0, 0, 0);
    Window window = XCreateSimpleWindow(display, root, x, y, 260, 140, 0, 0, 0xffffff);
    Atom leader_atom = XInternAtom(display, "WM_CLIENT_LEADER", False);
    Atom session_id = XInternAtom(display, "SM_CLIENT_ID", False);
    Atom role = XInternAtom(display, "WM_WINDOW_ROLE", False);
    XChangeProperty(display, window, leader_atom, XA_WINDOW, 32,
                    PropModeReplace, (unsigned char *)&leader, 1);
    XChangeProperty(display, leader, session_id, XA_STRING, 8,
                    PropModeReplace, (const unsigned char *)argv[1],
                    (int)strlen(argv[1]));
    XSetCommand(display, leader, argv, argc);
    XClassHint class_hint = {
        .res_name = "session-client",
        .res_class = "NoboxSessionClient",
    };
    XSetClassHint(display, window, &class_hint);
    const char role_value[] = "document";
    XChangeProperty(display, window, role, XA_STRING, 8, PropModeReplace,
                    (const unsigned char *)role_value, sizeof(role_value) - 1);
    XSizeHints hints = {.flags = PPosition, .x = x, .y = y};
    XSetWMNormalHints(display, window, &hints);
    XStoreName(display, window, argv[2]);
    XMapWindow(display, window);
    XFlush(display);
    printf("0x%lx\n", window);
    fflush(stdout);

    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();
    XDestroyWindow(display, window);
    XDestroyWindow(display, leader);
    XCloseDisplay(display);
    return 0;
}
