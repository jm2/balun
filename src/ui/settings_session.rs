//! Main-context owner of the loaded settings document and its saves.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use balun::discovery::ExactDiscoveryTarget;
use balun::settings::{Settings, SettingsStore, WindowState};

/// Loads the settings once and writes back only when the document was
/// readable, so a malformed or newer file is never overwritten.
pub(crate) struct SettingsSession {
    store: Option<SettingsStore>,
    settings: RefCell<Settings>,
    writable: Cell<bool>,
}

/// One staged write of the whole document. It owns copies of the store and
/// the settings, so it can move to a blocking worker while the session stays
/// on the main context.
#[must_use = "a staged save writes nothing until it runs"]
pub(crate) struct PendingSave {
    store: SettingsStore,
    settings: Settings,
}

impl PendingSave {
    /// Write the staged document atomically. The temporary-file flushes and
    /// the rename block, so run this off the main context.
    pub(crate) fn write(self) {
        if let Err(error) = self.store.save(&self.settings) {
            eprintln!("Balun settings could not be saved: {error}");
        }
    }
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

    /// Exact-address targets remembered from earlier launches, oldest first.
    pub(crate) fn remembered_targets(&self) -> Vec<ExactDiscoveryTarget> {
        self.settings.borrow().remembered_targets().to_vec()
    }

    /// Remember a target whose probe received a valid device reply and stage
    /// the save; the caller runs the write off the main context.
    pub(crate) fn remember_target(&self, target: ExactDiscoveryTarget) -> Option<PendingSave> {
        self.stage(|settings| settings.remember_target(target))
    }

    /// Window geometry to apply before the window is shown.
    pub(crate) fn window(&self) -> WindowState {
        self.settings.borrow().window()
    }

    /// Record the window's current geometry and stage its save. A fullscreen
    /// window is skipped because that size is transient and GTK restores the
    /// prior size itself.
    pub(crate) fn stage_window(&self, window: &adw::ApplicationWindow) -> Option<PendingSave> {
        if window.is_fullscreen() {
            return None;
        }
        let (width, height) = window.default_size();
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return None;
        };
        let state = WindowState::new(width, height, window.is_maximized()).ok()?;
        self.stage(|settings| settings.set_window(state))
    }

    /// Apply a change and stage a save when it altered the document.
    fn stage(&self, change: impl FnOnce(&mut Settings) -> bool) -> Option<PendingSave> {
        let changed = change(&mut self.settings.borrow_mut());
        if !changed || !self.writable.get() {
            return None;
        }
        let store = self.store.as_ref()?.clone();
        let settings = self.settings.borrow().clone();
        Some(PendingSave { store, settings })
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
        let staged = session.stage(|settings| settings.set_window(resized()));
        assert!(staged.is_none());
        assert_eq!(session.window(), resized(), "memory still updates");
    }

    #[test]
    fn empty_directory_is_writable_and_persists_changes() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));

        assert_eq!(session.window(), WindowState::default());
        let staged = session
            .stage(|settings| settings.set_window(resized()))
            .expect("changed document stages a save");
        assert!(
            !store.directory().join(SETTINGS_FILE_NAME).exists(),
            "staging alone writes nothing"
        );
        staged.write();

        let reloaded = store.load().expect("load").expect("document");
        assert_eq!(reloaded.window(), resized());
    }

    #[test]
    fn staged_save_carries_the_document_as_staged() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));
        let staged = session
            .stage(|settings| settings.set_window(resized()))
            .expect("changed document stages a save");

        let later = WindowState::new(900, 700, false).expect("valid window");
        let _ = session.stage(|settings| settings.set_window(later));
        staged.write();

        let reloaded = store.load().expect("load").expect("document");
        assert_eq!(reloaded.window(), resized());
    }

    #[test]
    fn remembered_targets_round_trip_through_the_store() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));
        let target = ExactDiscoveryTarget::parse("192.0.2.9").expect("valid target");
        assert!(session.remembered_targets().is_empty());

        session
            .remember_target(target)
            .expect("a new target stages a save")
            .write();

        assert_eq!(session.remembered_targets(), vec![target]);
        assert!(
            session.remember_target(target).is_none(),
            "repeating the newest target stages nothing"
        );
        let reloaded = SettingsSession::open(Some(store));
        assert_eq!(reloaded.remembered_targets(), vec![target]);
    }

    #[test]
    fn unchanged_updates_do_not_write() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));

        let staged = session.stage(|settings| settings.set_window(WindowState::default()));
        assert!(staged.is_none());

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
        let staged = session.stage(|settings| settings.set_window(resized()));
        assert!(staged.is_none());

        assert_eq!(
            fs::read(store.directory().join(SETTINGS_FILE_NAME)).expect("read raw"),
            raw
        );
    }
}
