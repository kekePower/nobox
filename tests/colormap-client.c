#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
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
    Window top;
    Window child;
    Window colormap_windows[2];
    Colormap top_colormap;
    Colormap child_colormap;
    XSetWindowAttributes attributes = {0};
    XSizeHints hints = {0};
    int screen;
    int x;
    int y;

    if (argc != 4) {
        fprintf(stderr, "usage: colormap-client TITLE X Y\n");
        return 2;
    }
    x = atoi(argv[2]);
    y = atoi(argv[3]);
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not connect to the X server\n");
        return 1;
    }
    screen = DefaultScreen(display);
    root = RootWindow(display, screen);
    top_colormap = XCreateColormap(display, root, DefaultVisual(display, screen), AllocNone);
    child_colormap = XCreateColormap(display, root, DefaultVisual(display, screen), AllocNone);

    attributes.background_pixel = 0;
    attributes.colormap = top_colormap;
    top = XCreateWindow(
        display,
        root,
        x,
        y,
        260,
        140,
        0,
        DefaultDepth(display, screen),
        InputOutput,
        DefaultVisual(display, screen),
        CWBackPixel | CWColormap,
        &attributes);
    attributes.colormap = child_colormap;
    child = XCreateWindow(
        display,
        top,
        20,
        20,
        220,
        100,
        0,
        DefaultDepth(display, screen),
        InputOutput,
        DefaultVisual(display, screen),
        CWBackPixel | CWColormap,
        &attributes);

    hints.flags = PPosition;
    hints.x = x;
    hints.y = y;
    XSetWMNormalHints(display, top, &hints);
    XStoreName(display, top, argv[1]);
    colormap_windows[0] = child;
    colormap_windows[1] = top;
    XChangeProperty(
        display,
        top,
        XInternAtom(display, "WM_COLORMAP_WINDOWS", False),
        XA_WINDOW,
        32,
        PropModeReplace,
        (unsigned char *)colormap_windows,
        2);
    XMapWindow(display, child);
    XMapWindow(display, top);
    XFlush(display);

    printf(
        "0x%lx 0x%lx 0x%lx 0x%lx 0x%lx\n",
        top,
        child,
        top_colormap,
        child_colormap,
        DefaultColormap(display, screen));
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    while (running) pause();

    XDestroyWindow(display, top);
    XFreeColormap(display, child_colormap);
    XFreeColormap(display, top_colormap);
    XCloseDisplay(display);
    return 0;
}
