//! Balun desktop application entry point.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod ui;

fn main() -> gtk::glib::ExitCode {
    match balun::playback::emit_macos_install_key_if_requested() {
        Ok(true) => return gtk::glib::ExitCode::SUCCESS,
        Ok(false) => {}
        Err(error) => {
            eprintln!("Balun could not prepare the platform runtime: {error}");
            return gtk::glib::ExitCode::FAILURE;
        }
    }
    balun::logging::init();
    app::run()
}
