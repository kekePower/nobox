#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xlib.h>

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr,
                "usage: %s WINDOW fullscreen|above|below|modal|skip-taskbar|"
                "skip-pager|attention|hidden|focused add|remove|toggle\n",
                argv[0]);
        return 2;
    }
    const char *state_name = strcmp(argv[2], "fullscreen") == 0
        ? "_NET_WM_STATE_FULLSCREEN"
        : strcmp(argv[2], "above") == 0
        ? "_NET_WM_STATE_ABOVE"
        : strcmp(argv[2], "below") == 0
        ? "_NET_WM_STATE_BELOW"
        : strcmp(argv[2], "modal") == 0
        ? "_NET_WM_STATE_MODAL"
        : strcmp(argv[2], "skip-taskbar") == 0
        ? "_NET_WM_STATE_SKIP_TASKBAR"
        : strcmp(argv[2], "skip-pager") == 0
        ? "_NET_WM_STATE_SKIP_PAGER"
        : strcmp(argv[2], "attention") == 0
        ? "_NET_WM_STATE_DEMANDS_ATTENTION"
        : strcmp(argv[2], "hidden") == 0
        ? "_NET_WM_STATE_HIDDEN"
        : strcmp(argv[2], "focused") == 0
        ? "_NET_WM_STATE_FOCUSED"
        : NULL;
    long action = strcmp(argv[3], "remove") == 0 ? 0
        : strcmp(argv[3], "add") == 0 ? 1
        : strcmp(argv[3], "toggle") == 0 ? 2
        : -1;
    if (state_name == NULL || action < 0) return 2;

    Display *display = XOpenDisplay(NULL);
    if (display == NULL) return 2;
    Window window = (Window)strtoul(argv[1], NULL, 0);
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = window;
    event.xclient.message_type = XInternAtom(display, "_NET_WM_STATE", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = action;
    event.xclient.data.l[1] = XInternAtom(display, state_name, False);
    event.xclient.data.l[3] = 1;
    XSendEvent(display, DefaultRootWindow(display), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &event);
    XFlush(display);
    XCloseDisplay(display);
    return 0;
}
