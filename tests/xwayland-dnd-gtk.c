#include <stdio.h>
#include <string.h>

#include <gtk/gtk.h>
#include <gdk/gdkx.h>

static const char payload[] = "nobox-cross-dnd";

static void report_drag_begin(GtkWidget *widget, GdkDragContext *context,
                              gpointer data) {
    (void)widget;
    (void)context;
    (void)data;
    puts("dnd-begin");
    fflush(stdout);
}

static void report_drag_end(GtkWidget *widget, GdkDragContext *context,
                            gpointer data) {
    (void)widget;
    (void)context;
    (void)data;
    puts("dnd-end");
    fflush(stdout);
}

static void provide_drag_data(GtkWidget *widget, GdkDragContext *context,
                              GtkSelectionData *selection, guint info,
                              guint time, gpointer data) {
    (void)widget;
    (void)context;
    (void)info;
    (void)time;
    (void)data;
    puts("dnd-data-requested");
    fflush(stdout);
    gtk_selection_data_set(selection,
                           gdk_atom_intern_static_string("text/plain;charset=utf-8"),
                           8, (const guchar *)payload, (gint)strlen(payload));
}

static void receive_drag_data(GtkWidget *widget, GdkDragContext *context,
                              gint x, gint y, GtkSelectionData *selection,
                              guint info, guint time, gpointer data) {
    (void)widget;
    (void)x;
    (void)y;
    (void)info;
    (void)data;
    gint length = 0;
    const guchar *received = gtk_selection_data_get_data_with_length(selection, &length);
    gboolean valid = received != NULL && length == (gint)strlen(payload) &&
                     memcmp(received, payload, (size_t)length) == 0;
    gtk_drag_finish(context, valid, FALSE, time);
    puts(valid ? "dnd-received=nobox-cross-dnd" : "dnd-received=invalid");
    fflush(stdout);
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "source") != 0 &&
                      strcmp(argv[1], "target") != 0)) {
        fprintf(stderr, "usage: %s source|target\n", argv[0]);
        return 2;
    }
    gtk_init(&argc, &argv);
    gdk_set_program_class(strcmp(argv[1], "source") == 0
                              ? "NoboxCrossDndSource"
                              : "NoboxCrossDndTarget");
    GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(window), strcmp(argv[1], "source") == 0
                                                 ? "nobox cross DND source"
                                                 : "nobox cross DND target");
    gtk_window_set_default_size(GTK_WINDOW(window), 180, 100);
    GtkWidget *label = gtk_label_new(strcmp(argv[1], "source") == 0
                                         ? "Nobox cross-protocol drag source"
                                         : "Nobox cross-protocol drop target");
    gtk_container_add(GTK_CONTAINER(window), label);
    g_signal_connect(window, "destroy", G_CALLBACK(gtk_main_quit), NULL);
    GtkTargetEntry targets[] = {
        {(gchar *)"text/plain;charset=utf-8", 0, 0},
        {(gchar *)"UTF8_STRING", 0, 1},
    };
    if (strcmp(argv[1], "source") == 0) {
        gtk_drag_source_set(window, GDK_BUTTON1_MASK, targets, 2, GDK_ACTION_COPY);
        g_signal_connect(window, "drag-data-get", G_CALLBACK(provide_drag_data), NULL);
        g_signal_connect(window, "drag-begin", G_CALLBACK(report_drag_begin), NULL);
        g_signal_connect(window, "drag-end", G_CALLBACK(report_drag_end), NULL);
    } else {
        gtk_drag_dest_set(window, GTK_DEST_DEFAULT_ALL, targets, 2, GDK_ACTION_COPY);
        g_signal_connect(window, "drag-data-received", G_CALLBACK(receive_drag_data), NULL);
    }
    gtk_widget_show_all(window);
    while (gtk_events_pending()) {
        gtk_main_iteration();
    }
    GdkWindow *gdk_window = gtk_widget_get_window(window);
    if (gdk_window != NULL && GDK_IS_X11_WINDOW(gdk_window)) {
        printf("window=0x%lx\n", GDK_WINDOW_XID(gdk_window));
    } else {
        puts("window=wayland");
    }
    fflush(stdout);
    gtk_main();
    return 0;
}
