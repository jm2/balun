//! Main-context owner of the loaded settings document and its saves.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use balun::settings::{Settings, SettingsStore, WindowState};

/// Loads the settings once and writes back only when the document was
/// readable, so a malformed or newer file is never overwritten.
pub(crate) struct SettingsSession {
    store: Option<SettingsStore>,
    settings: RefCell<Settings>,
    writable: Cell<bool>,
}

impl SettingsSession {
    /// Load from `store`, or run with defaults and no persistence when the
    /// platform names no directory or the existing file cannot be read.
    pub(crate) fn open(store: Option<SettingsStore>) -> Self {
        let (settings, writable) = match store.as_ref().map(SettingsStore::load) {
            None => (Settings::default(), false),
            Some(Ok(Some(settings))) => (settings, true),
            Some(Ok(None)) => (Settings::default(), true),
            Some(Err(error)) => {
                eprintln!("Balun settings were not loaded and will not be overwritten: {error}");
                (Settings::default(), false)
            }
        };
        Self {
            store,
            settings: RefCell::new(settings),
            writable: Cell::new(writable),
        }
    }

    /// Window geometry to apply before the window is shown.
    pub(crate) fn window(&self) -> WindowState {
        self.settings.borrow().window()
    }

    /// Record the window's current geometry. A fullscreen window is skipped
    /// because that size is transient and GTK restores the prior size itself.
    pub(crate) fn persist_window(&self, window: &adw::ApplicationWindow) {
        if window.is_fullscreen() {
            return;
        }
        let (width, height) = window.default_size();
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return;
        };
        let Ok(state) = WindowState::new(width, height, window.is_maximized()) else {
            return;
        };
        self.update(|settings| settings.set_window(state));
    }

    /// Apply a change and save when it altered the document.
    fn update(&self, change: impl FnOnce(&mut Settings) -> bool) {
        let changed = change(&mut self.settings.borrow_mut());
        if !changed || !self.writable.get() {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        if let Err(error) = store.save(&self.settings.borrow()) {
            eprintln!("Balun settings could not be saved: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use balun::settings::SETTINGS_FILE_NAME;
    use tempfile::TempDir;

    use super::*;

    fn store() -> (TempDir, SettingsStore) {
        let directory = tempfile::tempdir().expect("test directory");
        let store = SettingsStore::new(directory.path().join("balun"));
        (directory, store)
    }

    fn resized() -> WindowState {
        WindowState::new(1_500, 850, true).expect("valid window")
    }

    #[test]
    fn no_store_runs_with_defaults_and_never_writes() {
        let session = SettingsSession::open(None);

        assert_eq!(session.window(), WindowState::default());
        session.update(|settings| settings.set_window(resized()));
        assert_eq!(session.window(), resized(), "memory still updates");
    }

    #[test]
    fn empty_directory_is_writable_and_persists_changes() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));

        assert_eq!(session.window(), WindowState::default());
        session.update(|settings| settings.set_window(resized()));

        let reloaded = store.load().expect("load").expect("document");
        assert_eq!(reloaded.window(), resized());
    }

    #[test]
    fn unchanged_updates_do_not_write() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));

        session.update(|settings| settings.set_window(WindowState::default()));

        assert!(!store.directory().join(SETTINGS_FILE_NAME).exists());
    }

    #[test]
    fn unreadable_document_is_preserved_and_not_overwritten() {
        let (_directory, store) = store();
        fs::create_dir_all(store.directory()).expect("create directory");
        let raw = b"{\"schema_version\":99}\n";
        fs::write(store.directory().join(SETTINGS_FILE_NAME), raw).expect("write raw");
        let session = SettingsSession::open(Some(store.clone()));

        assert_eq!(session.window(), WindowState::default());
        session.update(|settings| settings.set_window(resized()));

        assert_eq!(
            fs::read(store.directory().join(SETTINGS_FILE_NAME)).expect("read raw"),
            raw
        );
    }
}
