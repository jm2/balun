//! Paired Linux route and approval-store observer ownership.
//!
//! A pair prepares both source observers against one coordinator incarnation,
//! hands each source's exact pre-read token to its final-drain callback, and
//! retains the paired activation owner for the complete live lifetime. Any
//! child event is coalesced into one replacement request. Explicit shutdown
//! retires the exact incarnation synchronously before returning a future
//! which awaits destruction of both actors.

#![cfg(target_os = "linux")]

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use thiserror::Error;

use crate::discovery::approval::store::ApprovalStore;
use crate::discovery::routes::RouteSnapshot;

use super::activation::{
    PairedObserverActivation, PairedObserverActivationError, RouteActivationCallback,
    StoreActivationCallback,
};
use super::linux::{
    LinuxRouteObserverBridgeError, LinuxRouteObserverSession, LinuxRouteObserverTermination,
    PreparedLinuxRouteObserver,
};
use super::store::{
    LinuxStoreObserverBridgeError, LinuxStoreObserverSession, LinuxStoreObserverTermination,
    PreparedLinuxStoreObserver,
};
use super::{
    HealthyRoutedEpoch, ObserverCoordinatorError, RoutedObserverCoordinator,
    RoutedObserverIncarnation,
};

/// Topology- and store-content-free failure to establish a paired observer.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum LinuxObserverPairError {
    #[error("the paired observer incarnation could not be started: {0}")]
    Coordinator(ObserverCoordinatorError),
    #[error("route observation could not be prepared or started: {0}")]
    Route(LinuxRouteObserverBridgeError),
    #[error("approval-store observation could not be prepared or started: {0}")]
    Store(LinuxStoreObserverBridgeError),
    #[error("the paired observer could not activate: {0}")]
    Activation(PairedObserverActivationError),
}

/// One coalesced signal that the complete observer pair must be replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxObserverPairEvent {
    ReplacementRequired,
}

/// Terminal outcomes after both observer actors have been destroyed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LinuxObserverPairShutdown {
    pub(super) route: LinuxRouteObserverTermination,
    pub(super) store: LinuxStoreObserverTermination,
}

/// The complete live Linux observer pair.
///
/// The pair is deliberately non-cloneable. It retains the exact activation
/// owner, both actors, and a strong coordinator reference. The controller
/// must call [`Self::shutdown`] and await its returned future before starting
/// a replacement. Dropping the pair is a synchronous fail-closed fallback.
#[must_use = "a paired observer must be driven and shut down explicitly"]
pub(super) struct LinuxObserverPair {
    owner: PairedObserverOwner<LinuxRouteObserverSession, LinuxStoreObserverSession>,
}

impl LinuxObserverPair {
    /// Prepare and activate one exact route/store observer incarnation.
    ///
    /// Route subscription precedes its snapshot, and approval-store
    /// subscription precedes the caller's exact reread. That reread receives
    /// the monitored route snapshot, allowing route/time revalidation to stay
    /// inside the watcher's exact sandwich. Its result is returned without
    /// interpretation so stronger admission typestates can cross this handoff
    /// without being weakened to a raw permit.
    pub(super) async fn prepare_and_activate<R, E, Read>(
        coordinator: Arc<RoutedObserverCoordinator>,
        store: Arc<ApprovalStore>,
        exact_reread: Read,
    ) -> Result<(RouteSnapshot, R, HealthyRoutedEpoch, Self), LinuxObserverPairError>
    where
        R: Send + 'static,
        E: Send + 'static,
        Read: FnOnce(&ApprovalStore, &RouteSnapshot) -> Result<R, E> + Send + 'static,
    {
        let mut incarnation = coordinator
            .start_incarnation()
            .map_err(LinuxObserverPairError::Coordinator)?;
        let (route_snapshot, route) = PreparedLinuxRouteObserver::prepare(&mut incarnation)
            .await
            .map_err(LinuxObserverPairError::Route)?;
        let reread_snapshot = route_snapshot.clone();
        let (reread, store) =
            PreparedLinuxStoreObserver::prepare(&mut incarnation, store, move |store| {
                exact_reread(store, &reread_snapshot)
            })
            .await
            .map_err(LinuxObserverPairError::Store)?;

        let mut owner = PairedObserverOwner::start(
            coordinator,
            incarnation,
            move |activation| route.start(activation),
            move |activation| store.start(activation),
        )
        .map_err(|error| match error {
            PairStartError::Route(error) => LinuxObserverPairError::Route(error),
            PairStartError::Store(error) => LinuxObserverPairError::Store(error),
        })?;
        let epoch = owner
            .take_epoch()
            .await
            .map_err(LinuxObserverPairError::Activation)?;

        Ok((route_snapshot, reread, epoch, Self { owner }))
    }

    /// Wait for either child to require replacement.
    ///
    /// Source actors revoke authority before publishing their events. The
    /// pair latches the first event, so simultaneous route and store changes
    /// become one whole-pair replacement request.
    pub(super) async fn next_event(&mut self) -> LinuxObserverPairEvent {
        self.owner.next_event().await
    }

    /// Retire this exact incarnation now and return the two-actor join future.
    ///
    /// This is intentionally not an `async fn`: activation retirement occurs
    /// while `shutdown` is called, even if the returned future is never
    /// polled. Cancelling that future drops both still-owned sessions, whose
    /// fail-closed fallbacks poison and abort their actors.
    pub(super) fn shutdown(self) -> impl Future<Output = LinuxObserverPairShutdown> {
        let shutdown = self.owner.shutdown();
        async move {
            let (route, store) = shutdown.await;
            LinuxObserverPairShutdown { route, store }
        }
    }
}

impl fmt::Debug for LinuxObserverPair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinuxObserverPair(<redacted>)")
    }
}

/// Minimal lifecycle required from either child actor owner.
trait ObserverActorOwner {
    type Termination;

    async fn next_event(&mut self);
    async fn shutdown(self) -> Self::Termination;
}

impl ObserverActorOwner for LinuxRouteObserverSession {
    type Termination = LinuxRouteObserverTermination;

    async fn next_event(&mut self) {
        let _event = LinuxRouteObserverSession::next_event(self).await;
    }

    async fn shutdown(self) -> Self::Termination {
        LinuxRouteObserverSession::shutdown(self).await
    }
}

impl ObserverActorOwner for LinuxStoreObserverSession {
    type Termination = LinuxStoreObserverTermination;

    async fn next_event(&mut self) {
        let _event = LinuxStoreObserverSession::next_event(self).await;
    }

    async fn shutdown(self) -> Self::Termination {
        LinuxStoreObserverSession::shutdown(self).await
    }
}

enum PairStartError<RouteError, StoreError> {
    Route(RouteError),
    Store(StoreError),
}

/// Testable, source-agnostic ownership core used by the concrete Linux pair.
struct PairedObserverOwner<Route, Store> {
    // Drop order is security-relevant: retire the exact incarnation before
    // child sessions release their source-bound poison handles, and keep the
    // coordinator alive until both operations are complete.
    activation: Option<PairedObserverActivation>,
    route: Option<Route>,
    store: Option<Store>,
    coordinator: Option<Arc<RoutedObserverCoordinator>>,
    replacement_required: bool,
}

impl<Route, Store> PairedObserverOwner<Route, Store> {
    fn start<RouteError, StoreError, StartRoute, StartStore>(
        coordinator: Arc<RoutedObserverCoordinator>,
        incarnation: RoutedObserverIncarnation,
        start_route: StartRoute,
        start_store: StartStore,
    ) -> Result<Self, PairStartError<RouteError, StoreError>>
    where
        StartRoute: FnOnce(RouteActivationCallback) -> Result<Route, RouteError>,
        StartStore: FnOnce(StoreActivationCallback) -> Result<Store, StoreError>,
    {
        let (activation, route_activation, store_activation) = incarnation.into_paired_activation();
        let route = start_route(route_activation).map_err(PairStartError::Route)?;
        let store = start_store(store_activation).map_err(PairStartError::Store)?;

        Ok(Self {
            activation: Some(activation),
            route: Some(route),
            store: Some(store),
            coordinator: Some(coordinator),
            replacement_required: false,
        })
    }

    async fn take_epoch(&mut self) -> Result<HealthyRoutedEpoch, PairedObserverActivationError> {
        self.activation
            .as_mut()
            .expect("a live pair retains its activation owner")
            .take_epoch()
            .await
    }

    fn retire_activation(&mut self) {
        // Dropping the activation drops its exact incarnation. Retirement is
        // source-bound, so a late old owner cannot invalidate a newer pair.
        drop(self.activation.take());
    }
}

impl<Route, Store> PairedObserverOwner<Route, Store>
where
    Route: ObserverActorOwner,
    Store: ObserverActorOwner,
{
    async fn next_event(&mut self) -> LinuxObserverPairEvent {
        if !self.replacement_required {
            let route = self
                .route
                .as_mut()
                .expect("a live pair retains its route actor");
            let store = self
                .store
                .as_mut()
                .expect("a live pair retains its store actor");
            tokio::select! {
                () = route.next_event() => {}
                () = store.next_event() => {}
            }
            // Child observers revoke their own source before publishing, and
            // retiring the paired activation here closes the whole
            // incarnation before the replacement event reaches its caller.
            self.retire_activation();
            self.replacement_required = true;
        }
        LinuxObserverPairEvent::ReplacementRequired
    }

    fn shutdown(mut self) -> impl Future<Output = (Route::Termination, Store::Termination)> {
        // Everything before construction of the async block is synchronous.
        self.retire_activation();
        let route = self
            .route
            .take()
            .expect("a live pair retains its route actor");
        let store = self
            .store
            .take()
            .expect("a live pair retains its store actor");
        let coordinator = self
            .coordinator
            .take()
            .expect("a live pair retains its coordinator");
        drop(self);

        async move {
            let outcomes = tokio::join!(route.shutdown(), store.shutdown());
            drop(coordinator);
            outcomes
        }
    }
}

impl<Route, Store> Drop for PairedObserverOwner<Route, Store> {
    fn drop(&mut self) {
        // Explicit order makes the fallback independent of field-drop order.
        self.retire_activation();
        drop(self.route.take());
        drop(self.store.take());
        drop(self.coordinator.take());
    }
}

impl<Route, Store> fmt::Debug for PairedObserverOwner<Route, Store> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairedObserverOwner(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::discovery::approval::controller::{RouteObserverSink, StoreObserverSink};

    enum FakeAuthority {
        Route(Arc<RouteObserverSink>),
        Store(StoreObserverSink),
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeTermination {
        Stopped,
    }

    struct FakeActor {
        event: mpsc::Receiver<()>,
        shutdown_started: Option<oneshot::Sender<()>>,
        shutdown_release: Option<oneshot::Receiver<()>>,
        dropped: Arc<AtomicBool>,
        _authority: FakeAuthority,
    }

    impl Drop for FakeActor {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl ObserverActorOwner for FakeActor {
        type Termination = FakeTermination;

        async fn next_event(&mut self) {
            let _event = self.event.recv().await;
        }

        async fn shutdown(mut self) -> Self::Termination {
            if let Some(started) = self.shutdown_started.take() {
                let _ = started.send(());
            }
            if let Some(release) = self.shutdown_release.take() {
                let _ = release.await;
            }
            FakeTermination::Stopped
        }
    }

    struct ActorFixture {
        actor: FakeActor,
        event: mpsc::Sender<()>,
        shutdown_started: oneshot::Receiver<()>,
        shutdown_release: oneshot::Sender<()>,
        dropped: Arc<AtomicBool>,
    }

    fn actor(authority: FakeAuthority) -> ActorFixture {
        let (event, event_receiver) = mpsc::channel(1);
        let (started_sender, shutdown_started) = oneshot::channel();
        let (shutdown_release, release_receiver) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        ActorFixture {
            actor: FakeActor {
                event: event_receiver,
                shutdown_started: Some(started_sender),
                shutdown_release: Some(release_receiver),
                dropped: Arc::clone(&dropped),
                _authority: authority,
            },
            event,
            shutdown_started,
            shutdown_release,
            dropped,
        }
    }

    struct PairFixture {
        coordinator: Arc<RoutedObserverCoordinator>,
        owner: PairedObserverOwner<FakeActor, FakeActor>,
        epoch: HealthyRoutedEpoch,
        route_event: mpsc::Sender<()>,
        store_event: mpsc::Sender<()>,
        route_started: oneshot::Receiver<()>,
        store_started: oneshot::Receiver<()>,
        route_release: oneshot::Sender<()>,
        store_release: oneshot::Sender<()>,
        route_dropped: Arc<AtomicBool>,
        store_dropped: Arc<AtomicBool>,
    }

    async fn pair_fixture() -> PairFixture {
        let coordinator = Arc::new(RoutedObserverCoordinator::new());
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route_sink = Arc::new(incarnation.take_route_sink().unwrap());
        let store_sink = incarnation.take_store_sink().unwrap();
        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        let route = actor(FakeAuthority::Route(route_sink));
        let store = actor(FakeAuthority::Store(store_sink));

        let mut owner = PairedObserverOwner::start(
            Arc::clone(&coordinator),
            incarnation,
            move |activation| {
                activation.activate(route_baseline).unwrap();
                Ok::<_, Infallible>(route.actor)
            },
            move |activation| {
                activation.activate(store_baseline).unwrap();
                Ok::<_, Infallible>(store.actor)
            },
        )
        .unwrap_or_else(|never| match never {
            PairStartError::Route(never) | PairStartError::Store(never) => match never {},
        });
        let epoch = owner.take_epoch().await.unwrap();

        PairFixture {
            coordinator,
            owner,
            epoch,
            route_event: route.event,
            store_event: store.event,
            route_started: route.shutdown_started,
            store_started: store.shutdown_started,
            route_release: route.shutdown_release,
            store_release: store.shutdown_release,
            route_dropped: route.dropped,
            store_dropped: store.dropped,
        }
    }

    #[tokio::test]
    async fn exact_callbacks_activate_one_owned_epoch() {
        let fixture = pair_fixture().await;
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();

        assert!(registration.is_current());
        assert!(!registration.cancellation().is_cancelled());
        drop(fixture);
    }

    #[tokio::test]
    async fn child_events_are_coalesced_into_one_pair_replacement() {
        let mut fixture = pair_fixture().await;
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();

        fixture.route_event.try_send(()).unwrap();
        fixture.store_event.try_send(()).unwrap();
        assert_eq!(
            fixture.owner.next_event().await,
            LinuxObserverPairEvent::ReplacementRequired
        );
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());

        // The pair-level latch consumes simultaneous child events as one
        // replacement request; a repeated read never waits on the second.
        assert_eq!(
            fixture.owner.next_event().await,
            LinuxObserverPairEvent::ReplacementRequired
        );
    }

    #[tokio::test]
    async fn shutdown_retires_synchronously_and_awaits_both_actors() {
        let fixture = pair_fixture().await;
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        let PairFixture {
            owner,
            mut route_started,
            mut store_started,
            route_release,
            store_release,
            route_dropped,
            store_dropped,
            ..
        } = fixture;

        let mut shutdown = Box::pin(owner.shutdown());
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());

        tokio::select! {
            outcomes = &mut shutdown => panic!("shutdown completed before release: {outcomes:?}"),
            starts = async { (&mut route_started).await.unwrap(); (&mut store_started).await.unwrap(); } => starts,
        }
        assert!(!route_dropped.load(Ordering::SeqCst));
        assert!(!store_dropped.load(Ordering::SeqCst));

        route_release.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(!store_dropped.load(Ordering::SeqCst));
        store_release.send(()).unwrap();
        assert_eq!(
            shutdown.await,
            (FakeTermination::Stopped, FakeTermination::Stopped)
        );
        assert!(route_dropped.load(Ordering::SeqCst));
        assert!(store_dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelling_shutdown_drops_both_actor_owners() {
        let fixture = pair_fixture().await;
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        let PairFixture {
            owner,
            mut route_started,
            mut store_started,
            route_dropped,
            store_dropped,
            ..
        } = fixture;
        let mut shutdown = Box::pin(owner.shutdown());

        tokio::select! {
            outcomes = &mut shutdown => panic!("shutdown completed before cancellation: {outcomes:?}"),
            starts = async { (&mut route_started).await.unwrap(); (&mut store_started).await.unwrap(); } => starts,
        }
        drop(shutdown);

        assert!(registration.cancellation().is_cancelled());
        assert!(route_dropped.load(Ordering::SeqCst));
        assert!(store_dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drop_retires_and_destroys_both_actor_owners() {
        let fixture = pair_fixture().await;
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        let route_dropped = Arc::clone(&fixture.route_dropped);
        let store_dropped = Arc::clone(&fixture.store_dropped);

        drop(fixture.owner);

        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());
        assert!(route_dropped.load(Ordering::SeqCst));
        assert!(store_dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn late_old_shutdown_cannot_invalidate_a_replacement_incarnation() {
        let old = pair_fixture().await;
        let coordinator = Arc::clone(&old.coordinator);
        let mut replacement = coordinator.start_incarnation().unwrap();
        let replacement_route = Arc::new(replacement.take_route_sink().unwrap());
        let replacement_store = replacement.take_store_sink().unwrap();
        let route_baseline = replacement.begin_route_baseline().unwrap();
        let store_baseline = replacement.begin_store_baseline().unwrap();
        let (mut activation, route_callback, store_callback) = replacement.into_paired_activation();
        route_callback.activate(route_baseline).unwrap();
        store_callback.activate(store_baseline).unwrap();
        let replacement_epoch = activation.take_epoch().await.unwrap();
        let registration = coordinator.register(&replacement_epoch).unwrap();

        let old_shutdown = old.owner.shutdown();
        assert!(registration.is_current());
        assert!(!registration.cancellation().is_cancelled());
        drop(old_shutdown);
        assert!(registration.is_current());
        assert!(!registration.cancellation().is_cancelled());

        drop(replacement_route);
        drop(replacement_store);
        drop(activation);
    }

    #[tokio::test]
    async fn second_start_failure_drops_the_started_sibling_and_activation() {
        let coordinator = Arc::new(RoutedObserverCoordinator::new());
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route_sink = Arc::new(incarnation.take_route_sink().unwrap());
        let store_sink = incarnation.take_store_sink().unwrap();
        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        let route = actor(FakeAuthority::Route(route_sink));
        let route_dropped = Arc::clone(&route.dropped);
        let store = actor(FakeAuthority::Store(store_sink));

        let result = PairedObserverOwner::start(
            Arc::clone(&coordinator),
            incarnation,
            move |activation| {
                activation.activate(route_baseline).unwrap();
                Ok::<_, Infallible>(route.actor)
            },
            move |activation| {
                activation.activate(store_baseline).unwrap();
                drop(store.actor);
                Err::<FakeActor, _>(())
            },
        );
        assert!(matches!(result, Err(PairStartError::Store(()))));
        assert!(route_dropped.load(Ordering::SeqCst));
        assert!(matches!(
            coordinator.start_incarnation(),
            Ok(RoutedObserverIncarnation { .. })
        ));
    }

    #[test]
    fn debug_and_errors_do_not_render_topology_or_store_values() {
        let error = LinuxObserverPairError::Activation(PairedObserverActivationError::OwnerDropped);
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("192.168"));
        assert!(!rendered.contains("site-a"));
        assert!(!rendered.contains("routed-approvals"));
    }
}
