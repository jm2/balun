//! Register app icons after GTK opens its display, including uninstalled builds.

use std::path::Path;

use crate::app::APPLICATION_ID;

pub(crate) fn initialize() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    if let Ok(exe) = std::env::current_exe() {
        add_search_paths(&theme, &exe);
    }
    gtk::Window::set_default_icon_name(APPLICATION_ID);
}

fn add_search_paths(theme: &gtk::IconTheme, exe: &Path) {
    // A relocated macOS bundle must use its own theme and app icons.
    if let Some(contents) = exe.parent().and_then(Path::parent)
        && contents.file_name().is_some_and(|name| name == "Contents")
    {
        theme.add_search_path(contents.join("Resources/share/icons"));
        return;
    }

    // Tributary's development and Windows layouts: target[/<triple>]/<profile>,
    // or an installed binary beside (or in bin/ below) share/icons.
    for directory in exe.ancestors().skip(1).take(4) {
        for relative in ["data/icons", "share/icons"] {
            let icons = directory.join(relative);
            if icons.is_dir() {
                theme.add_search_path(icons);
            }
        }
    }
    // Homebrew's GTK and icon-theme prefixes need not be the same. Capture
    // the installed theme at build time without needing pkg-config at launch.
    #[cfg(target_os = "macos")]
    if let Some(icons) = option_env!("BALUN_BUILD_ICON_DIR") {
        theme.add_search_path(icons);
    }
}
