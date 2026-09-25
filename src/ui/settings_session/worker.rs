//! One owned worker, one queued snapshot, and bounded main-context waits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use balun::discovery::TypedSubnetScope;
use balun::settings::{Settings, SettingsError, SettingsStore};
use tokio::sync::{oneshot, watch};

use super::PendingSave;

pub(super) const IO_WAIT: Duration = Duration::from_secs(2);

pub(super) trait Backend: Send + 'static {
    fn load(&self) -> Result<Option<Settings>, SettingsError>;
    fn save(&self, settings: &Settings, cancelled: &AtomicBool) -> Result<(), SettingsError>;
    /// The remembered subnet. A failure only leaves it unset: it is a
    /// convenience, and an unreadable file is never repaired.
    fn load_subnet_prefix(&self) -> Result<Option<TypedSubnetScope>, SettingsError>;
    fn save_subnet_prefix(
        &self,
        prefix: Option<TypedSubnetScope>,
        cancelled: &AtomicBool,
    ) -> Result<(), SettingsError>;
}

impl Backend for SettingsStore {
    fn load(&self) -> Result<Option<Settings>, SettingsError> {
        self.load()
    }

    fn save(&self, settings: &Settings, cancelled: &AtomicBool) -> Result<(), SettingsError> {
        self.save_unless_cancelled(settings, cancelled)
    }

    fn load_subnet_prefix(&self) -> Result<Option<TypedSubnetScope>, SettingsError> {
        self.load_subnet_prefix()
    }

    fn save_subnet_prefix(
        &self,
        prefix: Option<TypedSubnetScope>,
        cancelled: &AtomicBool,
    ) -> Result<(), SettingsError> {
        self.save_subnet_prefix_unless_cancelled(prefix, cancelled)
    }
}

struct Shared {
    queued: Mutex<Option<PendingSave>>,
    wake: Condvar,
    cancelled: AtomicBool,
    failed: AtomicBool,
    idle: watch::Sender<bool>,
    /// Becomes true once a save or the worker fails after a successful load.
    failure: watch::Sender<bool>,
}

/// The settings document, if one exists, and the remembered subnet.
type LoadResult = Result<(Option<Settings>, Option<TypedSubnetScope>), SettingsError>;
type LoadReceiver = oneshot::Receiver<LoadResult>;

pub(super) struct SettingsWriter {
    shared: Arc<Shared>,
}

impl SettingsWriter {
    pub(super) fn start(backend: impl Backend) -> Option<(Self, LoadReceiver)> {
        let (loaded, receiver) = oneshot::channel();
        let (idle, _) = watch::channel(false);
        let (failure, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            queued: Mutex::new(None),
            wake: Condvar::new(),
            cancelled: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            idle,
            failure,
        });
        let worker = Arc::clone(&shared);
        // Dropping the join handle detaches this single worker. Shutdown never
        // joins arbitrary OS I/O, and a timed-out session never replaces it.
        if std::thread::Builder::new()
            .name("balun-settings".into())
            .spawn(move || {
                worker.finish(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || {
                        worker.run(backend, loaded);
                    },
                )));
            })
            .is_err()
        {
            eprintln!("Balun settings worker could not start; persistence is disabled");
            return None;
        }
        Some((Self { shared }, receiver))
    }

    pub(super) fn writable(&self) -> bool {
        !self.shared.cancelled.load(Ordering::Acquire)
            && !self.shared.failed.load(Ordering::Acquire)
    }

    /// Observe a later save or worker failure. The sender closes, without
    /// reporting one, when the worker and this writer are both gone.
    pub(super) fn failure(&self) -> watch::Receiver<bool> {
        self.shared.failure.subscribe()
    }

    pub(super) fn save(&self, save: PendingSave) {
        let mut queued = self.shared.queued.lock().unwrap_or_else(|e| e.into_inner());
        if self.writable() {
            let save = match queued.take() {
                Some(earlier) => save.after(earlier),
                None => save,
            };
            *queued = Some(save);
            self.shared.idle.send_replace(false);
            self.shared.wake.notify_one();
        }
    }

    pub(super) async fn drain(&self, limit: Duration) -> bool {
        let mut idle = self.shared.idle.subscribe();
        tokio::select! {
            biased;
            result = idle.wait_for(|idle| *idle) => result.is_ok(),
            () = gtk::glib::timeout_future(limit) => false,
        }
    }

    pub(super) fn stop(&self) {
        let mut queued = self.shared.queued.lock().unwrap_or_else(|e| e.into_inner());
        self.shared.cancelled.store(true, Ordering::Release);
        *queued = None;
        self.shared.wake.notify_one();
    }
}

impl Drop for SettingsWriter {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    /// Disable persistence after a worker panic. Kept outside the generic
    /// spawn closure so every backend's panic reaches the same code.
    fn finish(&self, result: std::thread::Result<()>) {
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
            self.cancelled.store(true, Ordering::Release);
            self.idle.send_replace(true);
            self.failure.send_replace(true);
            eprintln!("Balun settings worker failed; persistence is disabled");
        }
    }

    fn run(&self, backend: impl Backend, loaded: oneshot::Sender<LoadResult>) {
        let result = backend
            .load()
            .map(|settings| (settings, backend.load_subnet_prefix().ok().flatten()));
        let usable = result.is_ok();
        self.failed.store(!usable, Ordering::Release);
        self.idle.send_replace(true);
        if self.cancelled.load(Ordering::Acquire) || loaded.send(result).is_err() || !usable {
            return;
        }
        loop {
            let next = {
                let mut queued = self.queued.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if self.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(next) = queued.take() {
                        break next;
                    }
                    self.idle.send_replace(true);
                    queued = self.wake.wait(queued).unwrap_or_else(|e| e.into_inner());
                }
            };
            let saved = next
                .settings
                .as_ref()
                .map_or(Ok(()), |settings| backend.save(settings, &self.cancelled))
                .and_then(|()| {
                    next.subnet_prefix.map_or(Ok(()), |prefix| {
                        backend.save_subnet_prefix(prefix, &self.cancelled)
                    })
                });
            if let Err(error) = saved {
                let mut queued = self.queued.lock().unwrap_or_else(|e| e.into_inner());
                self.failed.store(true, Ordering::Release);
                *queued = None;
                self.idle.send_replace(true);
                self.failure.send_replace(true);
                eprintln!("Balun settings could not be saved; persistence is disabled: {error}");
                return;
            }
        }
    }
}
