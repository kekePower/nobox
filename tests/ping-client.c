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
    Display *display;
    Window root;
    Window window;
    Atom protocols;
    Atom delete_window;
    Atom ping;
    Atom supported[2];
    XSizeHints hints = {0};
    int delay;

    if (argc != 3 ||
        (strcmp(argv[1], "responsive") != 0 && strcmp(argv[1], "late") != 0 &&
         strcmp(argv[1], "hung") != 0)) {
        fprintf(stderr, "usage: ping-client responsive|late|hung TITLE\n");
        return 2;
    }
    delay = strcmp(argv[1], "late") == 0 ? 6 : 0;
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    root = DefaultRootWindow(display);
    window = XCreateSimpleWindow(display, root, 100, 100, 320, 160, 0, 0, 0xffffff);
    protocols = XInternAtom(display, "WM_PROTOCOLS", False);
    delete_window = XInternAtom(display, "WM_DELETE_WINDOW", False);
    ping = XInternAtom(display, "_NET_WM_PING", False);
    supported[0] = delete_window;
    supported[1] = ping;
    XSetWMProtocols(display, window, supported, 2);
    XStoreName(display, window, argv[2]);
    hints.flags = PPosition;
    hints.x = 100;
    hints.y = 100;
    XSetWMNormalHints(display, window, &hints);
    XMapWindow(display, window);
    XFlush(display);
    printf("window 0x%lx\n", window);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);

    if (strcmp(argv[1], "hung") == 0) {
        while (running) pause();
    } else {
        while (running) {
            XEvent event;

            XNextEvent(display, &event);
            if (event.type != ClientMessage || event.xclient.message_type != protocols ||
                event.xclient.format != 32) {
                continue;
            }
            if ((Atom)event.xclient.data.l[0] == delete_window) {
                printf("delete %lu\n", (unsigned long)event.xclient.data.l[1]);
                fflush(stdout);
            } else if ((Atom)event.xclient.data.l[0] == ping) {
                if ((Window)event.xclient.data.l[2] != window) {
                    fprintf(stderr, "ping named the wrong client window\n");
                    return 1;
                }
                if (delay != 0) sleep((unsigned int)delay);
                event.xclient.window = root;
                XSendEvent(
                    display,
                    root,
                    False,
                    SubstructureRedirectMask | SubstructureNotifyMask,
                    &event);
                XFlush(display);
                printf(
                    "pong %lu 0x%lx\n",
                    (unsigned long)event.xclient.data.l[1],
                    (Window)event.xclient.data.l[2]);
                fflush(stdout);
            }
        }
    }
    XDestroyWindow(display, window);
    XCloseDisplay(display);
    return 0;
}
