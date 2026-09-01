//! Synchronous rendezvous for independently activated observer actors.
//!
//! Route and approval-store actors each reach their final drain boundary at a
//! different instant. Their callbacks cannot await one another without
//! reopening the handoff gap. This owner and its two one-use weak callbacks
//! therefore rendezvous under a short standard mutex. The second callback
//! atomically consumes both pre-read baseline tokens and installs the one
//! combined epoch. Dropping either unused callback or the owner retires the
//! incarnation instead of leaving partial health behind.

use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use thiserror::Error;
use tokio::sync::Notify;

use super::{
    HealthyRoutedEpoch, ObserverCoordinatorError, RouteBaselineToken, RoutedObserverIncarnation,
    StoreBaselineToken,
};

/// Topology-free failure to rendezvous two exact observer baselines.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum PairedObserverActivationError {
    #[error("combined observer activation was rejected: {0}")]
    Coordinator(ObserverCoordinatorError),
    #[error("a combined observer activation callback was dropped")]
    CallbackDropped,
    #[error("the combined observer activation owner was dropped")]
    OwnerDropped,
    #[error("combined observer activation state failed closed")]
    FailedClosed,
    #[error("the combined observer activation callback was already completed")]
    AlreadyCompleted,
    #[error("the combined observer epoch was already taken")]
    EpochAlreadyTaken,
}

impl From<ObserverCoordinatorError> for PairedObserverActivationError {
    fn from(error: ObserverCoordinatorError) -> Self {
        Self::Coordinator(error)
    }
}

enum ActivationOutcome {
    Pending,
    Ready(HealthyRoutedEpoch),
    Failed(PairedObserverActivationError),
    Taken,
}

struct ActivationState {
    incarnation: Option<RoutedObserverIncarnation>,
    route: Option<RouteBaselineToken>,
    store: Option<StoreBaselineToken>,
    outcome: ActivationOutcome,
}

struct ActivationInner {
    state: Mutex<ActivationState>,
    ready: Notify,
}

impl ActivationInner {
    fn offer_route(
        &self,
        baseline: RouteBaselineToken,
    ) -> Result<(), PairedObserverActivationError> {
        self.offer(SourceBaseline::Route(baseline))
    }

    fn offer_store(
        &self,
        baseline: StoreBaselineToken,
    ) -> Result<(), PairedObserverActivationError> {
        self.offer(SourceBaseline::Store(baseline))
    }

    fn offer(&self, baseline: SourceBaseline) -> Result<(), PairedObserverActivationError> {
        let mut retired = None;
        let (result, notify) = match self.state.lock() {
            Ok(mut state) => {
                let terminal_error = match &state.outcome {
                    ActivationOutcome::Pending => None,
                    ActivationOutcome::Failed(error) => Some(*error),
                    ActivationOutcome::Ready(_) | ActivationOutcome::Taken => {
                        Some(PairedObserverActivationError::AlreadyCompleted)
                    }
                };
                if let Some(error) = terminal_error {
                    (Err(error), false)
                } else {
                    let validation = {
                        let incarnation = state
                            .incarnation
                            .as_ref()
                            .expect("a pending activation retains its incarnation");
                        match &baseline {
                            SourceBaseline::Route(baseline) => {
                                incarnation.validate_route_baseline_current(baseline)
                            }
                            SourceBaseline::Store(baseline) => {
                                incarnation.validate_store_baseline_current(baseline)
                            }
                        }
                    }
                    .map_err(PairedObserverActivationError::Coordinator);

                    if let Err(error) = validation {
                        state.outcome = ActivationOutcome::Failed(error);
                        retired = state.incarnation.take();
                        (Err(error), true)
                    } else if match baseline {
                        SourceBaseline::Route(baseline) => state.route.replace(baseline).is_some(),
                        SourceBaseline::Store(baseline) => state.store.replace(baseline).is_some(),
                    } {
                        let error = PairedObserverActivationError::AlreadyCompleted;
                        state.outcome = ActivationOutcome::Failed(error);
                        retired = state.incarnation.take();
                        (Err(error), true)
                    } else if state.route.is_none() || state.store.is_none() {
                        (Ok(()), false)
                    } else {
                        let route = state.route.take().expect("checked route baseline");
                        let store = state.store.take().expect("checked store baseline");
                        let activation = state
                            .incarnation
                            .as_ref()
                            .expect("a pending activation retains its incarnation")
                            .activate(route, store)
                            .map_err(PairedObserverActivationError::Coordinator);
                        match activation {
                            Ok(epoch) => {
                                state.outcome = ActivationOutcome::Ready(epoch);
                                (Ok(()), true)
                            }
                            Err(error) => {
                                state.outcome = ActivationOutcome::Failed(error);
                                retired = state.incarnation.take();
                                (Err(error), true)
                            }
                        }
                    }
                }
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::FailedClosed);
                retired = state.incarnation.take();
                (Err(PairedObserverActivationError::FailedClosed), true)
            }
        };

        drop(retired);
        if notify {
            self.ready.notify_one();
        }
        result
    }

    fn callback_dropped(&self) {
        let mut retired = None;
        let notify = match self.state.lock() {
            Ok(mut state) if matches!(state.outcome, ActivationOutcome::Pending) => {
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::CallbackDropped);
                retired = state.incarnation.take();
                true
            }
            Ok(_) => false,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::FailedClosed);
                retired = state.incarnation.take();
                true
            }
        };
        drop(retired);
        if notify {
            self.ready.notify_one();
        }
    }

    fn owner_dropped(&self) {
        let retired = match self.state.lock() {
            Ok(mut state) => {
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::OwnerDropped);
                state.incarnation.take()
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::FailedClosed);
                state.incarnation.take()
            }
        };
        drop(retired);
        self.ready.notify_one();
    }

    fn take_epoch(&self) -> Result<Option<HealthyRoutedEpoch>, PairedObserverActivationError> {
        let mut retired = None;
        let result = match self.state.lock() {
            Ok(mut state) => match state.outcome {
                ActivationOutcome::Pending => Ok(None),
                ActivationOutcome::Failed(error) => Err(error),
                ActivationOutcome::Taken => Err(PairedObserverActivationError::EpochAlreadyTaken),
                ActivationOutcome::Ready(_) => {
                    let ActivationOutcome::Ready(epoch) =
                        std::mem::replace(&mut state.outcome, ActivationOutcome::Taken)
                    else {
                        unreachable!("the activation outcome was checked")
                    };
                    Ok(Some(epoch))
                }
            },
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.outcome =
                    ActivationOutcome::Failed(PairedObserverActivationError::FailedClosed);
                retired = state.incarnation.take();
                Err(PairedObserverActivationError::FailedClosed)
            }
        };
        drop(retired);
        result
    }
}

enum SourceBaseline {
    Route(RouteBaselineToken),
    Store(StoreBaselineToken),
}

/// Owner of one exact paired observer activation and its incarnation.
///
/// This value is non-cloneable and must live for the complete observer pair
/// lifetime. Dropping it drops the incarnation and synchronously revokes every
/// registration, while callbacks retain only weak references and cannot keep
/// old authority alive.
#[must_use = "paired observer activation must be retained for the observer lifetime"]
pub(super) struct PairedObserverActivation {
    inner: Arc<ActivationInner>,
}

impl PairedObserverActivation {
    /// Wait until both no-await callbacks activate one combined epoch.
    pub(super) async fn take_epoch(
        &mut self,
    ) -> Result<HealthyRoutedEpoch, PairedObserverActivationError> {
        loop {
            // Create the notification future before checking state. Notify
            // retains one permit for this sole owner waiter if the callback
            // wins immediately before the await.
            let notified = self.inner.ready.notified();
            if let Some(epoch) = self.inner.take_epoch()? {
                return Ok(epoch);
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn try_take_epoch(&self) -> Result<Option<HealthyRoutedEpoch>, PairedObserverActivationError> {
        self.inner.take_epoch()
    }
}

impl Drop for PairedObserverActivation {
    fn drop(&mut self) {
        self.inner.owner_dropped();
    }
}

impl fmt::Debug for PairedObserverActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairedObserverActivation(<redacted>)")
    }
}

/// One-use route-side callback for a final clean monitor drain.
#[must_use = "the route activation callback must be moved into its observer actor"]
pub(super) struct RouteActivationCallback {
    inner: Weak<ActivationInner>,
    armed: bool,
}

impl RouteActivationCallback {
    pub(super) fn activate(
        mut self,
        baseline: RouteBaselineToken,
    ) -> Result<(), PairedObserverActivationError> {
        self.armed = false;
        self.inner
            .upgrade()
            .ok_or(PairedObserverActivationError::OwnerDropped)?
            .offer_route(baseline)
    }
}

impl Drop for RouteActivationCallback {
    fn drop(&mut self) {
        if self.armed
            && let Some(inner) = self.inner.upgrade()
        {
            inner.callback_dropped();
        }
    }
}

impl fmt::Debug for RouteActivationCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteActivationCallback(<redacted>)")
    }
}

/// One-use approval-store-side callback for a final clean watcher drain.
#[must_use = "the store activation callback must be moved into its observer actor"]
pub(super) struct StoreActivationCallback {
    inner: Weak<ActivationInner>,
    armed: bool,
}

impl StoreActivationCallback {
    pub(super) fn activate(
        mut self,
        baseline: StoreBaselineToken,
    ) -> Result<(), PairedObserverActivationError> {
        self.armed = false;
        self.inner
            .upgrade()
            .ok_or(PairedObserverActivationError::OwnerDropped)?
            .offer_store(baseline)
    }
}

impl Drop for StoreActivationCallback {
    fn drop(&mut self) {
        if self.armed
            && let Some(inner) = self.inner.upgrade()
        {
            inner.callback_dropped();
        }
    }
}

impl fmt::Debug for StoreActivationCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreActivationCallback(<redacted>)")
    }
}

impl RoutedObserverIncarnation {
    /// Split this incarnation into one owner and two final-drain callbacks.
    ///
    /// Each observer mints its source token after subscribing and immediately
    /// before its corresponding route snapshot or exact store reread. Its
    /// callback accepts that token only at the final clean drain. This method
    /// intentionally does not expose the incarnation again.
    pub(super) fn into_paired_activation(
        self,
    ) -> (
        PairedObserverActivation,
        RouteActivationCallback,
        StoreActivationCallback,
    ) {
        let inner = Arc::new(ActivationInner {
            state: Mutex::new(ActivationState {
                incarnation: Some(self),
                route: None,
                store: None,
                outcome: ActivationOutcome::Pending,
            }),
            ready: Notify::new(),
        });
        (
            PairedObserverActivation {
                inner: Arc::clone(&inner),
            },
            RouteActivationCallback {
                inner: Arc::downgrade(&inner),
                armed: true,
            },
            StoreActivationCallback {
                inner: Arc::downgrade(&inner),
                armed: true,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;
    use crate::discovery::approval::controller::{
        RouteObserverSink, RoutedObserverCoordinator, StoreObserverSink,
    };
    use crate::discovery::approval::watch::StoreInvalidationSink;

    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }

    impl<T: ?Sized> AmbiguousIfClone<()> for T {}

    struct ImplementsClone;

    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}

    struct Fixture {
        coordinator: RoutedObserverCoordinator,
        activation: PairedObserverActivation,
        route_callback: RouteActivationCallback,
        store_callback: StoreActivationCallback,
        route_baseline: RouteBaselineToken,
        store_baseline: StoreBaselineToken,
        route_sink: Arc<RouteObserverSink>,
        store_sink: StoreObserverSink,
    }

    fn fixture() -> Fixture {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route_sink = Arc::new(incarnation.take_route_sink().unwrap());
        let store_sink = incarnation.take_store_sink().unwrap();
        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        let (activation, route_callback, store_callback) = incarnation.into_paired_activation();
        Fixture {
            coordinator,
            activation,
            route_callback,
            store_callback,
            route_baseline,
            store_baseline,
            route_sink,
            store_sink,
        }
    }

    #[test]
    fn activation_owner_and_callbacks_do_not_implement_clone() {
        let _ = <PairedObserverActivation as AmbiguousIfClone<_>>::marker;
        let _ = <RouteActivationCallback as AmbiguousIfClone<_>>::marker;
        let _ = <StoreActivationCallback as AmbiguousIfClone<_>>::marker;
    }

    #[tokio::test]
    async fn either_callback_order_mints_exactly_one_epoch() {
        let mut route_first = fixture();
        route_first
            .route_callback
            .activate(route_first.route_baseline)
            .unwrap();
        assert!(route_first.activation.try_take_epoch().unwrap().is_none());
        route_first
            .store_callback
            .activate(route_first.store_baseline)
            .unwrap();
        let epoch = route_first.activation.take_epoch().await.unwrap();
        assert!(
            route_first
                .coordinator
                .register(&epoch)
                .unwrap()
                .is_current()
        );
        assert_eq!(
            route_first.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::EpochAlreadyTaken
        );

        let mut store_first = fixture();
        store_first
            .store_callback
            .activate(store_first.store_baseline)
            .unwrap();
        assert!(store_first.activation.try_take_epoch().unwrap().is_none());
        store_first
            .route_callback
            .activate(store_first.route_baseline)
            .unwrap();
        let epoch = store_first.activation.take_epoch().await.unwrap();
        assert!(
            store_first
                .coordinator
                .register(&epoch)
                .unwrap()
                .is_current()
        );
    }

    #[tokio::test]
    async fn concurrent_callbacks_linearize_to_one_epoch() {
        let mut fixture = fixture();
        let barrier = Arc::new(Barrier::new(3));
        let route_barrier = Arc::clone(&barrier);
        let store_barrier = Arc::clone(&barrier);
        thread::scope(|scope| {
            let route = scope.spawn(move || {
                route_barrier.wait();
                fixture.route_callback.activate(fixture.route_baseline)
            });
            let store = scope.spawn(move || {
                store_barrier.wait();
                fixture.store_callback.activate(fixture.store_baseline)
            });
            barrier.wait();
            assert_eq!(route.join().unwrap(), Ok(()));
            assert_eq!(store.join().unwrap(), Ok(()));
        });
        let epoch = fixture.activation.take_epoch().await.unwrap();
        assert!(fixture.coordinator.register(&epoch).unwrap().is_current());
    }

    #[tokio::test]
    async fn source_event_between_callbacks_prevents_activation() {
        let mut fixture = fixture();
        fixture
            .route_callback
            .activate(fixture.route_baseline)
            .unwrap();
        StoreInvalidationSink::invalidate(&fixture.store_sink);
        assert_eq!(
            fixture
                .store_callback
                .activate(fixture.store_baseline)
                .unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
        assert_eq!(
            fixture.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
    }

    #[tokio::test]
    async fn an_event_invalidating_the_stored_first_token_prevents_activation() {
        let mut route_first = fixture();
        route_first
            .route_callback
            .activate(route_first.route_baseline)
            .unwrap();
        route_first.route_sink.invalidate();
        assert_eq!(
            route_first
                .store_callback
                .activate(route_first.store_baseline)
                .unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
        assert_eq!(
            route_first.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );

        let mut store_first = fixture();
        store_first
            .store_callback
            .activate(store_first.store_baseline)
            .unwrap();
        StoreInvalidationSink::invalidate(&store_first.store_sink);
        assert_eq!(
            store_first
                .route_callback
                .activate(store_first.route_baseline)
                .unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
        assert_eq!(
            store_first.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
    }

    #[test]
    fn foreign_baselines_are_rejected_at_the_exact_callback_boundary() {
        let first = RoutedObserverCoordinator::new();
        let mut first_incarnation = first.start_incarnation().unwrap();
        let _first_route_sink = first_incarnation.take_route_sink().unwrap();
        let _first_store_sink = first_incarnation.take_store_sink().unwrap();
        let foreign_route = first_incarnation.begin_route_baseline().unwrap();

        let second = RoutedObserverCoordinator::new();
        let mut second_incarnation = second.start_incarnation().unwrap();
        let _second_route_sink = second_incarnation.take_route_sink().unwrap();
        let _second_store_sink = second_incarnation.take_store_sink().unwrap();
        let (activation, route_callback, store_callback) =
            second_incarnation.into_paired_activation();
        assert_eq!(
            route_callback.activate(foreign_route).unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
        assert_eq!(
            activation.try_take_epoch().unwrap_err(),
            PairedObserverActivationError::Coordinator(ObserverCoordinatorError::StaleBaseline)
        );
        drop(store_callback);
    }

    #[tokio::test]
    async fn dropping_an_unused_callback_retires_partial_activation() {
        let mut fixture = fixture();
        fixture
            .route_callback
            .activate(fixture.route_baseline)
            .unwrap();
        drop(fixture.store_callback);
        assert_eq!(
            fixture.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::CallbackDropped
        );
        assert!(fixture.coordinator.start_incarnation().is_ok());
    }

    #[test]
    fn dropping_owner_makes_stale_callbacks_inert() {
        let fixture = fixture();
        let _escaped_inner = Arc::clone(&fixture.activation.inner);
        drop(fixture.activation);
        assert_eq!(
            fixture
                .route_callback
                .activate(fixture.route_baseline)
                .unwrap_err(),
            PairedObserverActivationError::OwnerDropped
        );
        assert_eq!(
            fixture
                .store_callback
                .activate(fixture.store_baseline)
                .unwrap_err(),
            PairedObserverActivationError::OwnerDropped
        );
        assert!(fixture.coordinator.start_incarnation().is_ok());
    }

    #[tokio::test]
    async fn waiter_cannot_lose_an_activation_notification() {
        let mut fixture = fixture();
        let wait = fixture.activation.take_epoch();
        let activate = async {
            tokio::task::yield_now().await;
            fixture
                .route_callback
                .activate(fixture.route_baseline)
                .unwrap();
            fixture
                .store_callback
                .activate(fixture.store_baseline)
                .unwrap();
        };
        let (epoch, ()) = tokio::join!(wait, activate);
        assert!(
            fixture
                .coordinator
                .register(&epoch.unwrap())
                .unwrap()
                .is_current()
        );
    }

    #[tokio::test]
    async fn poisoned_activation_mutex_retires_the_incarnation() {
        let mut fixture = fixture();
        let inner = Arc::clone(&fixture.activation.inner);
        assert!(
            thread::spawn(move || {
                let _state = inner.state.lock().unwrap();
                panic!("poison activation state for the regression fixture");
            })
            .join()
            .is_err()
        );

        assert_eq!(
            fixture
                .route_callback
                .activate(fixture.route_baseline)
                .unwrap_err(),
            PairedObserverActivationError::FailedClosed
        );
        assert_eq!(
            fixture.activation.take_epoch().await.unwrap_err(),
            PairedObserverActivationError::FailedClosed
        );
    }

    #[tokio::test]
    async fn dropping_active_owner_cancels_escaped_registration_signal() {
        let mut fixture = fixture();
        fixture
            .route_callback
            .activate(fixture.route_baseline)
            .unwrap();
        fixture
            .store_callback
            .activate(fixture.store_baseline)
            .unwrap();
        let epoch = fixture.activation.take_epoch().await.unwrap();
        let registration = fixture.coordinator.register(&epoch).unwrap();
        let cancellation = registration.cancellation().clone();
        let _escaped_inner = Arc::clone(&fixture.activation.inner);
        drop(fixture.activation);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn debug_and_errors_are_topology_free() {
        let fixture = fixture();
        let rendered = format!(
            "{:?} {:?} {:?} {:?}",
            fixture.activation,
            fixture.route_callback,
            fixture.store_callback,
            PairedObserverActivationError::CallbackDropped,
        );
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("192.168"));
        assert!(!rendered.contains("wg-test"));
    }
}
