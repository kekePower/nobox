#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc > 2) {
        fprintf(stderr, "usage: %s [EXPECTED_RGB_HEX]\n", argv[0]);
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fputs("could not open DISPLAY\n", stderr);
        return 1;
    }

    Window root = DefaultRootWindow(display);
    Window returned_root = None;
    Window returned_parent = None;
    Window *children = NULL;
    unsigned int child_count = 0;
    if (!XQueryTree(display, root, &returned_root, &returned_parent, &children, &child_count)) {
        fputs("could not query root window\n", stderr);
        XCloseDisplay(display);
        return 1;
    }

    Window largest = None;
    unsigned long largest_area = 0;
    XWindowAttributes largest_attributes;
    for (unsigned int index = 0; index < child_count; ++index) {
        XWindowAttributes attributes;
        if (!XGetWindowAttributes(display, children[index], &attributes) ||
            attributes.map_state != IsViewable || attributes.width <= 0 || attributes.height <= 0) {
            continue;
        }
        unsigned long area =
            (unsigned long)attributes.width * (unsigned long)attributes.height;
        if (area > largest_area) {
            largest = children[index];
            largest_area = area;
            largest_attributes = attributes;
        }
    }
    if (children != NULL) {
        XFree(children);
    }
    if (largest == None) {
        fputs("no viewable child window\n", stderr);
        XCloseDisplay(display);
        return 1;
    }

    XImage *image = XGetImage(
        display,
        largest,
        largest_attributes.width / 2,
        largest_attributes.height / 2,
        1,
        1,
        AllPlanes,
        ZPixmap);
    if (image == NULL) {
        fputs("could not read nested compositor pixel\n", stderr);
        XCloseDisplay(display);
        return 1;
    }
    unsigned long pixel = XGetPixel(image, 0, 0) & 0xffffffUL;
    printf("pixel=0x%06lx\n", pixel);
    XDestroyImage(image);
    XCloseDisplay(display);
    if (argc == 2) {
        char *end = NULL;
        unsigned long expected = strtoul(argv[1], &end, 16) & 0xffffffUL;
        if (end == argv[1] || *end != '\0') {
            fputs("invalid expected RGB value\n", stderr);
            return 2;
        }
        return pixel == expected ? 0 : 1;
    }
    return pixel == 0 ? 0 : 1;
}
