//! Balun desktop application entry point.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod ui;

fn main() -> gtk::glib::ExitCode {
    app::run()
}
