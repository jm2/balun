//! One owned worker, one queued snapshot, and bounded main-context waits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use balun::settings::{Settings, SettingsError, SettingsStore};
use tokio::sync::{oneshot, watch};

use super::PendingSave;

pub(super) const IO_WAIT: Duration = Duration::from_secs(2);

pub(super) trait Backend: Send + 'static {
    fn load(&self) -> Result<Option<Settings>, SettingsError>;
    fn save(&self, settings: &Settings, cancelled: &AtomicBool) -> Result<(), SettingsError>;
}

impl Backend for SettingsStore {
    fn load(&self) -> Result<Option<Settings>, SettingsError> {
        self.load()
    }

    fn save(&self, settings: &Settings, cancelled: &AtomicBool) -> Result<(), SettingsError> {
        self.save_unless_cancelled(settings, cancelled)
    }
}

struct Shared {
    queued: Mutex<Option<PendingSave>>,
    wake: Condvar,
    cancelled: AtomicBool,
    failed: AtomicBool,
    idle: watch::Sender<bool>,
}

type LoadResult = Result<Option<Settings>, SettingsError>;
type LoadReceiver = oneshot::Receiver<LoadResult>;

pub(super) struct SettingsWriter {
    shared: Arc<Shared>,
}

impl SettingsWriter {
    pub(super) fn start(backend: impl Backend) -> Option<(Self, LoadReceiver)> {
        let (loaded, receiver) = oneshot::channel();
        let (idle, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            queued: Mutex::new(None),
            wake: Condvar::new(),
            cancelled: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            idle,
        });
        let worker = Arc::clone(&shared);
        // Dropping the join handle detaches this single worker. Shutdown never
        // joins arbitrary OS I/O, and a timed-out session never replaces it.
        if std::thread::Builder::new()
            .name("balun-settings".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker.run(backend, loaded);
                }));
                if result.is_err() {
                    worker.failed.store(true, Ordering::Release);
                    worker.cancelled.store(true, Ordering::Release);
                    worker.idle.send_replace(true);
                    eprintln!("Balun settings worker failed; persistence is disabled");
                }
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

    pub(super) fn save(&self, save: PendingSave) {
        let mut queued = self.shared.queued.lock().unwrap_or_else(|e| e.into_inner());
        if self.writable() {
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
    fn run(&self, backend: impl Backend, loaded: oneshot::Sender<LoadResult>) {
        let result = backend.load();
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
            if let Err(error) = backend.save(&next.settings, &self.cancelled) {
                let mut queued = self.queued.lock().unwrap_or_else(|e| e.into_inner());
                self.failed.store(true, Ordering::Release);
                *queued = None;
                self.idle.send_replace(true);
                eprintln!("Balun settings could not be saved; persistence is disabled: {error}");
                return;
            }
        }
    }
}
