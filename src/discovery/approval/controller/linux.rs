//! Linux route-observer ownership and snapshot handoff.
//!
//! A prepared observer subscribes before collecting a route snapshot, binds
//! one exact coordinator baseline to that subscription, and moves the token
//! only into the monitor's synchronous activation callback. The collected
//! snapshot also tells the subscribed observer which interfaces' IPv6 address
//! changes cannot reach routed authority. The live session owns both the
//! monitor task and its capacity-one reconciliation receiver.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, OnceLock};

use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::discovery::routes::{
    InterfaceId, InterfaceKind, LinuxRouteEventMonitor, LinuxRouteMonitorError, LinuxRouteProvider,
    NetworkInterface, NotificationKind, RouteMonitorObserver, RouteProvider,
    RouteReconciliationRequired, RouteSnapshot,
};

use super::activation::RouteActivationCallback;
use super::{
    ObserverCoordinatorError, RouteBaselineToken, RouteObserverSink, RoutedObserverIncarnation,
};

/// The route monitor's view of one incarnation's routed authority.
///
/// Every notification invalidates, with one exception once the baseline
/// snapshot is bound: an IPv6 address notification whose every message names
/// an interface that the snapshot holds and never classifies as a tunnel.
/// Routers refresh IPv6 address lifetimes every few seconds, and that change
/// cannot reach routed authority. Candidate selection and the approval
/// fingerprint read only IPv4 routes and addresses, except for the assigned
/// prefixes of the tunnel a candidate is routed through, which include IPv6.
/// Interface kind and state come from link attributes, and any link change is
/// a link notification, which still invalidates.
struct RouteAuthorityObserver {
    sink: Arc<RouteObserverSink>,
    /// Interfaces whose IPv6 address changes cannot affect the baseline.
    ipv6_irrelevant: OnceLock<BTreeSet<InterfaceId>>,
}

impl RouteAuthorityObserver {
    fn new(sink: Arc<RouteObserverSink>) -> Self {
        Self {
            sink,
            ipv6_irrelevant: OnceLock::new(),
        }
    }

    /// Bind the baseline snapshot this incarnation's authority derives from.
    ///
    /// An incarnation has exactly one baseline. A second binding cannot be
    /// reconciled with notifications already judged, so it fails closed.
    fn bind(&self, snapshot: &RouteSnapshot) {
        let tunnels = snapshot
            .interfaces()
            .iter()
            .filter(|interface| interface.kind() == InterfaceKind::Tunnel)
            .map(NetworkInterface::id)
            .collect::<BTreeSet<_>>();
        let irrelevant = snapshot
            .interfaces()
            .iter()
            .map(NetworkInterface::id)
            .filter(|id| !tunnels.contains(id))
            .collect();
        if self.ipv6_irrelevant.set(irrelevant).is_err() {
            self.sink.poison();
        }
    }
}

impl RouteMonitorObserver for RouteAuthorityObserver {
    fn invalidate(&self) {
        self.sink.invalidate();
    }

    fn poison(&self) {
        self.sink.poison();
    }

    fn requires_invalidation(&self, kind: NotificationKind, interfaces: &[u32]) -> bool {
        if kind != NotificationKind::Ipv6Address || interfaces.is_empty() {
            return true;
        }
        let Some(irrelevant) = self.ipv6_irrelevant.get() else {
            return true;
        };
        !interfaces
            .iter()
            .all(|index| irrelevant.contains(&InterfaceId::new(u64::from(*index))))
    }
}

/// Topology-redacted failure while preparing or starting a route observer.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum LinuxRouteObserverBridgeError {
    #[error(transparent)]
    Coordinator(#[from] ObserverCoordinatorError),
    #[error(transparent)]
    Monitor(#[from] LinuxRouteMonitorError),
    #[error("the Linux route snapshot worker was unavailable")]
    SnapshotWorkerUnavailable,
    #[error("the Linux route snapshot could not be collected")]
    SnapshotUnavailable,
    #[error("the Linux route observer could not join the async runtime")]
    ActorRuntimeUnavailable,
}

/// A subscribed route observer and the exact baseline begun before its
/// snapshot was dispatched.
///
/// This value is deliberately non-cloneable. Dropping it drops the subscribed
/// monitor, which synchronously poisons its coordinator source.
#[must_use = "a prepared route observer must be started or allowed to fail closed"]
pub(super) struct PreparedLinuxRouteObserver {
    monitor: LinuxRouteEventMonitor,
    reconciliation: mpsc::Receiver<RouteReconciliationRequired>,
    sink: Arc<RouteObserverSink>,
    baseline: RouteBaselineToken,
}

impl PreparedLinuxRouteObserver {
    /// Subscribe, begin the coordinator baseline, and collect the exact route
    /// snapshot on Tokio's blocking pool, in that order.
    ///
    /// Cancellation while the snapshot worker is running drops this value's
    /// subscribed monitor and poisons the baseline. The bounded read-only
    /// snapshot worker may finish in the background, but it retains no
    /// authority and cannot activate discovery.
    pub(super) async fn prepare(
        incarnation: &mut RoutedObserverIncarnation,
    ) -> Result<(RouteSnapshot, Self), LinuxRouteObserverBridgeError> {
        let (snapshot, (monitor, reconciliation), sink, baseline) = Self::prepare_with(
            incarnation,
            |observer| {
                let monitor_observer: Arc<dyn RouteMonitorObserver> = observer;
                LinuxRouteEventMonitor::subscribe(monitor_observer).map_err(Into::into)
            },
            || {
                LinuxRouteProvider::new().snapshot().map_err(|error| {
                    // The message names the failing rtnetlink step or the
                    // unsupported route shape; it never carries an address,
                    // prefix, or interface name.
                    tracing::warn!(reason = %error, "the Linux route snapshot could not be collected");
                })
            },
        )
        .await?;

        Ok((
            snapshot,
            Self {
                monitor,
                reconciliation,
                sink,
                baseline,
            },
        ))
    }

    /// Execute the fixed subscribe/baseline/snapshot preparation order.
    ///
    /// The injected operations keep this security-sensitive ordering
    /// deterministic under test without requiring a live rtnetlink socket.
    /// A subscription remains owned across the blocking snapshot await, so
    /// cancelling this future drops it and fails the source closed while the
    /// read-only worker is allowed to finish without authority. The collected
    /// snapshot is bound to the subscribed observer before it is returned.
    async fn prepare_with<S, Subscribe, Snapshot>(
        incarnation: &mut RoutedObserverIncarnation,
        subscribe: Subscribe,
        snapshot: Snapshot,
    ) -> Result<
        (RouteSnapshot, S, Arc<RouteObserverSink>, RouteBaselineToken),
        LinuxRouteObserverBridgeError,
    >
    where
        S: Send,
        Subscribe:
            FnOnce(Arc<RouteAuthorityObserver>) -> Result<S, LinuxRouteObserverBridgeError> + Send,
        Snapshot: FnOnce() -> Result<RouteSnapshot, ()> + Send + 'static,
    {
        let sink = Arc::new(incarnation.take_route_sink()?);
        let observer = Arc::new(RouteAuthorityObserver::new(Arc::clone(&sink)));
        let subscription = subscribe(Arc::clone(&observer))?;

        // This token brackets the blocking snapshot. It cannot be replaced by
        // one minted after collection, and start() moves it directly into the
        // monitor's no-await activation callback.
        let baseline = incarnation.begin_route_baseline()?;
        let snapshot = tokio::task::spawn_blocking(snapshot)
            .await
            .map_err(|_| LinuxRouteObserverBridgeError::SnapshotWorkerUnavailable)?
            .map_err(|()| LinuxRouteObserverBridgeError::SnapshotUnavailable)?;
        observer.bind(&snapshot);

        Ok((snapshot, subscription, sink, baseline))
    }

    /// Start continuous polling and hand the exact prepared token to one
    /// synchronous activation callback after the monitor's final clean drain.
    pub(super) fn start(
        self,
        activate: RouteActivationCallback,
    ) -> Result<LinuxRouteObserverSession, LinuxRouteObserverBridgeError> {
        let Self {
            monitor,
            reconciliation,
            sink,
            baseline,
        } = self;
        let run = monitor.run_continuously(move || activate.activate(baseline).map_err(|_| ()));
        LinuxRouteObserverSession::start_actor(sink, reconciliation, run)
    }
}

impl fmt::Debug for PreparedLinuxRouteObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedLinuxRouteObserver(<redacted>)")
    }
}

enum RouteActorExit {
    Stopped,
    Monitor(Result<(), LinuxRouteMonitorError>),
}

/// Why one live route-observer actor stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxRouteObserverTermination {
    Stopped,
    MonitorStopped,
    MonitorFailed(LinuxRouteMonitorError),
    ActorFailed,
    AlreadyTerminated,
}

/// One fail-closed event from a live route-observer session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxRouteObserverSessionEvent {
    ReconciliationRequired,
    Terminated(LinuxRouteObserverTermination),
}

/// Owns continuous route polling and its capacity-one reconciliation signal.
///
/// The controller must call [`Self::shutdown`] and await the actor before
/// installing a replacement. `Drop` is a fail-closed fallback: it poisons
/// synchronously, requests cancellation, and aborts the task so a forgotten
/// handle can never detach and retain authority.
#[must_use = "a live route observer must be driven and shut down explicitly"]
pub(super) struct LinuxRouteObserverSession {
    sink: Arc<RouteObserverSink>,
    cancellation: CancellationToken,
    reconciliation: mpsc::Receiver<RouteReconciliationRequired>,
    actor: Option<JoinHandle<RouteActorExit>>,
}

impl LinuxRouteObserverSession {
    fn start_actor<Fut>(
        sink: Arc<RouteObserverSink>,
        reconciliation: mpsc::Receiver<RouteReconciliationRequired>,
        run: Fut,
    ) -> Result<Self, LinuxRouteObserverBridgeError>
    where
        Fut: Future<Output = Result<(), LinuxRouteMonitorError>> + Send + 'static,
    {
        let handle = match Handle::try_current() {
            Ok(handle) => handle,
            Err(_) => {
                sink.poison();
                return Err(LinuxRouteObserverBridgeError::ActorRuntimeUnavailable);
            }
        };
        let cancellation = CancellationToken::new();
        let actor_cancellation = cancellation.clone();
        let actor = handle.spawn(async move {
            tokio::select! {
                biased;
                () = actor_cancellation.cancelled() => RouteActorExit::Stopped,
                result = run => RouteActorExit::Monitor(result),
            }
        });

        Ok(Self {
            sink,
            cancellation,
            reconciliation,
            actor: Some(actor),
        })
    }

    /// Wait for either a coalesced route change or terminal actor completion.
    ///
    /// The monitor invalidates authority before it sends reconciliation, so a
    /// returned change can never race ahead of revocation.
    pub(super) async fn next_event(&mut self) -> LinuxRouteObserverSessionEvent {
        let Some(actor) = self.actor.as_mut() else {
            return LinuxRouteObserverSessionEvent::Terminated(
                LinuxRouteObserverTermination::AlreadyTerminated,
            );
        };

        enum Ready {
            Reconciliation(Option<RouteReconciliationRequired>),
            Actor(Result<RouteActorExit, JoinError>),
        }

        let ready = tokio::select! {
            reconciliation = self.reconciliation.recv() => Ready::Reconciliation(reconciliation),
            outcome = actor => Ready::Actor(outcome),
        };
        match ready {
            Ready::Reconciliation(Some(RouteReconciliationRequired)) => {
                LinuxRouteObserverSessionEvent::ReconciliationRequired
            }
            Ready::Reconciliation(None) => {
                let outcome = match join_actor(&mut self.actor).await {
                    Some(outcome) => outcome,
                    None => {
                        return LinuxRouteObserverSessionEvent::Terminated(
                            LinuxRouteObserverTermination::AlreadyTerminated,
                        );
                    }
                };
                self.terminal_event(outcome)
            }
            Ready::Actor(outcome) => {
                let _ = self.actor.take();
                self.terminal_event(outcome)
            }
        }
    }

    /// Synchronously poison authority, request cancellation, and await task
    /// destruction before returning ownership to a replacement controller.
    pub(super) async fn shutdown(mut self) -> LinuxRouteObserverTermination {
        self.sink.poison();
        self.cancellation.cancel();
        let Some(outcome) = join_actor(&mut self.actor).await else {
            return LinuxRouteObserverTermination::AlreadyTerminated;
        };
        classify_actor_exit(outcome)
    }

    fn terminal_event(
        &self,
        outcome: Result<RouteActorExit, JoinError>,
    ) -> LinuxRouteObserverSessionEvent {
        // Monitor drop already poisons on ordinary completion and unwind. Keep
        // this explicit call as defense in depth for every JoinError shape.
        self.sink.poison();
        LinuxRouteObserverSessionEvent::Terminated(classify_actor_exit(outcome))
    }
}

impl Drop for LinuxRouteObserverSession {
    fn drop(&mut self) {
        self.sink.poison();
        self.cancellation.cancel();
        if let Some(actor) = self.actor.take() {
            actor.abort();
        }
    }
}

impl fmt::Debug for LinuxRouteObserverSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinuxRouteObserverSession(<redacted>)")
    }
}

fn classify_actor_exit(
    outcome: Result<RouteActorExit, JoinError>,
) -> LinuxRouteObserverTermination {
    match outcome {
        Ok(RouteActorExit::Stopped) => LinuxRouteObserverTermination::Stopped,
        Ok(RouteActorExit::Monitor(Ok(()))) => LinuxRouteObserverTermination::MonitorStopped,
        Ok(RouteActorExit::Monitor(Err(error))) => {
            LinuxRouteObserverTermination::MonitorFailed(error)
        }
        Err(_) => LinuxRouteObserverTermination::ActorFailed,
    }
}

/// Await one actor without surrendering its handle until it is complete.
///
/// If the surrounding controller future is cancelled while this await is
/// pending, the handle remains in its owning session so `Drop` can abort it.
async fn join_actor(
    actor: &mut Option<JoinHandle<RouteActorExit>>,
) -> Option<Result<RouteActorExit, JoinError>> {
    let outcome = match actor.as_mut() {
        Some(actor) => actor.await,
        None => return None,
    };
    let _completed = actor.take();
    Some(outcome)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::*;
    use crate::discovery::approval::controller::RoutedObserverCoordinator;

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct FakeRouteSubscription {
        observer: Arc<RouteAuthorityObserver>,
    }

    impl Drop for FakeRouteSubscription {
        fn drop(&mut self) {
            self.observer.poison();
        }
    }

    fn route_sink_fixture() -> (
        RoutedObserverCoordinator,
        RoutedObserverIncarnation,
        Arc<RouteObserverSink>,
    ) {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let sink = Arc::new(incarnation.take_route_sink().unwrap());
        (coordinator, incarnation, sink)
    }

    fn interface(id: u64, kind: InterfaceKind, is_up: bool) -> NetworkInterface {
        let address = match kind {
            InterfaceKind::Tunnel => "fd00:7::2/64",
            InterfaceKind::Loopback | InterfaceKind::Other => "2001:db8::10/64",
        };
        NetworkInterface::new(
            InterfaceId::new(id),
            format!("if{id}"),
            kind,
            is_up,
            [address.parse().unwrap()],
        )
    }

    /// An Ethernet link, loopback, a down link, and up and down tunnels.
    fn mixed_snapshot() -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Loopback, true),
                interface(2, InterfaceKind::Other, true),
                interface(3, InterfaceKind::Other, false),
                interface(7, InterfaceKind::Tunnel, true),
                interface(8, InterfaceKind::Tunnel, false),
            ],
            vec![],
        )
    }

    #[test]
    fn authority_declines_only_ipv6_addresses_on_bound_non_tunnel_interfaces() {
        let (_coordinator, _incarnation, sink) = route_sink_fixture();
        let observer = RouteAuthorityObserver::new(sink);
        let ipv6 = NotificationKind::Ipv6Address;

        // Before a baseline is bound nothing is known to be irrelevant.
        assert!(observer.requires_invalidation(ipv6, &[2]));

        observer.bind(&mixed_snapshot());
        assert!(!observer.requires_invalidation(ipv6, &[2]));
        assert!(!observer.requires_invalidation(ipv6, &[1, 2, 3, 2]));
        for interfaces in [&[7][..], &[8], &[2, 7], &[7, 2], &[42], &[2, 42], &[0], &[]] {
            assert!(
                observer.requires_invalidation(ipv6, interfaces),
                "{interfaces:?}"
            );
        }
        assert!(observer.requires_invalidation(NotificationKind::Ipv4Address, &[2]));
        for kind in [
            NotificationKind::Link,
            NotificationKind::Ipv4Route,
            NotificationKind::Ipv4Rule,
        ] {
            assert!(observer.requires_invalidation(kind, &[]), "{kind:?}");
        }
    }

    #[test]
    fn authority_observer_forwards_to_its_sink_and_refuses_a_second_baseline() {
        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let observer = RouteAuthorityObserver::new(sink);
        let baseline = incarnation.begin_route_baseline().unwrap();
        observer.invalidate();
        assert_eq!(
            incarnation
                .validate_route_baseline_current(&baseline)
                .unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );
        observer.bind(&mixed_snapshot());
        incarnation.begin_route_baseline().unwrap();

        observer.bind(&RouteSnapshot::default());
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        assert!(
            !observer.requires_invalidation(NotificationKind::Ipv6Address, &[2]),
            "the first baseline stays bound"
        );

        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let observer = RouteAuthorityObserver::new(sink);
        observer.poison();
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
    }

    #[tokio::test]
    async fn preparation_subscribes_then_begins_baseline_before_snapshot_dispatch() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let phase = Arc::new(AtomicUsize::new(0));
        let subscription_phase = Arc::clone(&phase);
        let snapshot_phase = Arc::clone(&phase);
        let subscription_coordinator = Arc::clone(&coordinator.inner);
        let snapshot_coordinator = Arc::clone(&coordinator.inner);
        let subscribed = Arc::new(Mutex::new(None));
        let subscribed_observer = Arc::clone(&subscribed);
        let unbound_observer = Arc::clone(&subscribed);

        let (snapshot, subscription, sink, _baseline) = PreparedLinuxRouteObserver::prepare_with(
            &mut incarnation,
            move |observer| {
                assert_eq!(
                    subscription_coordinator
                        .state
                        .lock()
                        .unwrap()
                        .route_generation,
                    0,
                    "subscription must precede the route baseline",
                );
                assert_eq!(
                    subscription_phase.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst,),
                    Ok(0),
                );
                *subscribed_observer.lock().unwrap() = Some(Arc::clone(&observer));
                Ok(FakeRouteSubscription { observer })
            },
            move || {
                assert_eq!(
                    snapshot_coordinator.state.lock().unwrap().route_generation,
                    1,
                    "the route baseline must precede snapshot dispatch",
                );
                assert_eq!(
                    snapshot_phase.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst,),
                    Ok(1),
                );
                let observer = unbound_observer.lock().unwrap().clone().unwrap();
                assert!(
                    observer.requires_invalidation(NotificationKind::Ipv6Address, &[2]),
                    "the observer is unbound while its snapshot is collected",
                );
                Ok(mixed_snapshot())
            },
        )
        .await
        .unwrap();

        assert_eq!(phase.load(Ordering::SeqCst), 2);
        assert_eq!(snapshot, mixed_snapshot());
        let observer = subscribed.lock().unwrap().take().unwrap();
        assert!(
            !observer.requires_invalidation(NotificationKind::Ipv6Address, &[2]),
            "the collected snapshot is bound before it is returned",
        );
        assert!(observer.requires_invalidation(NotificationKind::Ipv6Address, &[7]));
        drop(observer);
        drop(subscription);
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        drop(sink);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_snapshot_worker_fails_closed_and_worker_finishes_inertly() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let retained_observer = Arc::new(Mutex::new(None));
        let subscribed_observer = Arc::clone(&retained_observer);
        let (started_sender, started) = oneshot::channel();
        let (release_sender, release) = std::sync::mpsc::channel();
        let (finished_sender, finished) = oneshot::channel();

        let mut preparation = Box::pin(PreparedLinuxRouteObserver::prepare_with(
            &mut incarnation,
            move |observer| {
                *subscribed_observer.lock().unwrap() = Some(Arc::clone(&observer));
                Ok(FakeRouteSubscription { observer })
            },
            move || {
                let _ = started_sender.send(());
                release.recv().unwrap();
                let _ = finished_sender.send(());
                Ok(RouteSnapshot::default())
            },
        ));

        tokio::select! {
            result = &mut preparation => {
                panic!("snapshot preparation completed before cancellation: {}", result.is_ok());
            }
            result = started => result.unwrap(),
        }
        drop(preparation);

        assert!(retained_observer.lock().unwrap().is_some());
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned,
            "dropping preparation must poison before the worker is released",
        );

        release_sender.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), finished)
            .await
            .expect("the read-only snapshot worker did not finish")
            .expect("the read-only snapshot worker dropped its completion signal");
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned,
            "a detached read-only result must not restore authority",
        );
    }

    #[test]
    fn actor_start_without_a_runtime_fails_closed_without_panicking() {
        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let _sink_lifetime = Arc::clone(&sink);
        let (_reconciliation_sender, reconciliation) = mpsc::channel(1);
        let run = std::future::pending::<Result<(), LinuxRouteMonitorError>>();

        assert_eq!(
            LinuxRouteObserverSession::start_actor(sink, reconciliation, run).unwrap_err(),
            LinuxRouteObserverBridgeError::ActorRuntimeUnavailable
        );
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
    }

    #[tokio::test]
    async fn shutdown_poisons_then_awaits_actor_destruction() {
        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let _sink_lifetime = Arc::clone(&sink);
        let (_reconciliation_sender, reconciliation) = mpsc::channel(1);
        let dropped = Arc::new(AtomicBool::new(false));
        let actor_dropped = Arc::clone(&dropped);
        let (started_sender, started) = oneshot::channel();
        let run = async move {
            let _drop_marker = DropMarker(actor_dropped);
            let _ = started_sender.send(());
            std::future::pending::<Result<(), LinuxRouteMonitorError>>().await
        };
        let session = LinuxRouteObserverSession::start_actor(sink, reconciliation, run).unwrap();
        started.await.unwrap();

        assert_eq!(
            session.shutdown().await,
            LinuxRouteObserverTermination::Stopped
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_join_keeps_the_actor_handle_owned() {
        let mut actor = Some(tokio::spawn(async {
            std::future::pending::<RouteActorExit>().await
        }));
        let mut join = Box::pin(join_actor(&mut actor));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(join.as_mut().poll(&mut context), Poll::Pending));
        drop(join);
        let actor = actor.take().expect("cancellation must retain the handle");
        actor.abort();
        assert!(matches!(actor.await, Err(error) if error.is_cancelled()));
    }

    #[tokio::test]
    async fn drop_poisons_synchronously_and_aborts_as_a_fallback() {
        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let _sink_lifetime = Arc::clone(&sink);
        let (_reconciliation_sender, reconciliation) = mpsc::channel(1);
        let dropped = Arc::new(AtomicBool::new(false));
        let actor_dropped = Arc::clone(&dropped);
        let (started_sender, started) = oneshot::channel();
        let run = async move {
            let _drop_marker = DropMarker(actor_dropped);
            let _ = started_sender.send(());
            std::future::pending::<Result<(), LinuxRouteMonitorError>>().await
        };
        let session = LinuxRouteObserverSession::start_actor(sink, reconciliation, run).unwrap();
        started.await.unwrap();

        drop(session);
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the aborted route actor must be destroyed");
    }

    #[tokio::test]
    async fn reconciliation_is_capacity_one_and_does_not_stop_the_actor() {
        let (_coordinator, _incarnation, sink) = route_sink_fixture();
        let (reconciliation_sender, reconciliation) = mpsc::channel(1);
        let run = std::future::pending::<Result<(), LinuxRouteMonitorError>>();
        let mut session =
            LinuxRouteObserverSession::start_actor(sink, reconciliation, run).unwrap();

        reconciliation_sender
            .try_send(RouteReconciliationRequired)
            .unwrap();
        assert!(matches!(
            reconciliation_sender.try_send(RouteReconciliationRequired),
            Err(mpsc::error::TrySendError::Full(RouteReconciliationRequired))
        ));
        assert_eq!(
            session.next_event().await,
            LinuxRouteObserverSessionEvent::ReconciliationRequired
        );
        assert_eq!(
            session.shutdown().await,
            LinuxRouteObserverTermination::Stopped
        );
    }

    #[tokio::test]
    async fn terminal_actor_failure_is_reported_and_poisons() {
        let (_coordinator, incarnation, sink) = route_sink_fixture();
        let (_reconciliation_sender, reconciliation) = mpsc::channel(1);
        let run = async { Err(LinuxRouteMonitorError::ReceiveFailed) };
        let mut session =
            LinuxRouteObserverSession::start_actor(sink, reconciliation, run).unwrap();

        assert_eq!(
            session.next_event().await,
            LinuxRouteObserverSessionEvent::Terminated(
                LinuxRouteObserverTermination::MonitorFailed(LinuxRouteMonitorError::ReceiveFailed)
            )
        );
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        assert_eq!(
            session.shutdown().await,
            LinuxRouteObserverTermination::AlreadyTerminated
        );
    }
}
