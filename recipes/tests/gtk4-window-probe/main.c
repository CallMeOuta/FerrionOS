#include <adwaita.h>
#include <appstream.h>
#include <gtk/gtk.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

enum {
    WINDOW_WIDTH = 480,
    WINDOW_HEIGHT = 320,
    MAX_RUN_MS = 20000,
    ABOUT_DISPLAY_MS = 2000,
};

typedef struct {
    GtkWidget *window;
    GtkWidget *box;
    GtkWidget *label;
    GtkCssProvider *css;
    const char *about_gresource;
    const char *about_resource_path;
    bool about_present;
    bool appstream_releases;
    bool saw_key;
    int exit_code;
} ProbeState;

static void apply_css(ProbeState *state, const char *css_text)
{
    gtk_css_provider_load_from_string(state->css, css_text);
}

static gboolean on_timeout_close(gpointer user_data)
{
    ProbeState *state = (ProbeState *)user_data;
    if (state->window != NULL) {
        gtk_window_close(GTK_WINDOW(state->window));
    }
    return G_SOURCE_REMOVE;
}

static gboolean on_key_pressed(
    GtkEventControllerKey *controller,
    guint keyval,
    guint keycode,
    GdkModifierType mods,
    gpointer user_data
)
{
    (void)controller;
    (void)keycode;
    (void)mods;
    ProbeState *state = (ProbeState *)user_data;

    if (keyval == GDK_KEY_k || keyval == GDK_KEY_K) {
        state->saw_key = true;
        apply_css(
            state,
            "window#gtk4-window-probe, box#gtk4-probe-box { background-color: #22aa44; }"
            "label#gtk4-probe-label { color: #ffffff; font-size: 24px; }"
        );
        gtk_label_set_text(GTK_LABEL(state->label), "KEY RECEIVED");
        printf("GTK4-WINDOW: key=k\n");
        fflush(stdout);
        return TRUE;
    }

    if (keyval == GDK_KEY_q || keyval == GDK_KEY_Q) {
        printf("GTK4-WINDOW: quit=key\n");
        fflush(stdout);
        gtk_window_close(GTK_WINDOW(state->window));
        return TRUE;
    }

    return FALSE;
}

static void on_window_realize(GtkWidget *widget, gpointer user_data)
{
    ProbeState *state = (ProbeState *)user_data;
    GdkDisplay *display = gtk_widget_get_display(widget);
    GskRenderer *renderer = gtk_native_get_renderer(GTK_NATIVE(widget));
    const char *disp_name = display != NULL ? gdk_display_get_name(display) : "null";
    const char *disp_type = display != NULL ? G_OBJECT_TYPE_NAME(display) : "null";
    const char *rend_type = renderer != NULL ? G_OBJECT_TYPE_NAME(renderer) : "null";

    printf("GTK4-WINDOW: display=%s type=%s renderer=%s\n", disp_name, disp_type, rend_type);
    fflush(stdout);

    if (state->about_gresource != NULL && state->about_present) {
        AdwDialog *dialog = adw_about_dialog_new_from_appdata(state->about_resource_path, NULL);
        if (dialog == NULL) {
            fprintf(stderr, "GTK4-ABOUT: adw_about_dialog_new_from_appdata returned NULL\n");
            state->exit_code = 1;
            gtk_window_close(GTK_WINDOW(widget));
            return;
        }
        const char *app_name = adw_about_dialog_get_application_name(ADW_ABOUT_DIALOG(dialog));
        const char *version = adw_about_dialog_get_version(ADW_ABOUT_DIALOG(dialog));
        printf(
            "GTK4-ABOUT: app_name=%s version=%s presented=1\n",
            app_name != NULL ? app_name : "",
            version != NULL ? version : ""
        );
        fflush(stdout);
        adw_dialog_present(dialog, widget);
        g_timeout_add(ABOUT_DISPLAY_MS, on_timeout_close, state);
    }
}

static void on_activate(GtkApplication *app, gpointer user_data)
{
    ProbeState *state = (ProbeState *)user_data;
    GdkDisplay *display = gdk_display_get_default();
    if (display == NULL) {
        fprintf(stderr, "GTK4-WINDOW: no default display\n");
        state->exit_code = 1;
        g_application_quit(G_APPLICATION(app));
        return;
    }

    state->window = adw_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(state->window), "FerrionOS GTK4 probe");
    gtk_window_set_default_size(GTK_WINDOW(state->window), WINDOW_WIDTH, WINDOW_HEIGHT);
    gtk_window_set_resizable(GTK_WINDOW(state->window), FALSE);
    gtk_widget_set_name(state->window, "gtk4-window-probe");

    state->css = gtk_css_provider_new();
    apply_css(
        state,
        "window#gtk4-window-probe, box#gtk4-probe-box { background-color: #2468ac; }"
        "label#gtk4-probe-label { color: #ffffff; font-size: 24px; }"
    );
    gtk_style_context_add_provider_for_display(
        display,
        GTK_STYLE_PROVIDER(state->css),
        GTK_STYLE_PROVIDER_PRIORITY_APPLICATION
    );

    state->box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
    gtk_widget_set_name(state->box, "gtk4-probe-box");
    gtk_widget_set_hexpand(state->box, TRUE);
    gtk_widget_set_vexpand(state->box, TRUE);

    state->label = gtk_label_new("GTK4 on FerrionOS");
    gtk_widget_set_name(state->label, "gtk4-probe-label");
    gtk_widget_set_hexpand(state->label, TRUE);
    gtk_widget_set_vexpand(state->label, TRUE);
    gtk_widget_set_halign(state->label, GTK_ALIGN_CENTER);
    gtk_widget_set_valign(state->label, GTK_ALIGN_CENTER);
    gtk_box_append(GTK_BOX(state->box), state->label);

    adw_application_window_set_content(ADW_APPLICATION_WINDOW(state->window), state->box);

    GtkEventController *key_ctrl = gtk_event_controller_key_new();
    g_signal_connect(key_ctrl, "key-pressed", G_CALLBACK(on_key_pressed), state);
    gtk_widget_add_controller(state->window, key_ctrl);

    g_signal_connect(state->window, "realize", G_CALLBACK(on_window_realize), state);

    g_timeout_add(MAX_RUN_MS, on_timeout_close, state);
    gtk_window_present(GTK_WINDOW(state->window));
}

static int run_appstream_releases(const char *gresource_path, const char *resource_path)
{
    GError *error = NULL;
    GResource *resource = g_resource_load(gresource_path, &error);
    if (resource == NULL) {
        fprintf(stderr, "APPSTREAM-RELEASES: failed to load gresource %s: %s\n",
            gresource_path, error != NULL ? error->message : "unknown");
        g_clear_error(&error);
        return 1;
    }

    GBytes *bytes = g_resource_lookup_data(resource, resource_path, G_RESOURCE_LOOKUP_FLAGS_NONE, &error);
    if (bytes == NULL) {
        fprintf(stderr, "APPSTREAM-RELEASES: failed to lookup %s: %s\n",
            resource_path, error != NULL ? error->message : "unknown");
        g_clear_error(&error);
        g_resource_unref(resource);
        return 1;
    }

    AsMetadata *metad = as_metadata_new();
    gboolean parsed = as_metadata_parse_bytes(metad, bytes, AS_FORMAT_KIND_XML, &error);
    g_bytes_unref(bytes);
    if (!parsed) {
        fprintf(stderr, "APPSTREAM-RELEASES: parse failed: %s\n",
            error != NULL ? error->message : "unknown");
        g_clear_error(&error);
        g_object_unref(metad);
        g_resource_unref(resource);
        return 1;
    }

    AsComponent *cpt = as_metadata_get_component(metad);
    if (cpt == NULL) {
        fprintf(stderr, "APPSTREAM-RELEASES: missing component\n");
        g_object_unref(metad);
        g_resource_unref(resource);
        return 1;
    }

    AsReleaseList *rels = as_component_load_releases(cpt, TRUE, &error);
    if (rels != NULL) {
        guint count = as_release_list_len(rels);
        const char *ver = "";
        if (count > 0) {
            AsRelease *rel = as_release_list_index(rels, 0);
            if (rel != NULL && as_release_get_version(rel) != NULL) {
                ver = as_release_get_version(rel);
            }
        }
        printf("APPSTREAM-RELEASES: loaded=1 count=%u version=%s\n", count, ver);
        fflush(stdout);
    } else {
        const char *dom = error != NULL ? g_quark_to_string(error->domain) : "none";
        int code = error != NULL ? error->code : 0;
        printf("APPSTREAM-RELEASES: loaded=0 count=0 version= domain=%s code=%d\n", dom, code);
        fflush(stdout);
        g_clear_error(&error);
    }

    g_object_unref(metad);
    g_resource_unref(resource);
    return 0;
}

int main(int argc, char **argv)
{
    ProbeState state;
    memset(&state, 0, sizeof(state));

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--about-gresource") == 0 && i + 2 < argc) {
            state.about_gresource = argv[i + 1];
            state.about_resource_path = argv[i + 2];
            i += 2;
        } else if (strcmp(argv[i], "--appstream-releases") == 0 && i + 2 < argc) {
            state.about_gresource = argv[i + 1];
            state.about_resource_path = argv[i + 2];
            state.appstream_releases = true;
            i += 2;
        } else if (strcmp(argv[i], "--present") == 0) {
            state.about_present = true;
        } else {
            fprintf(
                stderr,
                "Usage: %s [--about-gresource <file.gresource> <res_path> --present | --appstream-releases <file.gresource> <res_path>]\n",
                argv[0]
            );
            return 2;
        }
    }

    if (state.appstream_releases) {
        return run_appstream_releases(state.about_gresource, state.about_resource_path);
    }

    GResource *resource = NULL;
    if (state.about_gresource != NULL) {
        GError *error = NULL;
        resource = g_resource_load(state.about_gresource, &error);
        if (resource == NULL) {
            fprintf(stderr, "GTK4-ABOUT: failed to load gresource %s: %s\n",
                state.about_gresource, error != NULL ? error->message : "unknown");
            g_clear_error(&error);
            return 1;
        }
        g_resources_register(resource);
    }

    AdwApplication *app = adw_application_new("org.ferrionos.Gtk4WindowProbe", G_APPLICATION_NON_UNIQUE);
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), &state);
    int app_argc = 1;
    char *app_argv[2] = { argv[0], NULL };
    int status = g_application_run(G_APPLICATION(app), app_argc, app_argv);
    g_object_unref(app);

    if (state.css != NULL) {
        g_object_unref(state.css);
    }
    if (resource != NULL) {
        g_resources_unregister(resource);
        g_resource_unref(resource);
    }

    if (status == 0 && state.exit_code == 0) {
        printf("GTK4-WINDOW: closed\n");
        fflush(stdout);
    }
    return status != 0 ? status : state.exit_code;
}
