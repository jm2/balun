//! Main-context owner of the loaded settings document and its saves.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::time::Duration;

use adw::prelude::*;
use balun::discovery::TypedSubnetScope;
use balun::settings::{RememberedTarget, Settings, SettingsStore, WindowState};

mod worker;
use worker::{Backend, IO_WAIT, SettingsWriter};

/// Main-context preferences with a single worker for private-profile I/O.
pub(crate) struct SettingsSession {
    settings: RefCell<Settings>,
    subnet_prefix: Cell<Option<TypedSubnetScope>>,
    writer: Option<SettingsWriter>,
    notice_taken: Cell<bool>,
}

/// A snapshot owns no I/O handles and writes nothing until queued. It holds
/// the whole settings document, the remembered subnet, or both; `None` leaves
/// that file alone.
#[must_use = "a staged save writes nothing until it is queued"]
pub(crate) struct PendingSave {
    settings: Option<Settings>,
    subnet_prefix: Option<Option<TypedSubnetScope>>,
}

impl PendingSave {
    /// This snapshot queued after `earlier`: each file keeps its newest
    /// staged content, so a queued subnet change survives a later settings
    /// save and the other way round.
    fn after(self, earlier: Self) -> Self {
        Self {
            settings: self.settings.or(earlier.settings),
            subnet_prefix: self.subnet_prefix.or(earlier.subnet_prefix),
        }
    }
}

impl SettingsSession {
    /// Wait at most two seconds for loading, then use defaults without writes.
    pub(crate) async fn open_async(store: Option<SettingsStore>) -> Self {
        Self::open_backend(store, IO_WAIT).await
    }

    #[cfg(test)]
    pub(crate) fn open(store: Option<SettingsStore>) -> Self {
        if store.is_none() {
            return Self {
                settings: RefCell::new(Settings::default()),
                subnet_prefix: Cell::new(None),
                writer: None,
                notice_taken: Cell::new(false),
            };
        }
        gtk::glib::MainContext::new().block_on(Self::open_async(store))
    }

    async fn open_backend(backend: Option<impl Backend>, limit: Duration) -> Self {
        let mut session = Self {
            settings: RefCell::new(Settings::default()),
            subnet_prefix: Cell::new(None),
            writer: None,
            notice_taken: Cell::new(false),
        };
        let Some((writer, loaded)) = backend.and_then(SettingsWriter::start) else {
            return session;
        };
        let result = tokio::select! {
            biased;
            result = loaded => Some(result),
            () = gtk::glib::timeout_future(limit) => None,
        };
        match result {
            Some(Ok(Ok((settings, subnet_prefix)))) => {
                *session.settings.borrow_mut() = settings.unwrap_or_default();
                session.subnet_prefix.set(subnet_prefix);
                session.writer = Some(writer);
            }
            Some(Ok(Err(error))) => {
                eprintln!("Balun settings were not loaded and will not be overwritten: {error}");
            }
            Some(Err(_)) => {
                eprintln!("Balun settings worker stopped; persistence is disabled");
            }
            None => {
                eprintln!("Balun settings load timed out; persistence is disabled");
            }
        }
        session
    }

    /// Wait for persistence to become unavailable: at once when no store,
    /// load, or worker was admitted, or later when a save fails. Resolves
    /// `false` if the session ends normally. Only the first call gets a
    /// waiter, so the window shows at most one notice per session.
    pub(crate) fn take_unavailable_notice(&self) -> Option<impl Future<Output = bool> + 'static> {
        if self.notice_taken.replace(true) {
            return None;
        }
        let failure = self.writer.as_ref().map(SettingsWriter::failure);
        Some(async move {
            match failure {
                Some(mut failure) => failure.wait_for(|failed| *failed).await.is_ok(),
                None => true,
            }
        })
    }

    /// Keep at most one in-flight write and the newest queued snapshot.
    pub(crate) fn save(&self, save: PendingSave) {
        if let Some(writer) = &self.writer {
            writer.save(save);
        }
    }

    #[cfg(test)]
    pub(crate) async fn drain(&self) {
        if let Some(writer) = &self.writer {
            assert!(writer.drain(IO_WAIT).await, "settings test drain timed out");
        }
    }

    /// Bound close-time waiting, discard pending work, and stop accepting saves.
    pub(crate) async fn close(&self) {
        self.close_with_limit(IO_WAIT).await;
    }

    async fn close_with_limit(&self, limit: Duration) {
        if let Some(writer) = &self.writer {
            if !writer.drain(limit).await {
                eprintln!("Balun settings save timed out; the newest preferences may be unsaved");
            }
            writer.stop();
        }
    }

    /// Exact-address targets remembered from earlier launches, oldest first.
    pub(crate) fn remembered_targets(&self) -> Vec<RememberedTarget> {
        self.settings.borrow().remembered_targets().to_vec()
    }

    /// Remember a target whose probe received a valid device reply and stage
    /// the save; the caller runs the write off the main context.
    pub(crate) fn remember_target(&self, target: RememberedTarget) -> Option<PendingSave> {
        self.stage(|settings| settings.remember_target(target))
    }

    /// Forget a remembered target and stage the save; `None` when it was not
    /// remembered or the store is read-only.
    pub(crate) fn forget_target(&self, target: &RememberedTarget) -> Option<PendingSave> {
        self.stage(|settings| settings.forget_target(target))
    }

    /// The subnet last entered for subnet search, offered again as editable
    /// text. It never authorizes a search.
    pub(crate) fn subnet_prefix(&self) -> Option<TypedSubnetScope> {
        self.subnet_prefix.get()
    }

    /// Remember the entered subnet, or with `None` forget it, and stage the
    /// save of its own file; `None` when nothing changed or the store is
    /// read-only.
    pub(crate) fn set_subnet_prefix(
        &self,
        prefix: Option<TypedSubnetScope>,
    ) -> Option<PendingSave> {
        if self.subnet_prefix.replace(prefix) == prefix || !self.writable() {
            return None;
        }
        Some(PendingSave {
            settings: None,
            subnet_prefix: Some(prefix),
        })
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
        if !changed || !self.writable() {
            return None;
        }
        Some(PendingSave {
            settings: Some(self.settings.borrow().clone()),
            subnet_prefix: None,
        })
    }

    fn writable(&self) -> bool {
        self.writer.as_ref().is_some_and(SettingsWriter::writable)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use balun::settings::{SETTINGS_FILE_NAME, SettingsError};
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    use super::*;

    fn store() -> (TempDir, SettingsStore) {
        let directory = tempfile::tempdir().expect("test directory");
        let store = SettingsStore::new(directory.path().join("balun"));
        (directory, store)
    }

    fn resized() -> WindowState {
        WindowState::new(1_500, 850, true).expect("valid window")
    }

    fn sized(width: u32, height: u32) -> WindowState {
        WindowState::new(width, height, false).expect("valid window")
    }

    fn stored_window(store: &SettingsStore) -> WindowState {
        store
            .load()
            .expect("readable document")
            .expect("written document")
            .window()
    }

    #[test]
    fn saves_drain_in_order_and_restart_after_idle() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));
        gtk::glib::MainContext::new().block_on(async {
            session.drain().await;
            for state in [sized(1_000, 700), sized(1_100, 720), resized()] {
                session.save(
                    session
                        .stage(|settings| settings.set_window(state))
                        .expect("save"),
                );
            }
            session.drain().await;
            assert_eq!(stored_window(&store), resized());
            session.save(
                session
                    .stage(|settings| settings.set_window(sized(900, 600)))
                    .expect("save"),
            );
            session.drain().await;
            assert_eq!(stored_window(&store), sized(900, 600));
            session.close().await;
            assert!(
                session
                    .stage(|settings| settings.set_window(resized()))
                    .is_none()
            );
        });
    }

    fn write(session: &SettingsSession, staged: PendingSave) {
        session.save(staged);
        gtk::glib::MainContext::new().block_on(session.drain());
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
        write(&session, staged);

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
        write(&session, staged);

        let reloaded = store.load().expect("load").expect("document");
        assert_eq!(reloaded.window(), resized());
    }

    #[test]
    fn remembered_targets_round_trip_through_the_store() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));
        let target = RememberedTarget::Address(
            balun::discovery::ExactDiscoveryTarget::parse("192.0.2.9").expect("valid target"),
        );
        assert!(session.remembered_targets().is_empty());

        write(
            &session,
            session
                .remember_target(target.clone())
                .expect("a new target stages a save"),
        );

        assert_eq!(session.remembered_targets(), vec![target.clone()]);
        assert!(
            session.remember_target(target.clone()).is_none(),
            "repeating the newest target stages nothing"
        );
        let reloaded = SettingsSession::open(Some(store));
        assert_eq!(reloaded.remembered_targets(), vec![target]);
    }

    #[test]
    fn the_entered_subnet_round_trips_and_forgetting_it_saves() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store.clone()));
        let prefix: TypedSubnetScope = "192.168.2.0/23".parse().expect("valid subnet");
        assert_eq!(session.subnet_prefix(), None);
        assert!(
            session.set_subnet_prefix(None).is_none(),
            "nothing to forget"
        );

        write(
            &session,
            session
                .set_subnet_prefix(Some(prefix))
                .expect("a new subnet stages a save"),
        );
        assert!(session.set_subnet_prefix(Some(prefix)).is_none());
        assert_eq!(
            SettingsSession::open(Some(store.clone())).subnet_prefix(),
            Some(prefix)
        );

        write(
            &session,
            session
                .set_subnet_prefix(None)
                .expect("forgetting stages a save"),
        );
        assert_eq!(SettingsSession::open(Some(store)).subnet_prefix(), None);
        // Without a store the subnet is kept for the session only.
        let memory = SettingsSession::open(None);
        assert!(memory.set_subnet_prefix(Some(prefix)).is_none());
        assert_eq!(memory.subnet_prefix(), Some(prefix));
    }

    /// A subnet change queued behind a stalled save and a later settings
    /// change both reach their files: queued snapshots merge per file.
    #[test]
    fn queued_subnet_and_settings_changes_merge_behind_a_stalled_save() {
        gtk::glib::MainContext::new().block_on(async {
            let (_directory, store) = store();
            let (backend, _done) = stalled_backend(store.clone());
            let (gate, entered, release) = gate();
            *backend.save_gate.lock().unwrap() = Some(gate);
            let session = SettingsSession::open_backend(Some(backend), IO_WAIT).await;
            let prefix: TypedSubnetScope = "10.0.0.0/24".parse().expect("valid subnet");

            let first = session
                .stage(|settings| settings.set_window(sized(900, 600)))
                .expect("stage the first window");
            session.save(first);
            entered.await.expect("the first save is in flight");
            let remembered = session
                .set_subnet_prefix(Some(prefix))
                .expect("stage the subnet");
            session.save(remembered);
            let later = session
                .stage(|settings| settings.set_window(resized()))
                .expect("stage a later window");
            session.save(later);
            release.send(()).expect("release the stalled save");
            session.drain().await;

            assert_eq!(stored_window(&store), resized());
            assert_eq!(store.load_subnet_prefix(), Ok(Some(prefix)));
            assert_eq!(
                SettingsSession::open_async(Some(store))
                    .await
                    .subnet_prefix(),
                Some(prefix)
            );
        });
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

    struct Gate {
        entered: oneshot::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Gate {
        fn wait(self) {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
        }
    }

    fn gate() -> (Gate, oneshot::Receiver<()>, mpsc::Sender<()>) {
        let (entered, observed) = oneshot::channel();
        let (release, blocked) = mpsc::channel();
        (
            Gate {
                entered,
                release: blocked,
            },
            observed,
            release,
        )
    }

    struct StalledBackend {
        store: SettingsStore,
        load_gate: Mutex<Option<Gate>>,
        save_gate: Mutex<Option<Gate>>,
        loads: Arc<AtomicUsize>,
        writes: Arc<Mutex<Vec<WindowState>>>,
        done: Option<oneshot::Sender<()>>,
    }

    impl Backend for StalledBackend {
        fn load(&self) -> Result<Option<Settings>, SettingsError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.load_gate.lock().unwrap().take() {
                gate.wait();
            }
            self.store.load()
        }

        fn save(&self, settings: &Settings, cancelled: &AtomicBool) -> Result<(), SettingsError> {
            self.writes.lock().unwrap().push(settings.window());
            if let Some(gate) = self.save_gate.lock().unwrap().take() {
                gate.wait();
            }
            self.store.save_unless_cancelled(settings, cancelled)
        }

        fn load_subnet_prefix(&self) -> Result<Option<TypedSubnetScope>, SettingsError> {
            self.store.load_subnet_prefix()
        }

        fn save_subnet_prefix(
            &self,
            prefix: Option<TypedSubnetScope>,
            cancelled: &AtomicBool,
        ) -> Result<(), SettingsError> {
            if let Some(gate) = self.save_gate.lock().unwrap().take() {
                gate.wait();
            }
            self.store
                .save_subnet_prefix_unless_cancelled(prefix, cancelled)
        }
    }

    impl Drop for StalledBackend {
        fn drop(&mut self) {
            let _ = self.done.take().unwrap().send(());
        }
    }

    fn stalled_backend(store: SettingsStore) -> (StalledBackend, oneshot::Receiver<()>) {
        let (done, exited) = oneshot::channel();
        (
            StalledBackend {
                store,
                load_gate: Mutex::new(None),
                save_gate: Mutex::new(None),
                loads: Arc::new(AtomicUsize::new(0)),
                writes: Arc::new(Mutex::new(Vec::new())),
                done: Some(done),
            },
            exited,
        )
    }

    #[test]
    fn stalled_load_keeps_main_context_running_and_discards_late_result() {
        let (_directory, store) = store();
        let mut saved = Settings::default();
        saved.set_window(resized());
        store.save(&saved).unwrap();
        let (backend, exited) = stalled_backend(store.clone());
        let loads = Arc::clone(&backend.loads);
        let (block, entered, release) = gate();
        *backend.load_gate.lock().unwrap() = Some(block);
        gtk::glib::MainContext::new().block_on(async {
            let (session, ()) = tokio::join!(
                SettingsSession::open_backend(Some(backend), Duration::from_millis(30)),
                async {
                    entered.await.unwrap();
                    // This callback must run while the filesystem worker is
                    // still blocked; an unbounded startup cannot finish.
                    gtk::glib::timeout_future(Duration::from_millis(5)).await;
                }
            );
            assert_eq!(session.window(), WindowState::default());
            assert!(
                session
                    .stage(|settings| settings.set_window(sized(900, 600)))
                    .is_none()
            );
            release.send(()).unwrap();
            exited.await.unwrap();
            assert_eq!(loads.load(Ordering::SeqCst), 1);
            assert_eq!(
                session.window(),
                sized(900, 600),
                "late load must not replace memory"
            );
            assert_eq!(
                stored_window(&store),
                resized(),
                "no persistence after a load timeout"
            );
        });
    }

    #[test]
    fn stalled_writer_keeps_only_latest_snapshot_on_one_worker() {
        let (_directory, store) = store();
        let (backend, exited) = stalled_backend(store.clone());
        let writes = Arc::clone(&backend.writes);
        let loads = Arc::clone(&backend.loads);
        let (block, entered, release) = gate();
        *backend.save_gate.lock().unwrap() = Some(block);
        gtk::glib::MainContext::new().block_on(async {
            let session = SettingsSession::open_backend(Some(backend), IO_WAIT).await;
            session.save(session.stage(|s| s.set_window(sized(900, 600))).unwrap());
            entered.await.unwrap();
            for width in 1_000..1_100 {
                session.save(session.stage(|s| s.set_window(sized(width, 700))).unwrap());
            }
            assert_eq!(
                writes.lock().unwrap().len(),
                1,
                "no extra writer while blocked"
            );
            release.send(()).unwrap();
            session.drain().await;
            assert_eq!(
                *writes.lock().unwrap(),
                vec![sized(900, 600), sized(1_099, 700)]
            );
            assert_eq!(stored_window(&store), sized(1_099, 700));
            session.close().await;
            exited.await.unwrap();
            assert_eq!(loads.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn close_deadline_cancels_stalled_save_and_drops_queued_snapshots() {
        let (_directory, store) = store();
        store.save(&Settings::default()).unwrap();
        let before = fs::read(store.directory().join(SETTINGS_FILE_NAME)).unwrap();
        let (backend, exited) = stalled_backend(store.clone());
        let writes = Arc::clone(&backend.writes);
        let (block, entered, release) = gate();
        *backend.save_gate.lock().unwrap() = Some(block);
        gtk::glib::MainContext::new().block_on(async {
            let session = SettingsSession::open_backend(Some(backend), IO_WAIT).await;
            session.save(session.stage(|s| s.set_window(resized())).unwrap());
            entered.await.unwrap();
            session.save(session.stage(|s| s.set_window(sized(900, 600))).unwrap());
            session.close_with_limit(Duration::from_millis(30)).await;
            assert!(session.stage(|s| s.set_window(sized(950, 600))).is_none());
            assert_eq!(writes.lock().unwrap().len(), 1);
            release.send(()).unwrap();
            exited.await.unwrap();
            assert_eq!(
                writes.lock().unwrap().len(),
                1,
                "queued snapshot was discarded"
            );
            assert_eq!(
                fs::read(store.directory().join(SETTINGS_FILE_NAME)).unwrap(),
                before
            );
        });
    }

    #[test]
    fn failed_or_missing_load_offers_one_unavailable_notice() {
        let (_directory, store) = store();
        fs::create_dir_all(store.directory()).expect("create directory");
        fs::write(
            store.directory().join(SETTINGS_FILE_NAME),
            b"{\"schema_version\":99}",
        )
        .expect("write raw");
        for session in [
            SettingsSession::open(Some(store)),
            SettingsSession::open(None),
        ] {
            let notice = session.take_unavailable_notice().expect("first offer");
            assert!(
                session.take_unavailable_notice().is_none(),
                "one per session"
            );
            assert!(gtk::glib::MainContext::new().block_on(notice));
        }
    }

    #[test]
    fn a_normal_session_end_offers_no_notice() {
        let (_directory, store) = store();
        let session = SettingsSession::open(Some(store));
        let notice = session.take_unavailable_notice().expect("first offer");
        gtk::glib::MainContext::new().block_on(async move {
            session.save(session.stage(|s| s.set_window(resized())).unwrap());
            session.drain().await;
            session.close().await;
            drop(session);
            let ended = tokio::select! {
                biased;
                offered = notice => Some(offered),
                () = gtk::glib::timeout_future(Duration::from_secs(5)) => None,
            };
            assert_eq!(ended, Some(false), "the closed worker reports no failure");
        });
    }

    struct PanickingBackend;

    impl Backend for PanickingBackend {
        fn load(&self) -> Result<Option<Settings>, SettingsError> {
            Ok(None)
        }

        fn save(&self, _: &Settings, _: &AtomicBool) -> Result<(), SettingsError> {
            panic!("injected settings worker panic");
        }

        fn load_subnet_prefix(&self) -> Result<Option<TypedSubnetScope>, SettingsError> {
            Err(SettingsError::Busy)
        }

        fn save_subnet_prefix(
            &self,
            _: Option<TypedSubnetScope>,
            _: &AtomicBool,
        ) -> Result<(), SettingsError> {
            panic!("injected settings worker panic");
        }
    }

    #[test]
    fn a_panicking_worker_disables_persistence_and_offers_the_notice() {
        gtk::glib::MainContext::new().block_on(async {
            let session = SettingsSession::open_backend(Some(PanickingBackend), IO_WAIT).await;
            let notice = session.take_unavailable_notice().expect("first offer");
            let first = session.stage(|s| s.set_window(resized())).expect("loaded");
            let late = session
                .stage(|s| s.set_window(sized(900, 600)))
                .expect("writable");
            session.save(first);
            assert!(notice.await, "a worker panic offers the notice");
            // A snapshot staged before the failure is discarded, not queued.
            session.save(late);
            assert!(session.stage(|s| s.set_window(sized(950, 600))).is_none());
        });
    }

    #[test]
    fn failed_save_disables_future_persistence_and_preserves_newer_schema() {
        let (_directory, store) = store();
        store.save(&Settings::default()).unwrap();
        gtk::glib::MainContext::new().block_on(async {
            let session = SettingsSession::open_async(Some(store.clone())).await;
            let notice = session.take_unavailable_notice().expect("first offer");
            let sentinel = b"{\"schema_version\":99}";
            fs::write(store.directory().join(SETTINGS_FILE_NAME), sentinel).unwrap();
            session.save(session.stage(|s| s.set_window(resized())).unwrap());
            session.drain().await;
            assert!(notice.await, "a failed save offers the notice");
            assert!(session.take_unavailable_notice().is_none());
            assert!(session.stage(|s| s.set_window(sized(900, 600))).is_none());
            assert_eq!(
                fs::read(store.directory().join(SETTINGS_FILE_NAME)).unwrap(),
                sentinel
            );
        });
    }
}
