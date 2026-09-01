//! GTK application lifecycle.

use adw::prelude::*;

use crate::ui;

/// Reverse-DNS application identifier shared by Balun desktop integrations.
pub(crate) const APPLICATION_ID: &str = "io.github.jm2.Balun";

/// Start the desktop application and run the GLib main loop.
pub(crate) fn run() -> gtk::glib::ExitCode {
    gtk::glib::set_prgname(Some("Balun"));
    gtk::glib::set_application_name("Balun");

    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();

    application.connect_activate(|application| {
        if let Some(window) = application
            .active_window()
            .or_else(|| application.windows().into_iter().next())
        {
            window.present();
            return;
        }

        ui::window::build(application).present();
    });

    application.run()
}
