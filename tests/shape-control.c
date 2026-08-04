#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <X11/Xlib.h>
#include <X11/extensions/shape.h>

static Window parse_window(const char *value) {
    char *end = NULL;
    unsigned long parsed = strtoul(value, &end, 0);
    if (end == value || *end != '\0') {
        fprintf(stderr, "invalid window: %s\n", value);
        exit(2);
    }
    return (Window)parsed;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: shape-control WINDOW bounding|input|clear|inset\n");
        return 2;
    }
    Display *display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "could not open display\n");
        return 2;
    }
    int event_base = 0;
    int error_base = 0;
    if (!XShapeQueryExtension(display, &event_base, &error_base)) {
        fprintf(stderr, "X Shape is unavailable\n");
        XCloseDisplay(display);
        return 77;
    }

    Window window = parse_window(argv[1]);
    if (strcmp(argv[2], "bounding") == 0) {
        Bool bounding_shaped = False;
        Bool clip_shaped = False;
        int bounding_x = 0;
        int bounding_y = 0;
        int clip_x = 0;
        int clip_y = 0;
        unsigned int bounding_width = 0;
        unsigned int bounding_height = 0;
        unsigned int clip_width = 0;
        unsigned int clip_height = 0;
        if (!XShapeQueryExtents(display, window, &bounding_shaped, &bounding_x,
                                &bounding_y, &bounding_width, &bounding_height,
                                &clip_shaped, &clip_x, &clip_y, &clip_width,
                                &clip_height)) {
            fprintf(stderr, "could not query window shape\n");
            XCloseDisplay(display);
            return 1;
        }
        printf("%d %d %d %u %u\n", bounding_shaped, bounding_x, bounding_y,
               bounding_width, bounding_height);
    } else if (strcmp(argv[2], "input") == 0) {
#ifdef ShapeInput
        int count = 0;
        int ordering = 0;
        XRectangle *rectangles =
            XShapeGetRectangles(display, window, ShapeInput, &count, &ordering);
        if (rectangles == NULL || count <= 0) {
            fprintf(stderr, "could not query input shape\n");
            XFree(rectangles);
            XCloseDisplay(display);
            return 1;
        }
        int left = INT_MAX;
        int top = INT_MAX;
        int right = INT_MIN;
        int bottom = INT_MIN;
        for (int index = 0; index < count; ++index) {
            int rectangle_right = rectangles[index].x + rectangles[index].width;
            int rectangle_bottom = rectangles[index].y + rectangles[index].height;
            if (rectangles[index].x < left) left = rectangles[index].x;
            if (rectangles[index].y < top) top = rectangles[index].y;
            if (rectangle_right > right) right = rectangle_right;
            if (rectangle_bottom > bottom) bottom = rectangle_bottom;
        }
        printf("%d %d %d %d %d\n", count, left, top, right - left,
               bottom - top);
        XFree(rectangles);
#else
        fprintf(stderr, "X Shape input regions are unavailable\n");
        XCloseDisplay(display);
        return 77;
#endif
    } else if (strcmp(argv[2], "clear") == 0) {
        XShapeCombineMask(display, window, ShapeBounding, 0, 0, None, ShapeSet);
        XSync(display, False);
    } else if (strcmp(argv[2], "inset") == 0) {
        XWindowAttributes attributes;
        if (!XGetWindowAttributes(display, window, &attributes)) {
            fprintf(stderr, "could not read window geometry\n");
            XCloseDisplay(display);
            return 1;
        }
        XRectangle rectangle = {
            .x = 10,
            .y = 10,
            .width = (unsigned short)(attributes.width > 20 ? attributes.width - 20 : 1),
            .height = (unsigned short)(attributes.height > 20 ? attributes.height - 20 : 1),
        };
        XShapeCombineRectangles(display, window, ShapeBounding, 0, 0, &rectangle,
                                1, ShapeSet, Unsorted);
        XSync(display, False);
    } else {
        fprintf(stderr, "unknown operation: %s\n", argv[2]);
        XCloseDisplay(display);
        return 2;
    }
    XCloseDisplay(display);
    return 0;
}
