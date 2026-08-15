#include <signal.h>
#include <stdio.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>

static volatile sig_atomic_t running = 1;
static volatile sig_atomic_t send_spoof = 0;

static void stop(int signal_number) {
    (void)signal_number;
    running = 0;
}

static void spoof(int signal_number) {
    (void)signal_number;
    send_spoof = 1;
}

static int send_moveresize(Display *display, Window window, int x, int y,
                           long direction, unsigned int button) {
    XEvent request = {0};
    request.xclient.type = ClientMessage;
    request.xclient.display = display;
    request.xclient.window = window;
    request.xclient.message_type =
        XInternAtom(display, "_NET_WM_MOVERESIZE", False);
    request.xclient.format = 32;
    request.xclient.data.l[0] = x;
    request.xclient.data.l[1] = y;
    request.xclient.data.l[2] = direction;
    request.xclient.data.l[3] = button;
    request.xclient.data.l[4] = 1;
    return XSendEvent(display, DefaultRootWindow(display), False,
                      SubstructureRedirectMask | SubstructureNotifyMask,
                      &request) != 0;
}

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open XWayland DISPLAY\n", stderr);
        return 2;
    }
    Window root = DefaultRootWindow(display);
    Window managed = XCreateSimpleWindow(
        display, root, 0, 0, 523, 480, 0, 0, 0xff0000);
    XStoreName(display, managed, "nobox XWayland managed scene");
    XClassHint class_hint = {
        .res_name = "nobox-xwayland-scene",
        .res_class = "NoboxXWaylandScene",
    };
    XSetClassHint(display, managed, &class_hint);
    XSelectInput(display, managed,
                 ExposureMask | StructureNotifyMask | ButtonPressMask);
    XSizeHints size_hints = {
        .flags = PMinSize | PMaxSize | PBaseSize | PResizeInc | PAspect,
        .min_width = 100,
        .min_height = 80,
        .max_width = 600,
        .max_height = 500,
        .base_width = 100,
        .base_height = 80,
        .width_inc = 20,
        .height_inc = 10,
        .min_aspect = { .x = 3, .y = 2 },
        .max_aspect = { .x = 2, .y = 1 },
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
    signal(SIGUSR1, spoof);
    int focus_reported = 0;
    int geometry_reported = 0;
    while (running) {
        while (XPending(display) > 0) {
            XEvent event;
            XNextEvent(display, &event);
            if (event.type == Expose) {
                XClearWindow(display, event.xexpose.window);
            } else if (event.type == ButtonPress &&
                       event.xbutton.window == managed) {
                long direction = event.xbutton.button == Button1 ? 8
                    : event.xbutton.button == Button3 ? 4
                    : -1;
                if (direction >= 0 &&
                    send_moveresize(display, managed, event.xbutton.x_root,
                                    event.xbutton.y_root, direction,
                                    event.xbutton.button)) {
                    puts(direction == 8 ? "request=move" : "request=resize");
                    fflush(stdout);
                    XFlush(display);
                }
            }
        }
        if (send_spoof) {
            send_spoof = 0;
            if (send_moveresize(display, managed, 0, 0, 8, Button1)) {
                puts("request=spoof");
                fflush(stdout);
                XFlush(display);
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
        if (!geometry_reported) {
            XWindowAttributes current;
            if (XGetWindowAttributes(display, managed, &current) != 0 &&
                current.width == 520 && current.height == 360) {
                puts("geometry=520x360");
                fflush(stdout);
                geometry_reported = 1;
            }
        }
        usleep(10000);
    }

    XDestroyWindow(display, unmanaged);
    XDestroyWindow(display, managed);
    XCloseDisplay(display);
    return 0;
}
