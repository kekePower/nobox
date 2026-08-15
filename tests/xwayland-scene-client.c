#include <signal.h>
#include <stdio.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open XWayland DISPLAY\n", stderr);
        return 2;
    }
    Window root = DefaultRootWindow(display);
    Window managed = XCreateSimpleWindow(
        display, root, 0, 0, 520, 320, 0, 0, 0xff0000);
    XStoreName(display, managed, "nobox XWayland managed scene");
    XClassHint class_hint = {
        .res_name = "nobox-xwayland-scene",
        .res_class = "NoboxXWaylandScene",
    };
    XSetClassHint(display, managed, &class_hint);
    XSelectInput(display, managed, ExposureMask | StructureNotifyMask);
    XSizeHints size_hints = {
        .flags = PMinSize | PMaxSize,
        .min_width = 520,
        .min_height = 320,
        .max_width = 520,
        .max_height = 320,
    };
    XSetWMNormalHints(display, managed, &size_hints);
    XMapWindow(display, managed);
    XClearWindow(display, managed);
    XSync(display, False);

    XSetWindowAttributes attributes = {
        .override_redirect = True,
        .background_pixel = 0x00ff00,
    };
    Window unmanaged = XCreateWindow(
        display, root, 380, 280, 40, 40, 0, CopyFromParent, InputOutput,
        CopyFromParent, CWOverrideRedirect | CWBackPixel, &attributes);
    XStoreName(display, unmanaged, "nobox XWayland unmanaged scene");
    XSelectInput(display, unmanaged, ExposureMask | StructureNotifyMask);
    XMapRaised(display, unmanaged);
    XClearWindow(display, unmanaged);
    XSync(display, False);

    printf("managed=0x%lx unmanaged=0x%lx\n", managed, unmanaged);
    fflush(stdout);
    signal(SIGTERM, stop);
    signal(SIGINT, stop);
    int focus_reported = 0;
    while (running) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == Expose) {
                XClearWindow(display, event.xexpose.window);
            }
        }
        if (!focus_reported) {
            Window focused = None;
            int revert_to = 0;
            XGetInputFocus(display, &focused, &revert_to);
            if (focused == managed) {
                puts("focus=managed");
                fflush(stdout);
                focus_reported = 1;
            }
        }
        usleep(10000);
    }

    XDestroyWindow(display, unmanaged);
    XDestroyWindow(display, managed);
    XCloseDisplay(display);
    return 0;
}
