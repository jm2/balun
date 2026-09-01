//! Linux approval-store observer ownership and exact-reread handoff.
//!
//! A prepared observer subscribes to the store's pinned directory before it
//! begins the coordinator baseline and before it dispatches the caller's exact
//! reread to a blocking worker. The live session owns the observer actor, its
//! store sink, and a capacity-one reconciliation signal. No store event is
//! suppressed: invalidation is synchronous and precedes reconciliation.

#![cfg(target_os = "linux")]

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::discovery::approval::store::ApprovalStore;
use crate::discovery::approval::watch::{
    LinuxApprovalStoreObserver, StoreBaselineError, StoreBaselineProof, StoreInvalidationSink,
    StoreWatchError, subscribe_linux,
};

use super::activation::StoreActivationCallback;
use super::{
    ObserverCoordinatorError, RoutedObserverIncarnation, StoreBaselineToken, StoreObserverSink,
};

const RECONCILIATION_CAPACITY: usize = 1;

/// One coalesced request to replace and rebaseline the store observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StoreReconciliationRequired;

/// Topology-redacted failure while preparing or starting store observation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum LinuxStoreObserverBridgeError {
    #[error(transparent)]
    Coordinator(#[from] ObserverCoordinatorError),
    #[error(transparent)]
    Watch(#[from] StoreWatchError),
    #[error("the approval-store observer blocking worker was unavailable")]
    BlockingWorkerUnavailable,
    #[error("the exact approval-store reread was rejected")]
    ExactRereadRejected,
    #[error("the approval-store observer could not join the async runtime")]
    ActorRuntimeUnavailable,
}

struct StoreSourceState {
    sink: Option<StoreObserverSink>,
}

struct StoreSourceInner {
    state: Mutex<StoreSourceState>,
}

/// Exclusive poison control retained by the prepared/live session.
struct StoreSourceControl {
    inner: Arc<StoreSourceInner>,
}

impl StoreSourceControl {
    fn new(
        sink: StoreObserverSink,
    ) -> (
        Self,
        StoreObserverInvalidator,
        mpsc::Receiver<StoreReconciliationRequired>,
    ) {
        let inner = Arc::new(StoreSourceInner {
            state: Mutex::new(StoreSourceState { sink: Some(sink) }),
        });
        let (reconciliation, receiver) = mpsc::channel(RECONCILIATION_CAPACITY);
        (
            Self {
                inner: Arc::clone(&inner),
            },
            StoreObserverInvalidator {
                inner,
                reconciliation,
            },
            receiver,
        )
    }

    fn handle(&self) -> StoreSourceHandle {
        StoreSourceHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    fn poison(&self) {
        poison_store_source(&self.inner);
    }
}

impl Drop for StoreSourceControl {
    fn drop(&mut self) {
        self.poison();
    }
}

impl fmt::Debug for StoreSourceControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreSourceControl(<redacted>)")
    }
}

#[derive(Clone)]
struct StoreSourceHandle {
    inner: Arc<StoreSourceInner>,
}

impl StoreSourceHandle {
    fn poison(&self) {
        poison_store_source(&self.inner);
    }
}

struct StoreObserverInvalidator {
    inner: Arc<StoreSourceInner>,
    reconciliation: mpsc::Sender<StoreReconciliationRequired>,
}

impl StoreInvalidationSink for StoreObserverInvalidator {
    fn invalidate(&self) {
        let mut retired = None;
        match self.inner.state.lock() {
            Ok(mut state) => {
                let Some(sink) = state.sink.as_ref() else {
                    return;
                };

                // Authority is revoked before reconciliation is published.
                StoreInvalidationSink::invalidate(sink);
                match self.reconciliation.try_send(StoreReconciliationRequired) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        retired = state.sink.take();
                    }
                }
            }
            Err(poisoned) => {
                retired = poisoned.into_inner().sink.take();
            }
        }

        drop(retired);
    }
}

impl fmt::Debug for StoreObserverInvalidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreObserverInvalidator(<redacted>)")
    }
}

fn poison_store_source(inner: &StoreSourceInner) {
    let retired = match inner.state.lock() {
        Ok(mut state) => state.sink.take(),
        Err(poisoned) => poisoned.into_inner().sink.take(),
    };
    // StoreObserverSink::drop marks this source poisoned synchronously.
    drop(retired);
}

enum BaselineWorkerError {
    Observation(StoreWatchError),
    ExactRereadRejected,
}

/// A subscribed watcher and the exact coordinator baseline begun before its
/// reread was dispatched.
///
/// Dropping this value drops its exclusive source control and synchronously
/// poisons the store half of the combined incarnation.
#[must_use = "a prepared store observer must be started or allowed to fail closed"]
pub(super) struct PreparedLinuxStoreObserver {
    observer: LinuxApprovalStoreObserver<StoreObserverInvalidator>,
    proof: StoreBaselineProof,
    source: StoreSourceControl,
    reconciliation: mpsc::Receiver<StoreReconciliationRequired>,
    baseline: StoreBaselineToken,
}

impl PreparedLinuxStoreObserver {
    /// Subscribe, begin the coordinator baseline, and execute one exact reread.
    ///
    /// Subscription and the complete watcher/read/watcher sandwich run on
    /// Tokio's blocking pool. Cancellation drops the source control at once;
    /// a blocking operation which has already started may finish later, but it
    /// no longer owns a live coordinator sink and cannot activate authority.
    pub(super) async fn prepare<R, E, F>(
        incarnation: &mut RoutedObserverIncarnation,
        store: Arc<ApprovalStore>,
        exact_reread: F,
    ) -> Result<(R, Self), LinuxStoreObserverBridgeError>
    where
        R: Send + 'static,
        E: Send + 'static,
        F: FnOnce(&ApprovalStore) -> Result<R, E> + Send + 'static,
    {
        let sink = incarnation.take_store_sink()?;
        let (source, invalidator, reconciliation) = StoreSourceControl::new(sink);

        let subscribe_store = Arc::clone(&store);
        let observer =
            tokio::task::spawn_blocking(move || subscribe_linux(&subscribe_store, invalidator))
                .await
                .map_err(|_| LinuxStoreObserverBridgeError::BlockingWorkerUnavailable)??;

        // The coordinator token is minted only after subscription and before
        // the watcher's own pre-read drain and the exact store reread.
        let baseline = incarnation.begin_store_baseline()?;
        let outcome = tokio::task::spawn_blocking(move || {
            let mut observer = observer;
            match observer.revalidate_with(|| exact_reread(&store)) {
                Ok((value, proof)) => Ok((value, observer, proof)),
                Err(StoreBaselineError::Observation(error)) => {
                    Err(BaselineWorkerError::Observation(error))
                }
                Err(StoreBaselineError::Revalidation(_)) => {
                    Err(BaselineWorkerError::ExactRereadRejected)
                }
            }
        })
        .await
        .map_err(|_| LinuxStoreObserverBridgeError::BlockingWorkerUnavailable)?;

        let (value, observer, proof) = match outcome {
            Ok(parts) => parts,
            Err(BaselineWorkerError::Observation(error)) => return Err(error.into()),
            Err(BaselineWorkerError::ExactRereadRejected) => {
                return Err(LinuxStoreObserverBridgeError::ExactRereadRejected);
            }
        };
        incarnation.validate_store_baseline_current(&baseline)?;

        Ok((
            value,
            Self {
                observer,
                proof,
                source,
                reconciliation,
                baseline,
            },
        ))
    }

    /// Start continuous polling and offer the prepared token only from the
    /// watcher's synchronous final-drain callback.
    pub(super) fn start(
        self,
        activation: StoreActivationCallback,
    ) -> Result<LinuxStoreObserverSession, LinuxStoreObserverBridgeError> {
        let Self {
            observer,
            proof,
            source,
            reconciliation,
            baseline,
        } = self;
        let run = observer.run_continuously(proof, move |_invalidator| {
            activation.activate(baseline).map_err(|_| ())
        });
        LinuxStoreObserverSession::start_actor(source, reconciliation, run)
    }
}

impl fmt::Debug for PreparedLinuxStoreObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedLinuxStoreObserver(<redacted>)")
    }
}

enum StoreActorExit {
    Stopped,
    WatchFailed(StoreWatchError),
}

/// Why one live approval-store observer actor stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxStoreObserverTermination {
    Stopped,
    WatchFailed(StoreWatchError),
    ActorFailed,
    AlreadyTerminated,
}

/// One fail-closed event from a live approval-store observer session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxStoreObserverSessionEvent {
    ReconciliationRequired,
    Terminated(LinuxStoreObserverTermination),
}

/// Owns continuous store polling, source poisoning, and reconciliation.
///
/// The controller must consume [`Self::shutdown`] and await actor destruction
/// before installing a replacement. `Drop` is the fail-closed fallback.
#[must_use = "a live store observer must be driven and shut down explicitly"]
pub(super) struct LinuxStoreObserverSession {
    source: StoreSourceControl,
    cancellation: CancellationToken,
    reconciliation: mpsc::Receiver<StoreReconciliationRequired>,
    actor: Option<JoinHandle<StoreActorExit>>,
}

impl LinuxStoreObserverSession {
    fn start_actor<Fut>(
        source: StoreSourceControl,
        reconciliation: mpsc::Receiver<StoreReconciliationRequired>,
        run: Fut,
    ) -> Result<Self, LinuxStoreObserverBridgeError>
    where
        Fut: Future<Output = Result<Infallible, StoreWatchError>> + Send + 'static,
    {
        let handle = match Handle::try_current() {
            Ok(handle) => handle,
            Err(_) => {
                source.poison();
                return Err(LinuxStoreObserverBridgeError::ActorRuntimeUnavailable);
            }
        };
        let cancellation = CancellationToken::new();
        let actor_cancellation = cancellation.clone();
        let actor_source = source.handle();
        let actor = handle.spawn(async move {
            let _poison_on_exit = StoreActorPoisonGuard(actor_source);
            tokio::select! {
                biased;
                () = actor_cancellation.cancelled() => StoreActorExit::Stopped,
                result = run => match result {
                    Err(error) => StoreActorExit::WatchFailed(error),
                    Ok(never) => match never {},
                },
            }
        });

        Ok(Self {
            source,
            cancellation,
            reconciliation,
            actor: Some(actor),
        })
    }

    /// Wait for a coalesced store change or terminal actor completion.
    pub(super) async fn next_event(&mut self) -> LinuxStoreObserverSessionEvent {
        let Some(actor) = self.actor.as_mut() else {
            return LinuxStoreObserverSessionEvent::Terminated(
                LinuxStoreObserverTermination::AlreadyTerminated,
            );
        };

        enum Ready {
            Reconciliation(Option<StoreReconciliationRequired>),
            Actor(Result<StoreActorExit, JoinError>),
        }

        let ready = tokio::select! {
            reconciliation = self.reconciliation.recv() => Ready::Reconciliation(reconciliation),
            outcome = actor => Ready::Actor(outcome),
        };
        match ready {
            Ready::Reconciliation(Some(StoreReconciliationRequired)) => {
                LinuxStoreObserverSessionEvent::ReconciliationRequired
            }
            Ready::Reconciliation(None) => {
                let outcome = match join_actor(&mut self.actor).await {
                    Some(outcome) => outcome,
                    None => {
                        return LinuxStoreObserverSessionEvent::Terminated(
                            LinuxStoreObserverTermination::AlreadyTerminated,
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

    /// Poison first, request cancellation, and await actor destruction.
    pub(super) async fn shutdown(mut self) -> LinuxStoreObserverTermination {
        self.source.poison();
        self.cancellation.cancel();
        let Some(outcome) = join_actor(&mut self.actor).await else {
            return LinuxStoreObserverTermination::AlreadyTerminated;
        };
        classify_actor_exit(outcome)
    }

    fn terminal_event(
        &self,
        outcome: Result<StoreActorExit, JoinError>,
    ) -> LinuxStoreObserverSessionEvent {
        self.source.poison();
        LinuxStoreObserverSessionEvent::Terminated(classify_actor_exit(outcome))
    }
}

impl Drop for LinuxStoreObserverSession {
    fn drop(&mut self) {
        self.source.poison();
        self.cancellation.cancel();
        if let Some(actor) = self.actor.take() {
            actor.abort();
        }
    }
}

impl fmt::Debug for LinuxStoreObserverSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinuxStoreObserverSession(<redacted>)")
    }
}

struct StoreActorPoisonGuard(StoreSourceHandle);

impl Drop for StoreActorPoisonGuard {
    fn drop(&mut self) {
        self.0.poison();
    }
}

fn classify_actor_exit(
    outcome: Result<StoreActorExit, JoinError>,
) -> LinuxStoreObserverTermination {
    match outcome {
        Ok(StoreActorExit::Stopped) => LinuxStoreObserverTermination::Stopped,
        Ok(StoreActorExit::WatchFailed(error)) => LinuxStoreObserverTermination::WatchFailed(error),
        Err(_) => LinuxStoreObserverTermination::ActorFailed,
    }
}

/// Await one actor without surrendering its handle until it is complete.
///
/// If the surrounding controller future is cancelled while this await is
/// pending, the handle remains in its owning session so `Drop` can abort it.
async fn join_actor(
    actor: &mut Option<JoinHandle<StoreActorExit>>,
) -> Option<Result<StoreActorExit, JoinError>> {
    let outcome = match actor.as_mut() {
        Some(actor) => actor.await,
        None => return None,
    };
    let _completed = actor.take();
    Some(outcome)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::super::activation::PairedObserverActivationError;
    use super::*;
    use crate::discovery::approval::controller::RoutedObserverCoordinator;
    use crate::discovery::approval::store::{ApprovalStoreStatus, STATE_FILE_NAME, StorePaths};

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn store(temporary: &tempfile::TempDir) -> (Arc<ApprovalStore>, PathBuf) {
        let directory = temporary.path().join("store");
        let store = Arc::new(ApprovalStore::new(StorePaths::new(directory.clone())));
        // Initialize the permanent lock before observation. Its first CREATE
        // is itself an observed store change and therefore belongs outside a
        // healthy baseline.
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Missing {
                key_initialized: false,
            }
        );
        (store, directory)
    }

    fn source_fixture() -> (
        RoutedObserverCoordinator,
        RoutedObserverIncarnation,
        StoreSourceControl,
        StoreObserverInvalidator,
        mpsc::Receiver<StoreReconciliationRequired>,
    ) {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let sink = incarnation.take_store_sink().unwrap();
        let (source, invalidator, reconciliation) = StoreSourceControl::new(sink);
        (
            coordinator,
            incarnation,
            source,
            invalidator,
            reconciliation,
        )
    }

    #[test]
    fn invalidation_precedes_capacity_one_reconciliation() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route = Arc::new(incarnation.take_route_sink().unwrap());
        let store = incarnation.take_store_sink().unwrap();
        let (_source, invalidator, mut reconciliation) = StoreSourceControl::new(store);
        let epoch = incarnation
            .activate(
                incarnation.begin_route_baseline().unwrap(),
                incarnation.begin_store_baseline().unwrap(),
            )
            .unwrap();
        let registration = coordinator.register(&epoch).unwrap();

        StoreInvalidationSink::invalidate(&invalidator);
        StoreInvalidationSink::invalidate(&invalidator);
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());
        assert_eq!(reconciliation.try_recv(), Ok(StoreReconciliationRequired));
        assert!(matches!(
            reconciliation.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(route);
    }

    #[test]
    fn a_closed_reconciler_poisons_the_store_source() {
        let (_coordinator, incarnation, _source, invalidator, reconciliation) = source_fixture();
        drop(reconciliation);
        StoreInvalidationSink::invalidate(&invalidator);
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[test]
    fn a_poisoned_source_mutex_drops_authority_without_reconciliation() {
        let (_coordinator, incarnation, source, invalidator, mut reconciliation) = source_fixture();
        let inner = Arc::clone(&source.inner);
        let poisoned = std::panic::catch_unwind(move || {
            let _guard = inner.state.lock().unwrap();
            panic!("poison the store-source test mutex");
        });
        assert!(poisoned.is_err());

        StoreInvalidationSink::invalidate(&invalidator);
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
        assert!(matches!(
            reconciliation.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_reread_final_drain_and_activation_form_one_live_session() {
        let temporary = tempfile::tempdir().unwrap();
        let (store, directory) = store(&temporary);
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route = Arc::new(incarnation.take_route_sink().unwrap());
        let route_baseline = incarnation.begin_route_baseline().unwrap();

        let (status, prepared) =
            PreparedLinuxStoreObserver::prepare(&mut incarnation, Arc::clone(&store), |store| {
                store.load()
            })
            .await
            .unwrap();
        assert_eq!(
            status,
            ApprovalStoreStatus::Missing {
                key_initialized: false,
            }
        );

        let (mut activation, route_activation, store_activation) =
            incarnation.into_paired_activation();
        route_activation.activate(route_baseline).unwrap();
        let mut session = prepared.start(store_activation).unwrap();
        let epoch = tokio::time::timeout(Duration::from_secs(2), activation.take_epoch())
            .await
            .expect("store actor did not activate")
            .unwrap();
        let registration = coordinator.register(&epoch).unwrap();

        std::fs::write(directory.join(STATE_FILE_NAME), b"changed").unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), session.next_event())
                .await
                .expect("store actor did not reconcile"),
            LinuxStoreObserverSessionEvent::ReconciliationRequired
        );
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());
        assert_eq!(
            session.shutdown().await,
            LinuxStoreObserverTermination::Stopped
        );
        drop(route);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn event_during_exact_reread_rejects_the_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let (store, directory) = store(&temporary);
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();

        let error = PreparedLinuxStoreObserver::prepare(&mut incarnation, store, move |store| {
            let status = store.load()?;
            std::fs::write(directory.join(STATE_FILE_NAME), b"changed").unwrap();
            Ok::<_, crate::discovery::approval::store::StoreError>(status)
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            LinuxStoreObserverBridgeError::Watch(StoreWatchError::ChangedDuringBaseline)
        ));
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_a_blocked_reread_poisons_before_the_worker_finishes() {
        struct WorkerCompletion(Option<oneshot::Sender<()>>);

        impl Drop for WorkerCompletion {
            fn drop(&mut self) {
                if let Some(completed) = self.0.take() {
                    let _ = completed.send(());
                }
            }
        }

        let temporary = tempfile::tempdir().unwrap();
        let (store, _directory) = store(&temporary);
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();
        let (entered_sender, mut entered) = oneshot::channel();
        let (completed_sender, completed) = oneshot::channel();
        let (release_sender, release) = std::sync::mpsc::channel();

        let mut preparation = Box::pin(PreparedLinuxStoreObserver::prepare(
            &mut incarnation,
            store,
            move |store| {
                let _ = entered_sender.send(());
                release.recv().unwrap();
                let _ = store.load()?;
                Ok::<_, crate::discovery::approval::store::StoreError>(WorkerCompletion(Some(
                    completed_sender,
                )))
            },
        ));
        tokio::select! {
            _result = &mut preparation => panic!("blocked reread completed unexpectedly"),
            entered_result = &mut entered => entered_result.unwrap(),
        }

        drop(preparation);
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );

        release_sender.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), completed)
            .await
            .expect("detached blocking worker did not finish")
            .unwrap();
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_a_prepared_observer_poisons_its_store_source() {
        let temporary = tempfile::tempdir().unwrap();
        let (store, _directory) = store(&temporary);
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();
        let (_status, prepared) =
            PreparedLinuxStoreObserver::prepare(&mut incarnation, store, |store| store.load())
                .await
                .unwrap();

        drop(prepared);
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exact_reread_error_is_not_retained_or_rendered() {
        let temporary = tempfile::tempdir().unwrap();
        let (store, _directory) = store(&temporary);
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();

        let secret = "/private/key-material/site-a".to_owned();
        let error = PreparedLinuxStoreObserver::prepare(&mut incarnation, store, move |_store| {
            Err::<ApprovalStoreStatus, _>(secret)
        })
        .await
        .unwrap_err();
        assert_eq!(error, LinuxStoreObserverBridgeError::ExactRereadRejected);
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("private"));
        assert!(!rendered.contains("key-material"));
        assert!(!rendered.contains("site-a"));
    }

    #[test]
    fn actor_start_without_a_runtime_fails_closed_without_panicking() {
        let (_coordinator, incarnation, source, invalidator, reconciliation) = source_fixture();
        let run = async move {
            let _invalidator = invalidator;
            std::future::pending::<Result<Infallible, StoreWatchError>>().await
        };

        assert_eq!(
            LinuxStoreObserverSession::start_actor(source, reconciliation, run).unwrap_err(),
            LinuxStoreObserverBridgeError::ActorRuntimeUnavailable
        );
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[tokio::test]
    async fn shutdown_poisons_then_awaits_actor_destruction() {
        let (_coordinator, incarnation, source, invalidator, reconciliation) = source_fixture();
        let dropped = Arc::new(AtomicBool::new(false));
        let actor_dropped = Arc::clone(&dropped);
        let (started_sender, started) = oneshot::channel();
        let run = async move {
            let _invalidator = invalidator;
            let _drop_marker = DropMarker(actor_dropped);
            let _ = started_sender.send(());
            std::future::pending::<Result<Infallible, StoreWatchError>>().await
        };
        let session = LinuxStoreObserverSession::start_actor(source, reconciliation, run).unwrap();
        started.await.unwrap();

        assert_eq!(
            session.shutdown().await,
            LinuxStoreObserverTermination::Stopped
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_join_keeps_the_actor_handle_owned() {
        let mut actor = Some(tokio::spawn(async {
            std::future::pending::<StoreActorExit>().await
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
        let (_coordinator, incarnation, source, invalidator, reconciliation) = source_fixture();
        let dropped = Arc::new(AtomicBool::new(false));
        let actor_dropped = Arc::clone(&dropped);
        let (started_sender, started) = oneshot::channel();
        let run = async move {
            let _invalidator = invalidator;
            let _drop_marker = DropMarker(actor_dropped);
            let _ = started_sender.send(());
            std::future::pending::<Result<Infallible, StoreWatchError>>().await
        };
        let session = LinuxStoreObserverSession::start_actor(source, reconciliation, run).unwrap();
        started.await.unwrap();

        drop(session);
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the aborted store actor must be destroyed");
    }

    #[tokio::test]
    async fn terminal_actor_failure_is_reported_and_poisons() {
        let (_coordinator, incarnation, source, invalidator, reconciliation) = source_fixture();
        let run = async move {
            let _invalidator = invalidator;
            Err(StoreWatchError::FailedClosed)
        };
        let mut session =
            LinuxStoreObserverSession::start_actor(source, reconciliation, run).unwrap();

        assert_eq!(
            session.next_event().await,
            LinuxStoreObserverSessionEvent::Terminated(LinuxStoreObserverTermination::WatchFailed(
                StoreWatchError::FailedClosed
            ))
        );
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
        assert_eq!(
            session.shutdown().await,
            LinuxStoreObserverTermination::AlreadyTerminated
        );
    }

    #[test]
    fn debug_and_errors_are_topology_and_key_redacted() {
        let (_coordinator, _incarnation, source, invalidator, _reconciliation) = source_fixture();
        let rendered = format!(
            "{source:?} {invalidator:?} {:?} {}",
            LinuxStoreObserverBridgeError::ExactRereadRejected,
            LinuxStoreObserverBridgeError::ExactRereadRejected,
        );
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("192.168"));
        assert!(!rendered.contains("routed-approvals"));
        assert!(!rendered.contains("key"));
    }

    #[test]
    fn activation_error_is_topology_free() {
        let error = PairedObserverActivationError::OwnerDropped;
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains('/'));
        assert!(!rendered.contains("192.168"));
    }
}
