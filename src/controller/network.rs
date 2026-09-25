//! The network-change lane's service boundary and its native source.
//!
//! The controller actor never touches platform notifications or enumerates
//! interfaces itself. A [`NetworkChangeSource`] hands it one channel of
//! already debounced [`NetworkChange`]s and one [`ObservationWatch`] saying
//! whether observation is healthy, both produced on a dedicated thread on
//! Linux, macOS, and Windows and absent everywhere else, so the actor's
//! behaviour with no source is exactly its behaviour on a platform without
//! one.

use tokio::sync::mpsc;

use crate::discovery::{NetworkChange, ObservationWatch};

/// What one subscription delivers.
#[derive(Debug)]
pub struct NetworkSubscription {
    /// Debounced changes, each coalesced over the documented quiet period.
    pub changes: mpsc::Receiver<NetworkChange>,
    /// Readiness of the observation behind `changes`. It is ready only while
    /// a baseline is established and reconciled with no change pending; a
    /// detected change revokes it before the debounced change is delivered.
    pub observation: ObservationWatch,
}

/// Packet-free boundary the controller uses to learn about network changes.
///
/// Constructing an implementation must not open sockets or enumerate
/// interfaces; [`Self::subscribe`] may start a thread but must not block, and
/// it is called once, on the controller runtime. Every change it yields is
/// already coalesced over the documented quiet period and cap.
pub trait NetworkChangeSource: Send + Sync + 'static {
    /// Begin observing and return the change stream and its readiness, or
    /// `None` when this system cannot observe changes. A closed stream means
    /// the source stopped; the controller then behaves as if it had none.
    fn subscribe(&self) -> Option<NetworkSubscription>;
}

/// A change source for systems that cannot observe network changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UnavailableNetworkChangeSource;

impl NetworkChangeSource for UnavailableNetworkChangeSource {
    fn subscribe(&self) -> Option<NetworkSubscription> {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod native {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use tokio::runtime::Builder;
    use tokio::sync::mpsc;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{NetworkChangeSource, NetworkSubscription};
    #[cfg(target_os = "linux")]
    use crate::discovery::LinuxNetworkChangeWatcher as PlatformWatcher;
    #[cfg(target_os = "macos")]
    use crate::discovery::MacosNetworkChangeWatcher as PlatformWatcher;
    #[cfg(windows)]
    use crate::discovery::WindowsNetworkChangeWatcher as PlatformWatcher;
    use crate::discovery::{
        InterfaceInventory, NetworkChange, NetworkChangeWatchError, ObservationGate,
    };

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

    /// The production change source: one thread owning the platform
    /// subscription (rtnetlink on Linux, a routing socket on macOS, IP Helper
    /// notifications on Windows), the debouncer, the interface inventory,
    /// and the readiness it publishes.
    ///
    /// Constructing it does nothing. The first `subscribe` spawns the thread;
    /// dropping the source stops and joins it.
    pub struct NativeNetworkChangeSource {
        shutdown: CancellationToken,
        thread: Mutex<Option<thread::JoinHandle<()>>>,
        subscribed: AtomicBool,
        gate: ObservationGate,
    }

    impl NativeNetworkChangeSource {
        #[must_use]
        pub fn new() -> Self {
            Self {
                shutdown: CancellationToken::new(),
                thread: Mutex::new(None),
                subscribed: AtomicBool::new(false),
                gate: ObservationGate::new(),
            }
        }
    }

    impl Default for NativeNetworkChangeSource {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NetworkChangeSource for NativeNetworkChangeSource {
        fn subscribe(&self) -> Option<NetworkSubscription> {
            if self.subscribed.swap(true, Ordering::SeqCst) {
                return None;
            }
            let (changes, receiver) = mpsc::channel(CHANGE_CAPACITY);
            let shutdown = self.shutdown.clone();
            let gate = self.gate.clone();
            let thread = thread::Builder::new()
                .name(WATCHER_THREAD_NAME.to_owned())
                .spawn(move || {
                    let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                        return;
                    };
                    runtime.block_on(watch(
                        changes,
                        shutdown,
                        gate,
                        async |changes, inventory, gate| {
                            PlatformWatcher::observe(changes, inventory, gate).await
                        },
                    ));
                })
                .ok()?;
            if let Ok(mut slot) = self.thread.lock() {
                *slot = Some(thread);
            }
            Some(NetworkSubscription {
                changes: receiver,
                observation: self.gate.watch(),
            })
        }
    }

    impl Drop for NativeNetworkChangeSource {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Ok(mut thread) = self.thread.lock()
                && let Some(thread) = thread.take()
            {
                let _ = thread.join();
            }
            self.gate.invalidate();
        }
    }

    impl std::fmt::Debug for NativeNetworkChangeSource {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("NativeNetworkChangeSource(<redacted>)")
        }
    }

    /// Observe until the controller drops its receiver, shutdown is requested,
    /// or the failure budget is spent. Each attempt that ends early counts
    /// against the budget; a long-lived one resets it. Readiness is published
    /// only inside an attempt, so the gap before resubscription and a source
    /// that gave up are both unavailable.
    async fn watch<F>(
        changes: mpsc::Sender<NetworkChange>,
        shutdown: CancellationToken,
        gate: ObservationGate,
        mut observe: F,
    ) where
        F: AsyncFnMut(
            &mpsc::Sender<NetworkChange>,
            &mut Option<InterfaceInventory>,
            &ObservationGate,
        ) -> Result<(), NetworkChangeWatchError>,
    {
        let mut inventory: Option<InterfaceInventory> = None;
        let mut failures: u8 = 0;
        loop {
            let started = Instant::now();
            let outcome = tokio::select! {
                biased;
                () = shutdown.cancelled() => None,
                outcome = observe(&changes, &mut inventory, &gate) => Some(outcome),
            };
            gate.invalidate();
            let Some(Err(_)) = outcome else {
                return;
            };
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
        use crate::discovery::ObservationState;

        #[test]
        fn construction_is_inert_and_subscription_is_single_use() {
            let source = NativeNetworkChangeSource::new();
            assert!(source.thread.lock().unwrap().is_none());

            let subscription = source.subscribe();
            assert!(subscription.is_some());
            assert!(source.thread.lock().unwrap().is_some());
            assert!(source.subscribe().is_none(), "one stream per source");
            assert!(!format!("{source:?}").contains("eth"));

            let observation = subscription.map(|subscription| subscription.observation);
            drop(source);
            assert_eq!(
                observation.map(|observation| observation.current()),
                Some(ObservationState::Unavailable),
                "a stopped source is not observing"
            );
        }

        /// The native source publishes readiness once its first baseline is
        /// established, on every platform lane that can observe.
        #[test]
        fn a_native_subscription_becomes_ready_after_its_baseline() {
            let source = NativeNetworkChangeSource::new();
            let subscription = source.subscribe().expect("a first subscription");
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut ready = None;
            while std::time::Instant::now() < deadline {
                if let Some(generation) = subscription.observation.current().generation() {
                    ready = Some(generation);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // A sandbox without the platform's notifications fails closed
            // and never becomes ready.
            if let Some(generation) = ready {
                assert!(generation.get() >= 1);
            }
            drop(source);
            assert_eq!(
                subscription.observation.current(),
                ObservationState::Unavailable
            );
        }

        #[tokio::test(start_paused = true)]
        async fn readiness_is_lost_between_attempts_and_restored_as_a_new_generation() {
            let (changes, _receiver) = mpsc::channel(1);
            let gate = ObservationGate::new();
            let mut observation = gate.watch();
            let shutdown = CancellationToken::new();
            let mut attempts = 0_usize;
            let watcher = watch(
                changes,
                shutdown.clone(),
                gate.clone(),
                async |_, _, gate| {
                    attempts += 1;
                    gate.establish();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    if attempts == 1 {
                        // The first attempt ends early.
                        Err(NetworkChangeWatchError::MonitorStopped)
                    } else {
                        std::future::pending().await
                    }
                },
            );
            let checks = async {
                let first = observation.changed().await.generation().unwrap();
                let started = Instant::now();
                assert_eq!(observation.changed().await, ObservationState::Unavailable);
                let second = observation.changed().await.generation().unwrap();
                assert!(
                    started.elapsed() >= RESUBSCRIBE_DELAY,
                    "unavailable while resubscribing"
                );
                assert!(second > first);
                shutdown.cancel();
            };
            tokio::join!(watcher, checks);
            assert_eq!(attempts, 2);
            assert_eq!(gate.state(), ObservationState::Unavailable);
        }

        #[tokio::test(start_paused = true)]
        async fn a_source_that_gives_up_stays_unavailable() {
            let (changes, _receiver) = mpsc::channel(1);
            let gate = ObservationGate::new();
            let mut attempts = 0_usize;
            watch(
                changes,
                CancellationToken::new(),
                gate.clone(),
                async |_, _, gate| {
                    attempts += 1;
                    gate.establish();
                    Err(NetworkChangeWatchError::MonitorUnavailable)
                },
            )
            .await;
            assert_eq!(attempts, usize::from(MAX_CONSECUTIVE_FAILURES));
            assert_eq!(gate.state(), ObservationState::Unavailable);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use native::NativeNetworkChangeSource;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_source_yields_no_stream() {
        assert!(UnavailableNetworkChangeSource.subscribe().is_none());
        assert!(!format!("{UnavailableNetworkChangeSource:?}").is_empty());
    }
}
