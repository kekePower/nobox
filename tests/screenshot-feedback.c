#define _POSIX_C_SOURCE 200809L
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/keysym.h>
#include <X11/extensions/shape.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void pause_ms(long ms) {
    struct timespec delay = {.tv_sec = ms / 1000, .tv_nsec = ms % 1000 * 1000000};
    nanosleep(&delay, NULL);
}

static Window active(Display *d, Window root) {
    Atom type;
    int format;
    unsigned long count, remaining;
    unsigned char *data = NULL;
    Window result = None;
    XGetWindowProperty(d, root, XInternAtom(d, "_NET_ACTIVE_WINDOW", False),
                       0, 1, False, XA_WINDOW, &type, &format, &count, &remaining, &data);
    if (format == 32 && count == 1) result = *(Window *)data;
    if (data) XFree(data);
    return result;
}

int main(int argc, char **argv) {
    if (argc != 4) return 2; /* binary, output filename, decorated/fullscreen */
    Display *d = XOpenDisplay(NULL);
    if (!d) return 1;
    Window root = DefaultRootWindow(d);
    Window client = XCreateSimpleWindow(d, root, 100, 100, 320, 240, 0, 0, 0x245678);
    XStoreName(d, client, "screenshot feedback fixture");
    if (!strcmp(argv[3], "fullscreen")) {
        Atom state = XInternAtom(d, "_NET_WM_STATE_FULLSCREEN", False);
        XChangeProperty(d, client, XInternAtom(d, "_NET_WM_STATE", False), XA_ATOM,
                        32, PropModeReplace, (unsigned char *)&state, 1);
    }
    XMapWindow(d, client);
    XFlush(d);
    for (int i = 0; i < 300 && active(d, root) != client; ++i) pause_ms(10);
    if (active(d, root) != client) return 1;
    pause_ms(100);
    Window tree_root, frame, *children = NULL;
    unsigned int count;
    XQueryTree(d, client, &tree_root, &frame, &children, &count);
    if (children) XFree(children);
    XWindowAttributes initial;
    XGetWindowAttributes(d, client, &initial);
    /* A client that never repaints makes destructive feedback observable.
       This inset child also records exposures too brief for pixel polling. */
    Window content = XCreateSimpleWindow(d, client, 4, 4,
                                        initial.width - 8, initial.height - 8, 0, 0, 0x713957);
    XSelectInput(d, content, VisibilityChangeMask | ExposureMask);
    XMapWindow(d, content);
    XSelectInput(d, root, SubstructureNotifyMask);
    XSelectInput(d, client, StructureNotifyMask | PropertyChangeMask);
    XSelectInput(d, frame, StructureNotifyMask | PropertyChangeMask);
    XSync(d, False);
    pause_ms(100);
    while (XPending(d)) { XEvent event; XNextEvent(d, &event); }
    int x, y;
    Window child;
    XTranslateCoordinates(d, content, root, 0, 0, &x, &y, &child);
    Atom opacity = XInternAtom(d, "_NET_WM_WINDOW_OPACITY", False);
    FILE *baseline = tmpfile();
    if (!baseline) return 1;
    for (int mode = 0; mode < 4; ++mode) {
        unlink(argv[2]);
        pid_t process = 0;
        if (mode < 3) {
            process = fork();
            if (process == 0) {
                close(ConnectionNumber(d));
                if (mode == 0)
                    execl(argv[1], argv[1], "--window", "--file", argv[2], "--no-flash", (char *)NULL);
                else if (mode == 1)
                    execl(argv[1], argv[1], "--window", "--file", argv[2], (char *)NULL);
                else {
                    if (!freopen(argv[2], "wb", stdout)) _exit(127);
                    execl(argv[1], argv[1], "--window", "--stdout", (char *)NULL);
                }
                _exit(127);
            }
            if (process < 0) return 1;
        } else {
            KeyCode alt = XKeysymToKeycode(d, XK_Alt_L), print = XKeysymToKeycode(d, XK_Print);
            XTestFakeKeyEvent(d, alt, True, 0);
            XTestFakeKeyEvent(d, print, True, 10);
            XTestFakeKeyEvent(d, print, False, 10);
            XTestFakeKeyEvent(d, alt, False, 0);
            XFlush(d);
        }
        int outlines = 0, settled = 0;
        for (int tick = 0; tick < 3000; ++tick) {
            XSync(d, False);
            while (XPending(d)) {
                XEvent event;
                XNextEvent(d, &event);
                if ((event.type == VisibilityNotify && event.xvisibility.window == content
                     && event.xvisibility.state != VisibilityUnobscured)
                    || (event.type == Expose && event.xexpose.window == content)
                    || (event.type == UnmapNotify && (event.xunmap.window == client || event.xunmap.window == frame))
                    || (event.type == ReparentNotify && event.xreparent.window == client)
                    || (event.type == PropertyNotify && event.xproperty.atom == opacity)) {
                    fprintf(stderr, "capture disturbed the live window (event %d, mode %d)\n", event.type, mode);
                    return 1;
                }
                if (event.type == MapNotify && event.xmap.override_redirect) {
                    char *name = NULL;
                    XFetchName(d, event.xmap.window, &name);
                    int feedback = name && !strcmp(name, "nobox-screenshot feedback");
                    if (name) XFree(name);
                    if (!feedback) continue;
                    XWindowAttributes attrs;
                    XGetWindowAttributes(d, event.xmap.window, &attrs);
                    if (attrs.width > 2 && attrs.height > 2) {
                        fputs("feedback covered more than a border strip\n", stderr);
                        return 1;
                    }
                    int rectangles, ordering;
                    XRectangle *input = XShapeGetRectangles(d, event.xmap.window, ShapeInput, &rectangles, &ordering);
                    if (input) XFree(input);
                    if (rectangles != 0) { fputs("feedback intercepts input\n", stderr); return 1; }
                    ++outlines;
                }
            }
            XImage *image = XGetImage(d, root, x, y, initial.width - 8, initial.height - 8, AllPlanes, ZPixmap);
            if (!image) return 1;
            for (int row = 0; row < image->height; ++row)
                for (int col = 0; col < image->width; ++col)
                    if (XGetPixel(image, col, row) != 0x713957) {
                        fputs("capture changed live content pixels\n", stderr);
                        return 1;
                    }
            XDestroyImage(image);
            if (access(argv[2], F_OK) == 0 && ++settled >= 250) break;
            pause_ms(1);
        }
        if (settled < 250 || outlines != (mode % 2 == 0 ? 0 : 4) || active(d, root) != client) {
            fprintf(stderr, "capture mode %d: settled=%d outlines=%d\n", mode, settled, outlines);
            return 1;
        }
        if (process) {
            int status;
            if (waitpid(process, &status, 0) != process || !WIFEXITED(status) || WEXITSTATUS(status)) return 1;
        }
        FILE *capture = fopen(argv[2], "rb");
        if (!capture) return 1;
        rewind(baseline);
        int byte;
        while ((byte = fgetc(capture)) != EOF) {
            if (mode == 0) {
                if (fputc(byte, baseline) == EOF) return 1;
            } else if (fgetc(baseline) != byte) {
                fprintf(stderr, "feedback changed saved screenshot bytes in mode %d\n", mode);
                return 1;
            }
        }
        if (ferror(capture) || (mode != 0 && fgetc(baseline) != EOF)) return 1;
        fclose(capture);
    }
    fclose(baseline);
    XCloseDisplay(d);
    puts("live content, visibility, focus, and input stayed stable for silent, outlined, and Alt+Print captures");
    return 0;
}
