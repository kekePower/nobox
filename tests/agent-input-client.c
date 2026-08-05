/* A window that reports the input it receives, for agent-injection tests.
 *
 * Prints one line per event so the harness can prove that window-addressed
 * injection reached the right window at the right point inside it.
 */

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/keysym.h>

#include <stdio.h>

int main(int argc, char **argv) {
    const char *title = argc > 1 ? argv[1] : "agent-input-client";
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open the display\n", stderr);
        return 1;
    }
    int screen = DefaultScreen(display);
    Window window = XCreateSimpleWindow(
        display, RootWindow(display, screen), 0, 0, 240, 160, 0,
        BlackPixel(display, screen), WhitePixel(display, screen));
    XStoreName(display, window, title);
    XClassHint class_hint;
    class_hint.res_name = (char *)"agent-input";
    class_hint.res_class = (char *)"AgentInput";
    XSetClassHint(display, window, &class_hint);
    Atom delete_window = XInternAtom(display, "WM_DELETE_WINDOW", False);
    XSetWMProtocols(display, window, &delete_window, 1);
    XSelectInput(display, window,
                 KeyPressMask | ButtonPressMask | ButtonReleaseMask |
                     StructureNotifyMask | ExposureMask);
    XMapWindow(display, window);
    XFlush(display);

    GC gc = XCreateGC(display, window, 0, NULL);

    for (;;) {
        XEvent event;
        XNextEvent(display, &event);
        if (event.type == ButtonPress) {
            printf("button %u at %d,%d\n", event.xbutton.button,
                   event.xbutton.x, event.xbutton.y);
            fflush(stdout);
        } else if (event.type == KeyPress) {
            char buffer[16];
            KeySym keysym = NoSymbol;
            int length = XLookupString(&event.xkey, buffer, sizeof(buffer) - 1,
                                       &keysym, NULL);
            buffer[length > 0 ? length : 0] = '\0';
            printf("key %s text %s\n", XKeysymToString(keysym), buffer);
            fflush(stdout);
        } else if (event.type == ClientMessage &&
                   (Atom)event.xclient.data.l[0] == delete_window) {
            printf("closing\n");
            fflush(stdout);
            break;
        } else if (event.type == Expose && event.xexpose.count == 0) {
            /* A fixed marker lets the capture test distinguish the drawable's
             * top-left pixels from a same-sized crop at a non-zero origin. */
            XSetForeground(display, gc, BlackPixel(display, screen));
            XFillRectangle(display, window, gc, 0, 0, 32, 32);
            XFlush(display);
        }
    }
    XFreeGC(display, gc);
    XCloseDisplay(display);
    return 0;
}
