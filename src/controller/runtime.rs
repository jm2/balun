//! Packet-free controller-thread ownership and command admission.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::Instant;

use thiserror::Error;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    ApplicationSnapshot, DeviceSummary, DiscoveryFailure, DiscoveryState, DiscoveryStatus,
    OperationGeneration, SelectedLineupState, SnapshotRevision, StateError,
};
use crate::discovery::{
    DeviceRegistry, DiscoveryClient, DiscoveryError, DiscoveryReport, RegistryInstant,
};
use crate::domain::DeviceId;

/// Name assigned to Balun's GTK-independent controller thread.
pub const CONTROLLER_THREAD_NAME: &str = "balun-controller";
/// Default upper bound for commands waiting to enter the controller actor.
pub const DEFAULT_COMMAND_CAPACITY: usize = 8;
/// Largest command queue accepted by the controller constructor.
pub const MAX_COMMAND_CAPACITY: usize = 1_024;

/// Owned, `'static` future returned by an injected local-discovery service.
pub type LocalDiscoveryFuture =
    Pin<Box<dyn Future<Output = Result<DiscoveryReport, DiscoveryFailure>> + Send + 'static>>;

/// Async local discovery behind a packet-free controller boundary.
///
/// The controller invokes this service only after admitting an explicit
/// [`ControllerCommand::RefreshLocalDiscovery`] command. Implementations must
/// observe `cancellation` promptly. In particular, constructing the service or
/// controller must not enumerate interfaces, open sockets, or send packets.
pub trait LocalDiscoveryService: Send + Sync + 'static {
    fn discover_local(&self, cancellation: CancellationToken) -> LocalDiscoveryFuture;
}

impl LocalDiscoveryService for DiscoveryClient {
    fn discover_local(&self, cancellation: CancellationToken) -> LocalDiscoveryFuture {
        let client = self.clone();
        Box::pin(async move {
            DiscoveryClient::discover_local(&client, &cancellation)
                .await
                .map_err(discovery_failure)
        })
    }
}

fn discovery_failure(error: DiscoveryError) -> DiscoveryFailure {
    match error {
        DiscoveryError::Interfaces(_) => DiscoveryFailure::InterfaceEnumeration,
        DiscoveryError::Io { .. } | DiscoveryError::ShortSend { .. } => DiscoveryFailure::Network,
        DiscoveryError::InvalidEndpoint { .. }
        | DiscoveryError::Task(_)
        | DiscoveryError::RoutedScanDeadline { .. }
        | DiscoveryError::Cancelled
        | DiscoveryError::Protocol(_) => DiscoveryFailure::Internal,
    }
}

/// Bounded commands accepted by the controller actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ControllerCommand {
    /// Supersede any current local refresh and run ordinary local discovery.
    RefreshLocalDiscovery,
    /// Cancel a current local refresh without discarding last-good devices.
    CancelLocalDiscovery,
    /// Reserved until selected-device inspection is integrated.
    SelectDevice(DeviceId),
    /// Reserved until selected-device inspection is integrated.
    ClearSelection,
}

/// Immediate result of trying to admit a controller command.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ControllerCommandError {
    #[error("controller command queue is full")]
    Full,
    #[error("controller is shutting down")]
    ShuttingDown,
    #[error("selected-device commands are not available yet")]
    Unsupported,
}

/// Failure while constructing the controller thread and runtime.
#[derive(Debug, Error)]
pub enum ControllerStartError {
    #[error("controller command capacity must be between 1 and {maximum}; got {value}")]
    InvalidCommandCapacity { value: usize, maximum: usize },
    #[error("failed to spawn the controller thread: {0}")]
    ThreadSpawn(#[source] io::Error),
    #[error("failed to construct the controller Tokio runtime: {0}")]
    Runtime(#[source] io::Error),
    #[error("controller thread exited before its readiness handshake")]
    ReadinessChannelClosed,
    #[error("controller thread panicked before its readiness handshake")]
    ReadinessThreadPanicked,
}

/// Internal invariant failure reported when joining the controller.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ControllerRuntimeError {
    #[error("application snapshot revision is exhausted")]
    SnapshotRevisionExhausted,
    #[error("local-discovery generation is exhausted")]
    DiscoveryGenerationExhausted,
    #[error(transparent)]
    InvalidSnapshot(#[from] StateError),
}

/// Failure observed while deterministically joining the controller thread.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ControllerJoinError {
    #[error("controller thread panicked")]
    ThreadPanicked,
    #[error(transparent)]
    Runtime(#[from] ControllerRuntimeError),
}

/// Cloneable, nonblocking ingress to Balun's GTK-independent controller actor.
#[derive(Clone)]
pub struct ControllerHandle {
    commands: mpsc::Sender<ControllerCommand>,
    shutdown: CancellationToken,
    snapshots: watch::Receiver<Arc<ApplicationSnapshot>>,
}

/// Unique owner of the controller thread and its deterministic shutdown.
///
/// Keep this owner outside GTK callback reference cycles. Callback code may
/// clone [`Self::handle`]. When the application closes, call
/// [`Self::begin_shutdown`] synchronously, then move the owner to a worker that
/// calls [`Self::join`] if blocking the GLib thread is undesirable. `Drop`
/// provides the same cancel-and-join guarantee as a final fallback.
pub struct ControllerRuntime {
    handle: ControllerHandle,
    thread: Option<thread::JoinHandle<Result<(), ControllerRuntimeError>>>,
}

impl ControllerRuntime {
    /// Start an inert controller with Balun's production local-discovery
    /// client. No discovery work begins until an explicit refresh command.
    pub fn start_default() -> Result<Self, ControllerStartError> {
        Self::start(DiscoveryClient::default())
    }

    /// Start an inert controller with the default bounded command capacity.
    pub fn start<S>(service: S) -> Result<Self, ControllerStartError>
    where
        S: LocalDiscoveryService,
    {
        Self::start_with_capacity(service, DEFAULT_COMMAND_CAPACITY)
    }

    /// Start an inert controller with an explicit bounded command capacity.
    pub fn start_with_capacity<S>(
        service: S,
        command_capacity: usize,
    ) -> Result<Self, ControllerStartError>
    where
        S: LocalDiscoveryService,
    {
        if !(1..=MAX_COMMAND_CAPACITY).contains(&command_capacity) {
            return Err(ControllerStartError::InvalidCommandCapacity {
                value: command_capacity,
                maximum: MAX_COMMAND_CAPACITY,
            });
        }

        let service: Arc<dyn LocalDiscoveryService> = Arc::new(service);
        let (command_sender, command_receiver) = mpsc::channel(command_capacity);
        let shutdown = CancellationToken::new();
        let (snapshot_sender, snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        let (ready_sender, ready_receiver) = std_mpsc::sync_channel(1);
        let actor_shutdown = shutdown.clone();

        let controller_thread = thread::Builder::new()
            .name(CONTROLLER_THREAD_NAME.to_owned())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return Ok(());
                    }
                };
                let actor = ControllerActor::new(
                    service,
                    command_receiver,
                    actor_shutdown,
                    snapshot_sender,
                );
                if ready_sender.send(Ok(())).is_err() {
                    return Ok(());
                }
                runtime.block_on(actor.run())
            })
            .map_err(ControllerStartError::ThreadSpawn)?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                handle: ControllerHandle {
                    commands: command_sender,
                    shutdown,
                    snapshots: snapshot_receiver,
                },
                thread: Some(controller_thread),
            }),
            Ok(Err(error)) => {
                let _ = controller_thread.join();
                Err(ControllerStartError::Runtime(error))
            }
            Err(_) => match controller_thread.join() {
                Ok(_) => Err(ControllerStartError::ReadinessChannelClosed),
                Err(_) => Err(ControllerStartError::ReadinessThreadPanicked),
            },
        }
    }

    /// Clone a lightweight nonblocking ingress for application callbacks.
    #[must_use]
    pub fn handle(&self) -> ControllerHandle {
        self.handle.clone()
    }

    /// Synchronously close command admission and cancel any active operation.
    ///
    /// This path is independent of the bounded command queue and never waits.
    pub fn begin_shutdown(&self) {
        self.handle.shutdown.cancel();
    }

    /// Deterministically cancel and join the controller thread.
    pub fn join(mut self) -> Result<(), ControllerJoinError> {
        self.begin_shutdown();
        self.join_thread()
    }

    /// Alias for [`Self::join`] emphasizing the complete shutdown operation.
    pub fn shutdown(self) -> Result<(), ControllerJoinError> {
        self.join()
    }

    fn join_thread(&mut self) -> Result<(), ControllerJoinError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| ControllerJoinError::ThreadPanicked)??;
        Ok(())
    }
}

impl Drop for ControllerRuntime {
    fn drop(&mut self) {
        self.begin_shutdown();
        let _ = self.join_thread();
    }
}

impl ControllerHandle {
    /// Try to admit a command without waiting for queue capacity.
    pub fn try_send(&self, command: ControllerCommand) -> Result<(), ControllerCommandError> {
        if self.shutdown.is_cancelled() {
            return Err(ControllerCommandError::ShuttingDown);
        }
        if matches!(
            command,
            ControllerCommand::SelectDevice(_) | ControllerCommand::ClearSelection
        ) {
            return Err(ControllerCommandError::Unsupported);
        }
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ControllerCommandError::Full,
                mpsc::error::TrySendError::Closed(_) => ControllerCommandError::ShuttingDown,
            })?;
        if self.shutdown.is_cancelled() {
            return Err(ControllerCommandError::ShuttingDown);
        }
        Ok(())
    }

    /// Subscribe to complete, immutable, URL-free application snapshots.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Arc<ApplicationSnapshot>> {
        self.snapshots.clone()
    }

    /// Clone the most recently published complete application snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ApplicationSnapshot> {
        Arc::clone(&self.snapshots.borrow())
    }
}

struct ControllerActor {
    service: Arc<dyn LocalDiscoveryService>,
    commands: mpsc::Receiver<ControllerCommand>,
    shutdown: CancellationToken,
    snapshots: watch::Sender<Arc<ApplicationSnapshot>>,
    registry: DeviceRegistry,
    registry_epoch: Instant,
    revision: SnapshotRevision,
    discovery_generation: OperationGeneration,
    selection_generation: OperationGeneration,
    discovery: DiscoveryState,
    devices: Vec<DeviceSummary>,
    selected_lineup: SelectedLineupState,
    active_discovery: Option<ActiveDiscovery>,
}

impl ControllerActor {
    fn new(
        service: Arc<dyn LocalDiscoveryService>,
        commands: mpsc::Receiver<ControllerCommand>,
        shutdown: CancellationToken,
        snapshots: watch::Sender<Arc<ApplicationSnapshot>>,
    ) -> Self {
        Self {
            service,
            commands,
            shutdown,
            snapshots,
            registry: DeviceRegistry::default(),
            registry_epoch: Instant::now(),
            revision: SnapshotRevision::INITIAL,
            discovery_generation: OperationGeneration::INITIAL,
            selection_generation: OperationGeneration::INITIAL,
            discovery: DiscoveryState::idle(OperationGeneration::INITIAL),
            devices: Vec::new(),
            selected_lineup: SelectedLineupState::unselected(OperationGeneration::INITIAL),
            active_discovery: None,
        }
    }

    async fn run(mut self) -> Result<(), ControllerRuntimeError> {
        loop {
            let event = if let Some(active) = &mut self.active_discovery {
                tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                    command = self.commands.recv() => ActorEvent::Command(command),
                    completion = &mut active.task => ActorEvent::Discovery(completion),
                }
            } else {
                tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                    command = self.commands.recv() => ActorEvent::Command(command),
                }
            };

            match event {
                ActorEvent::Shutdown | ActorEvent::Command(None) => {
                    self.cancel_active_discovery().await;
                    return Ok(());
                }
                ActorEvent::Command(Some(ControllerCommand::RefreshLocalDiscovery)) => {
                    self.start_local_refresh().await?;
                }
                ActorEvent::Command(Some(ControllerCommand::CancelLocalDiscovery)) => {
                    self.cancel_local_refresh().await?;
                }
                ActorEvent::Command(Some(
                    ControllerCommand::SelectDevice(_) | ControllerCommand::ClearSelection,
                )) => {
                    // Public admission currently rejects these commands. Keep
                    // the actor side inert as defense in depth.
                }
                ActorEvent::Discovery(completion) => {
                    self.finish_local_refresh(completion)?;
                }
            }
        }
    }

    async fn start_local_refresh(&mut self) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_discovery().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        let generation = self.next_discovery_generation()?;
        self.discovery = DiscoveryState::refreshing(generation);
        self.publish()?;

        let cancellation = self.shutdown.child_token();
        let service = Arc::clone(&self.service);
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let result = if task_cancellation.is_cancelled() {
                Err(DiscoveryFailure::Internal)
            } else {
                service.discover_local(task_cancellation).await
            };
            DiscoveryCompletion { generation, result }
        });
        self.active_discovery = Some(ActiveDiscovery {
            generation,
            cancellation,
            task,
        });
        Ok(())
    }

    async fn cancel_local_refresh(&mut self) -> Result<(), ControllerRuntimeError> {
        if self.active_discovery.is_none() {
            return Ok(());
        }
        self.cancel_active_discovery().await;
        let generation = self.next_discovery_generation()?;
        self.discovery = DiscoveryState::idle(generation);
        self.publish()
    }

    async fn cancel_active_discovery(&mut self) {
        let Some(active) = self.active_discovery.take() else {
            return;
        };
        active.cancellation.cancel();
        let _ = active.task.await;
    }

    fn finish_local_refresh(
        &mut self,
        completion: Result<DiscoveryCompletion, tokio::task::JoinError>,
    ) -> Result<(), ControllerRuntimeError> {
        let Some(active) = self.active_discovery.take() else {
            return Ok(());
        };
        let completion = match completion {
            Ok(completion) => completion,
            Err(_) => DiscoveryCompletion {
                generation: active.generation,
                result: Err(DiscoveryFailure::Internal),
            },
        };
        self.apply_discovery_completion(completion)?;
        Ok(())
    }

    fn apply_discovery_completion(
        &mut self,
        completion: DiscoveryCompletion,
    ) -> Result<bool, ControllerRuntimeError> {
        if completion.generation != self.discovery_generation
            || self.discovery.status() != DiscoveryStatus::Refreshing
        {
            return Ok(false);
        }

        match completion.result {
            Ok(report) => {
                let issue_count = u16::try_from(report.issues.len()).unwrap_or(u16::MAX);
                match self.apply_discovery_report(report) {
                    Ok(()) => {
                        self.discovery =
                            DiscoveryState::ready(self.discovery_generation, issue_count);
                    }
                    Err(()) => {
                        self.discovery = DiscoveryState::failed(
                            self.discovery_generation,
                            DiscoveryFailure::Internal,
                        );
                    }
                }
            }
            Err(failure) => {
                self.discovery = DiscoveryState::failed(self.discovery_generation, failure);
            }
        }
        self.publish()?;
        Ok(true)
    }

    fn apply_discovery_report(&mut self, mut report: DiscoveryReport) -> Result<(), ()> {
        report
            .observations
            .sort_by_key(|observation| (observation.device_id, observation.source));
        // A completed local scan is the new authoritative local-discovery
        // view. Build it atomically so absent devices disappear on success,
        // while any malformed/conflicting report leaves the last-good
        // registry untouched.
        let mut candidate = DeviceRegistry::default();
        let seen_at = RegistryInstant::from_duration(self.registry_epoch.elapsed());
        for observation in report.observations {
            candidate.observe(observation, seen_at).map_err(|_| ())?;
        }
        let projection = project_devices(&candidate)?;
        self.registry = candidate;
        self.devices = projection;
        Ok(())
    }

    fn next_discovery_generation(&mut self) -> Result<OperationGeneration, ControllerRuntimeError> {
        let generation = self
            .discovery_generation
            .checked_next()
            .ok_or(ControllerRuntimeError::DiscoveryGenerationExhausted)?;
        self.discovery_generation = generation;
        Ok(generation)
    }

    fn publish(&mut self) -> Result<(), ControllerRuntimeError> {
        let revision = self
            .revision
            .checked_next()
            .ok_or(ControllerRuntimeError::SnapshotRevisionExhausted)?;
        let snapshot = ApplicationSnapshot::new(
            revision,
            self.discovery_generation,
            self.selection_generation,
            self.discovery,
            self.devices.iter().cloned(),
            None,
            self.selected_lineup.clone(),
        )?;
        self.revision = revision;
        self.snapshots.send_replace(Arc::new(snapshot));
        Ok(())
    }
}

fn project_devices(registry: &DeviceRegistry) -> Result<Vec<DeviceSummary>, ()> {
    registry
        .devices()
        .map(|device| {
            let preferred = device.preferred_locator().ok_or(())?;
            DeviceSummary::new(
                device.device_id(),
                None,
                None,
                preferred.tuner_count(),
                preferred.source(),
                device.locators().len(),
            )
            .map_err(|_| ())
        })
        .collect()
}

enum ActorEvent {
    Shutdown,
    Command(Option<ControllerCommand>),
    Discovery(Result<DiscoveryCompletion, tokio::task::JoinError>),
}

struct ActiveDiscovery {
    generation: OperationGeneration,
    cancellation: CancellationToken,
    task: JoinHandle<DiscoveryCompletion>,
}

struct DiscoveryCompletion {
    generation: OperationGeneration,
    result: Result<DiscoveryReport, DiscoveryFailure>,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, mpsc as std_mpsc};
    use std::time::Duration;

    use tokio::runtime::RuntimeFlavor;
    use tokio::sync::oneshot;

    use super::*;
    use crate::discovery::{DiscoveryMethod, DiscoveryObservation};

    const WAIT: Duration = Duration::from_secs(3);

    #[derive(Clone)]
    struct ScriptedService {
        shared: Arc<ScriptedState>,
    }

    struct ScriptedState {
        steps: Mutex<VecDeque<ServiceStep>>,
        calls: AtomicUsize,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        started: std_mpsc::Sender<ServiceStart>,
    }

    struct ServiceStart {
        call: usize,
        thread_name: Option<String>,
        runtime_flavor: RuntimeFlavor,
    }

    enum ServiceStep {
        Immediate(Result<DiscoveryReport, DiscoveryFailure>),
        Gated {
            release: oneshot::Receiver<Result<DiscoveryReport, DiscoveryFailure>>,
            cancelled: std_mpsc::Sender<()>,
            cancellation_result: Result<DiscoveryReport, DiscoveryFailure>,
        },
        CancellationBarrier {
            cancellation_observed: std_mpsc::Sender<()>,
            finish_cancellation: oneshot::Receiver<()>,
            cancellation_result: Result<DiscoveryReport, DiscoveryFailure>,
        },
    }

    impl ScriptedService {
        fn new(
            steps: impl IntoIterator<Item = ServiceStep>,
        ) -> (Self, std_mpsc::Receiver<ServiceStart>) {
            let (started, starts) = std_mpsc::channel();
            (
                Self {
                    shared: Arc::new(ScriptedState {
                        steps: Mutex::new(steps.into_iter().collect()),
                        calls: AtomicUsize::new(0),
                        active: AtomicUsize::new(0),
                        maximum_active: AtomicUsize::new(0),
                        started,
                    }),
                },
                starts,
            )
        }

        fn calls(&self) -> usize {
            self.shared.calls.load(Ordering::SeqCst)
        }

        fn maximum_active(&self) -> usize {
            self.shared.maximum_active.load(Ordering::SeqCst)
        }
    }

    impl LocalDiscoveryService for ScriptedService {
        fn discover_local(&self, cancellation: CancellationToken) -> LocalDiscoveryFuture {
            let state = Arc::clone(&self.shared);
            let step = state
                .steps
                .lock()
                .expect("script mutex should not be poisoned")
                .pop_front()
                .expect("test should provide one service step per call");
            let call = state.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
            state.maximum_active.fetch_max(active, Ordering::SeqCst);
            state
                .started
                .send(ServiceStart {
                    call,
                    thread_name: thread::current().name().map(str::to_owned),
                    runtime_flavor: tokio::runtime::Handle::current().runtime_flavor(),
                })
                .expect("test start receiver should remain open");

            Box::pin(async move {
                let result = match step {
                    ServiceStep::Immediate(result) => result,
                    ServiceStep::Gated {
                        release,
                        cancelled,
                        cancellation_result,
                    } => {
                        tokio::select! {
                            result = release => result.expect("test release sender should remain open"),
                            () = cancellation.cancelled() => {
                                let _ = cancelled.send(());
                                cancellation_result
                            }
                        }
                    }
                    ServiceStep::CancellationBarrier {
                        cancellation_observed,
                        finish_cancellation,
                        cancellation_result,
                    } => {
                        cancellation.cancelled().await;
                        let _ = cancellation_observed.send(());
                        finish_cancellation
                            .await
                            .expect("test cancellation release should remain open");
                        cancellation_result
                    }
                };
                state.active.fetch_sub(1, Ordering::SeqCst);
                result
            })
        }
    }

    fn first_id() -> DeviceId {
        DeviceId::new(0x105A_1232).unwrap()
    }

    fn second_id() -> DeviceId {
        DeviceId::new(0x105A_1243).unwrap()
    }

    fn report(device_id: DeviceId, source: &str, tuner_count: u8) -> DiscoveryReport {
        DiscoveryReport {
            observations: vec![DiscoveryObservation {
                device_id,
                source: source.parse().unwrap(),
                method: DiscoveryMethod::Targeted,
                interface: Some("synthetic0".to_owned()),
                device_types: vec![1],
                tuner_count: Some(tuner_count),
                advertised_base_url: Some("http://untrusted.invalid/secret".to_owned()),
                advertised_lineup_url: Some("http://untrusted.invalid/lineup".to_owned()),
            }],
            ..DiscoveryReport::default()
        }
    }

    async fn wait_for_snapshot(
        receiver: &mut watch::Receiver<Arc<ApplicationSnapshot>>,
        predicate: impl Fn(&ApplicationSnapshot) -> bool,
    ) -> Arc<ApplicationSnapshot> {
        tokio::time::timeout(WAIT, async {
            loop {
                let snapshot = Arc::clone(&receiver.borrow_and_update());
                if predicate(&snapshot) {
                    return snapshot;
                }
                receiver
                    .changed()
                    .await
                    .expect("controller should remain alive while waiting");
            }
        })
        .await
        .expect("controller snapshot wait should remain bounded")
    }

    fn recv_start(starts: &std_mpsc::Receiver<ServiceStart>) -> ServiceStart {
        starts
            .recv_timeout(WAIT)
            .expect("discovery service should start within the test deadline")
    }

    #[test]
    fn invalid_capacities_fail_before_service_or_thread_startup() {
        let (service, _) = ScriptedService::new([]);
        assert!(matches!(
            ControllerRuntime::start_with_capacity(service.clone(), 0),
            Err(ControllerStartError::InvalidCommandCapacity { value: 0, .. })
        ));
        assert!(matches!(
            ControllerRuntime::start_with_capacity(service.clone(), MAX_COMMAND_CAPACITY + 1),
            Err(ControllerStartError::InvalidCommandCapacity { .. })
        ));
        assert_eq!(service.calls(), 0);
    }

    #[test]
    fn construction_is_inert_and_selection_commands_fail_closed() {
        let (service, _) = ScriptedService::new([]);
        let controller = ControllerRuntime::start(service.clone()).unwrap();
        let handle = controller.handle();

        assert_eq!(service.calls(), 0);
        assert_eq!(*handle.snapshot(), ApplicationSnapshot::initial());
        assert_eq!(
            handle.try_send(ControllerCommand::SelectDevice(first_id())),
            Err(ControllerCommandError::Unsupported)
        );
        assert_eq!(
            handle.try_send(ControllerCommand::ClearSelection),
            Err(ControllerCommandError::Unsupported)
        );
        assert_eq!(service.calls(), 0);
        assert_eq!(*handle.snapshot(), ApplicationSnapshot::initial());
        controller.shutdown().unwrap();
    }

    #[test]
    fn production_controller_is_default_constructible_and_sendable() {
        fn assert_send<T: Send>() {}
        assert_send::<ControllerRuntime>();
        assert_send::<ControllerHandle>();

        let controller = ControllerRuntime::start_default().unwrap();
        assert_eq!(
            *controller.handle().snapshot(),
            ApplicationSnapshot::initial()
        );
        controller.shutdown().unwrap();
    }

    #[test]
    fn bounded_command_admission_reports_full_without_waiting() {
        let (sender, _receiver) = mpsc::channel(1);
        let (_snapshot_sender, snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        let shutdown = CancellationToken::new();
        let controller = ControllerHandle {
            commands: sender,
            shutdown: shutdown.clone(),
            snapshots: snapshot_receiver,
        };

        controller
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(
            controller.try_send(ControllerCommand::CancelLocalDiscovery),
            Err(ControllerCommandError::Full)
        );
        shutdown.cancel();
        assert_eq!(
            controller.try_send(ControllerCommand::RefreshLocalDiscovery),
            Err(ControllerCommandError::ShuttingDown)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_refresh_runs_on_named_current_thread_runtime_and_publishes_devices() {
        let (service, starts) = ScriptedService::new([ServiceStep::Immediate(Ok(report(
            first_id(),
            "192.0.2.10:65001",
            4,
        )))]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        let start = recv_start(&starts);
        assert_eq!(start.call, 1);
        assert_eq!(start.thread_name.as_deref(), Some(CONTROLLER_THREAD_NAME));
        assert_eq!(start.runtime_flavor, RuntimeFlavor::CurrentThread);

        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(ready.revision(), SnapshotRevision::new(2));
        assert_eq!(ready.discovery_generation(), OperationGeneration::new(1));
        assert_eq!(ready.devices().len(), 1);
        let device = &ready.devices()[0];
        assert_eq!(device.device_id(), first_id());
        assert_eq!(
            device.preferred_locator(),
            "192.0.2.10:65001".parse().unwrap()
        );
        assert_eq!(device.tuner_count(), Some(4));
        assert_eq!(device.friendly_name(), None);
        assert_eq!(device.model_number(), None);
        assert_eq!(ready.selected_device(), None);
        assert!(ready.selected_lineup().channels().is_empty());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_supersession_cancels_and_joins_before_starting_replacement() {
        let (first_release, _first_receiver) = oneshot::channel();
        let (first_cancelled, first_cancelled_rx) = std_mpsc::channel();
        let (service, starts) = ScriptedService::new([
            ServiceStep::Gated {
                release: _first_receiver,
                cancelled: first_cancelled,
                cancellation_result: Ok(report(first_id(), "192.0.2.10:65001", 2)),
            },
            ServiceStep::Immediate(Ok(report(second_id(), "192.0.2.20:65001", 4))),
        ]);
        let observed_service = service.clone();
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(recv_start(&starts).call, 1);
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        first_cancelled_rx
            .recv_timeout(WAIT)
            .expect("supersession should cancel the first service future");
        assert_eq!(recv_start(&starts).call, 2);

        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(ready.devices().len(), 1);
        assert_eq!(ready.devices()[0].device_id(), second_id());
        assert_eq!(observed_service.maximum_active(), 1);
        drop(first_release);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_cancel_is_idle_in_a_new_generation_and_retains_devices() {
        let (_release, release_rx) = oneshot::channel();
        let (cancelled, cancelled_rx) = std_mpsc::channel();
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Gated {
                release: release_rx,
                cancelled,
                cancellation_result: Err(DiscoveryFailure::Internal),
            },
        ]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        handle
            .try_send(ControllerCommand::CancelLocalDiscovery)
            .unwrap();
        cancelled_rx
            .recv_timeout(WAIT)
            .expect("explicit cancel should synchronously reach the service token");
        let idle = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(3)
                && snapshot.discovery().status() == DiscoveryStatus::Idle
        })
        .await;
        assert_eq!(idle.devices().len(), 1);
        assert_eq!(idle.devices()[0].device_id(), first_id());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_refresh_retains_last_good_projection() {
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Err(DiscoveryFailure::Network)),
        ]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        let failed = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Failed(DiscoveryFailure::Network)
        })
        .await;

        assert_eq!(failed.devices().len(), 1);
        assert_eq!(failed.devices()[0].device_id(), first_id());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_empty_refresh_replaces_the_previous_registry_projection() {
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
        ]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(1)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        assert!(ready.devices().is_empty());
        controller.shutdown().unwrap();
    }

    #[test]
    fn stale_completion_cannot_change_registry_or_snapshot() {
        let (service, _) = ScriptedService::new([]);
        let (_commands, receiver) = mpsc::channel(1);
        let (snapshots, snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        let mut actor = ControllerActor::new(
            Arc::new(service),
            receiver,
            CancellationToken::new(),
            snapshots,
        );
        actor.discovery_generation = OperationGeneration::new(2);
        actor.discovery = DiscoveryState::refreshing(OperationGeneration::new(2));

        assert!(
            !actor
                .apply_discovery_completion(DiscoveryCompletion {
                    generation: OperationGeneration::new(1),
                    result: Ok(report(first_id(), "192.0.2.10:65001", 4)),
                })
                .unwrap()
        );
        assert!(actor.registry.is_empty());
        assert!(actor.devices.is_empty());
        assert_eq!(
            snapshot_receiver.borrow().revision(),
            SnapshotRevision::INITIAL
        );
        assert_eq!(
            actor.discovery,
            DiscoveryState::refreshing(OperationGeneration::new(2))
        );
    }

    #[test]
    fn shutdown_bypasses_commands_and_cancels_before_join() {
        let (_release, release_rx) = oneshot::channel();
        let (cancelled, cancelled_rx) = std_mpsc::channel();
        let (service, starts) = ScriptedService::new([ServiceStep::Gated {
            release: release_rx,
            cancelled,
            cancellation_result: Err(DiscoveryFailure::Internal),
        }]);
        let controller = ControllerRuntime::start_with_capacity(service, 1).unwrap();
        let handle = controller.handle();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);

        controller.begin_shutdown();
        assert_eq!(
            handle.try_send(ControllerCommand::CancelLocalDiscovery),
            Err(ControllerCommandError::ShuttingDown)
        );
        controller.shutdown().unwrap();
        cancelled_rx
            .recv_timeout(WAIT)
            .expect("shutdown token should cancel the active service future");
    }

    #[test]
    fn shutdown_winning_during_supersession_prevents_the_replacement_service_call() {
        let (cancellation_observed, cancellation_observed_rx) = std_mpsc::channel();
        let (finish_cancellation, finish_cancellation_rx) = oneshot::channel();
        let (service, starts) = ScriptedService::new([
            ServiceStep::CancellationBarrier {
                cancellation_observed,
                finish_cancellation: finish_cancellation_rx,
                cancellation_result: Err(DiscoveryFailure::Internal),
            },
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
        ]);
        let observed_service = service.clone();
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(recv_start(&starts).call, 1);
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        cancellation_observed_rx
            .recv_timeout(WAIT)
            .expect("supersession should reach the first operation's cancellation barrier");
        controller.begin_shutdown();
        finish_cancellation
            .send(())
            .expect("controller should still own the cancellation barrier receiver");
        controller.join().unwrap();
        assert_eq!(observed_service.calls(), 1);
        assert!(starts.try_recv().is_err());
    }

    #[test]
    fn drop_cancels_and_joins_active_discovery() {
        let (_release, release_rx) = oneshot::channel();
        let (cancelled, cancelled_rx) = std_mpsc::channel();
        let (service, starts) = ScriptedService::new([ServiceStep::Gated {
            release: release_rx,
            cancelled,
            cancellation_result: Err(DiscoveryFailure::Internal),
        }]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);

        drop(controller);
        cancelled_rx
            .recv_timeout(WAIT)
            .expect("dropping the owner should cancel and join the service future");
    }
}
