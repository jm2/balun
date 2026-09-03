//! The network-change lane's service boundary and its Linux source.
//!
//! The controller actor never touches netlink or enumerates interfaces
//! itself. A [`NetworkChangeSource`] hands it one channel of already
//! debounced [`NetworkChange`]s, produced on a dedicated thread on Linux and
//! absent everywhere else, so the actor's behaviour with no source is exactly
//! its behaviour on a platform without one.

use tokio::sync::mpsc;

use crate::discovery::NetworkChange;

/// Packet-free boundary the controller uses to learn about network changes.
///
/// Constructing an implementation must not open sockets or enumerate
/// interfaces; [`Self::subscribe`] may start a thread but must not block, and
/// it is called once, on the controller runtime. Every change it yields is
/// already coalesced over the documented quiet period and cap.
pub trait NetworkChangeSource: Send + Sync + 'static {
    /// Begin observing and return the debounced change stream, or `None` when
    /// this system cannot observe changes. A closed stream means the source
    /// stopped; the controller then behaves as if it had none.
    fn subscribe(&self) -> Option<mpsc::Receiver<NetworkChange>>;
}

/// A change source for systems that cannot observe network changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnavailableNetworkChangeSource;

impl NetworkChangeSource for UnavailableNetworkChangeSource {
    fn subscribe(&self) -> Option<mpsc::Receiver<NetworkChange>> {
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use tokio::runtime::Builder;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::NetworkChangeSource;
    use crate::discovery::{InterfaceInventory, LinuxNetworkChangeWatcher, NetworkChange};

    const WATCHER_THREAD_NAME: &str = "balun-network";
    const CHANGE_CAPACITY: usize = 4;
    /// Pause before re-establishing observation after it ends.
    const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
    /// An observation that lasted this long counts as healthy and resets the
    /// failure budget.
    const HEALTHY_OBSERVATION: Duration = Duration::from_secs(60);
    /// Consecutive short-lived attempts before the source gives up and the
    /// controller continues without one.
    const MAX_CONSECUTIVE_FAILURES: u8 = 8;

    /// The production change source: one thread owning the rtnetlink
    /// subscription, the debouncer, and the interface inventory.
    ///
    /// Constructing it does nothing. The first `subscribe` spawns the thread;
    /// dropping the source stops and joins it.
    pub struct LinuxNetworkChangeSource {
        shutdown: CancellationToken,
        thread: Mutex<Option<thread::JoinHandle<()>>>,
        subscribed: AtomicBool,
    }

    impl LinuxNetworkChangeSource {
        #[must_use]
        pub fn new() -> Self {
            Self {
                shutdown: CancellationToken::new(),
                thread: Mutex::new(None),
                subscribed: AtomicBool::new(false),
            }
        }
    }

    impl Default for LinuxNetworkChangeSource {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NetworkChangeSource for LinuxNetworkChangeSource {
        fn subscribe(&self) -> Option<mpsc::Receiver<NetworkChange>> {
            if self.subscribed.swap(true, Ordering::SeqCst) {
                return None;
            }
            let (changes, receiver) = mpsc::channel(CHANGE_CAPACITY);
            let shutdown = self.shutdown.clone();
            let thread = thread::Builder::new()
                .name(WATCHER_THREAD_NAME.to_owned())
                .spawn(move || {
                    let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                        return;
                    };
                    runtime.block_on(watch(changes, shutdown));
                })
                .ok()?;
            if let Ok(mut slot) = self.thread.lock() {
                *slot = Some(thread);
            }
            Some(receiver)
        }
    }

    impl Drop for LinuxNetworkChangeSource {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Ok(mut thread) = self.thread.lock()
                && let Some(thread) = thread.take()
            {
                let _ = thread.join();
            }
        }
    }

    impl std::fmt::Debug for LinuxNetworkChangeSource {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("LinuxNetworkChangeSource(<redacted>)")
        }
    }

    /// Observe until the controller drops its receiver, shutdown is requested,
    /// or the failure budget is spent. Each attempt that ends early counts
    /// against the budget; a long-lived one resets it.
    async fn watch(changes: mpsc::Sender<NetworkChange>, shutdown: CancellationToken) {
        let mut inventory: Option<InterfaceInventory> = None;
        let mut failures: u8 = 0;
        loop {
            let started = Instant::now();
            let outcome = tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                outcome = LinuxNetworkChangeWatcher::observe(&changes, &mut inventory) => outcome,
            };
            if outcome.is_ok() {
                return;
            }
            failures = if started.elapsed() >= HEALTHY_OBSERVATION {
                1
            } else {
                failures.saturating_add(1)
            };
            if failures >= MAX_CONSECUTIVE_FAILURES {
                return;
            }
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(RESUBSCRIBE_DELAY) => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn construction_is_inert_and_subscription_is_single_use() {
            let source = LinuxNetworkChangeSource::new();
            assert!(source.thread.lock().unwrap().is_none());

            let receiver = source.subscribe();
            assert!(receiver.is_some());
            assert!(source.thread.lock().unwrap().is_some());
            assert!(source.subscribe().is_none(), "one stream per source");
            assert!(!format!("{source:?}").contains("eth"));

            drop(receiver);
            drop(source);
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxNetworkChangeSource;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_source_yields_no_stream() {
        assert!(UnavailableNetworkChangeSource.subscribe().is_none());
        assert!(!format!("{UnavailableNetworkChangeSource:?}").is_empty());
    }
}
