#include <gtk/gtk.h>
#include <stdio.h>

static gboolean finish(gpointer data)
{
    gtk_widget_destroy(GTK_WIDGET(data));
    return G_SOURCE_REMOVE;
}

int main(int argc, char **argv)
{
    if (!gtk_init_check(&argc, &argv)) {
        fputs("GTK-WINDOW: cannot open display\n", stderr);
        return 1;
    }

    GtkWidget *window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(window), "FerrionOS GTK window probe");
    gtk_window_set_default_size(GTK_WINDOW(window), 360, 200);
    gtk_window_set_resizable(GTK_WINDOW(window), FALSE);
    gtk_window_set_position(GTK_WINDOW(window), GTK_WIN_POS_CENTER);
    gtk_widget_set_name(window, "gtk-window-probe");

    GtkCssProvider *css = gtk_css_provider_new();
    GError *error = NULL;
    if (!gtk_css_provider_load_from_data(css,
            "#gtk-window-probe { background-color: #2468ac; }"
            "#gtk-window-probe label { color: #ffffff; font: 24px Sans; }",
            -1, &error)) {
        fprintf(stderr, "GTK-WINDOW: CSS: %s\n", error->message);
        g_error_free(error);
        g_object_unref(css);
        gtk_widget_destroy(window);
        return 1;
    }
    gtk_style_context_add_provider_for_screen(gdk_screen_get_default(),
        GTK_STYLE_PROVIDER(css), GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);
    g_object_unref(css);
    gtk_container_add(GTK_CONTAINER(window), gtk_label_new("GTK on FerrionOS"));
    g_signal_connect(window, "destroy", G_CALLBACK(gtk_main_quit), NULL);
    gtk_widget_show_all(window);
    printf("GTK-WINDOW: display=%s type=%s\n",
        gdk_display_get_name(gdk_display_get_default()),
        G_OBJECT_TYPE_NAME(gdk_display_get_default()));
    fflush(stdout);
    g_timeout_add(8000, finish, window);
    gtk_main();
    puts("GTK-WINDOW: closed");
    return 0;
}
