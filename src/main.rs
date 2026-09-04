//! Balun desktop application entry point.

#![cfg_attr(
    all(not(debug_assertions), not(feature = "windows-console")),
    windows_subsystem = "windows"
)]

mod app;
mod ui;

fn main() -> gtk::glib::ExitCode {
    balun::logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Balun starting");
    app::run()
}
