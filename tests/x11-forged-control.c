#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "x11-forged-control: cannot open display\n");
        return 1;
    }

    Atom supporting_atom = XInternAtom(display, "_NET_SUPPORTING_WM_CHECK", False);
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long count = 0;
    unsigned long remaining = 0;
    unsigned char *value = NULL;
    int status = XGetWindowProperty(
        display,
        DefaultRootWindow(display),
        supporting_atom,
        0,
        1,
        False,
        XA_WINDOW,
        &actual_type,
        &actual_format,
        &count,
        &remaining,
        &value);
    if (status != Success || actual_type != XA_WINDOW || actual_format != 32 ||
        count != 1 || value == NULL) {
        fprintf(stderr, "x11-forged-control: no supporting window\n");
        if (value != NULL) XFree(value);
        XCloseDisplay(display);
        return 1;
    }
    Window supporting = *(Window *)value;
    XFree(value);

    XEvent event;
    memset(&event, 0, sizeof(event));
    event.xclient.type = ClientMessage;
    event.xclient.display = display;
    event.xclient.window = supporting;
    event.xclient.message_type = XInternAtom(display, "_NOBOX_CONTROL", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = 2; /* Obsolete unauthenticated shutdown opcode. */

    if (XSendEvent(display, supporting, False, NoEventMask, &event) == 0) {
        fprintf(stderr, "x11-forged-control: could not send client message\n");
        XCloseDisplay(display);
        return 1;
    }
    XSync(display, False);
    XCloseDisplay(display);
    return 0;
}
