#include <stdio.h>
#include <gtk/gtk.h>
#include <gdk/gdkx.h>

static gboolean report_focus(gpointer data) {
    GtkWidget *window = GTK_WIDGET(data);
    static gboolean was_active = FALSE;
    gboolean active = gtk_window_is_active(GTK_WINDOW(window));
    if (active && !was_active) {
        puts("focus=gtk");
        fflush(stdout);
    }
    was_active = active;
    return G_SOURCE_CONTINUE;
}

int main(int argc, char **argv) {
    gtk_init(&argc, &argv);
    gdk_set_program_class("NoboxXWaylandGtk");
    GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(window), "nobox XWayland GTK client");
    gtk_window_set_default_size(GTK_WINDOW(window), 240, 140);
    gtk_widget_set_name(window, "nobox-xwayland-gtk");
    g_signal_connect(window, "destroy", G_CALLBACK(gtk_main_quit), NULL);
    gtk_widget_show_all(window);
    while (gtk_events_pending()) {
        gtk_main_iteration();
    }
    GdkWindow *gdk_window = gtk_widget_get_window(window);
    if (gdk_window == NULL || !GDK_IS_X11_WINDOW(gdk_window)) {
        fputs("GTK client did not create an X11 window\n", stderr);
        return 2;
    }
    printf("window=0x%lx\n", GDK_WINDOW_XID(gdk_window));
    fflush(stdout);
    g_timeout_add(10, report_focus, window);
    gtk_main();
    return 0;
}
