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
    ApplicationSnapshot, ChannelSummary, DeviceSummary, DiscoveryFailure, DiscoveryState,
    DiscoveryStatus, LineupFailure, OperationGeneration, SelectedLineupState, SelectedLineupStatus,
    SnapshotRevision, StateError,
};
use crate::discovery::{
    DeviceRegistry, DiscoveryClient, DiscoveryError, DiscoveryReport, RegistryInstant,
};
use crate::domain::DeviceId;
use crate::hdhr::{
    DeviceSnapshotIssueKind, DeviceSnapshotResolutionError, DeviceSnapshotResolver,
    DeviceSnapshotTarget, DeviceSnapshotTargetError, ResolvedDeviceSnapshot,
};

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

type SelectedDeviceFuture = Pin<
    Box<
        dyn Future<Output = Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>>
            + Send
            + 'static,
    >,
>;

/// Private injection boundary for the identity-checked selected-device lane.
/// Production always installs [`DeviceSnapshotResolver`]; tests may inject a
/// packet-free scripted service through a test-only constructor.
trait SelectedDeviceService: Send + Sync + 'static {
    fn resolve_selected(
        &self,
        target: DeviceSnapshotTarget,
        cancellation: CancellationToken,
    ) -> SelectedDeviceFuture;
}

impl SelectedDeviceService for DeviceSnapshotResolver {
    fn resolve_selected(
        &self,
        target: DeviceSnapshotTarget,
        cancellation: CancellationToken,
    ) -> SelectedDeviceFuture {
        let resolver = self.clone();
        Box::pin(async move { resolver.resolve(&target, &cancellation).await })
    }
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
    /// Resolve and retain exactly this registered device's lineup.
    SelectDevice(DeviceId),
    /// Cancel selected-device work and discard its retained snapshot.
    ClearSelection,
}

/// Immediate result of trying to admit a controller command.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ControllerCommandError {
    #[error("controller command queue is full")]
    Full,
    #[error("controller is shutting down")]
    ShuttingDown,
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
    #[error("selected-device generation is exhausted")]
    SelectionGenerationExhausted,
    #[error("selected-device snapshot retention invariant failed")]
    SelectionSnapshotInvariant,
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
        Self::start_with_services_and_capacity(
            service,
            DeviceSnapshotResolver::default(),
            command_capacity,
        )
    }

    fn start_with_services_and_capacity<D, S>(
        discovery_service: D,
        selection_service: S,
        command_capacity: usize,
    ) -> Result<Self, ControllerStartError>
    where
        D: LocalDiscoveryService,
        S: SelectedDeviceService,
    {
        if !(1..=MAX_COMMAND_CAPACITY).contains(&command_capacity) {
            return Err(ControllerStartError::InvalidCommandCapacity {
                value: command_capacity,
                maximum: MAX_COMMAND_CAPACITY,
            });
        }

        let discovery_service: Arc<dyn LocalDiscoveryService> = Arc::new(discovery_service);
        let selection_service: Arc<dyn SelectedDeviceService> = Arc::new(selection_service);
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
                    discovery_service,
                    selection_service,
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

    #[cfg(test)]
    fn start_with_test_services<D, S>(
        discovery_service: D,
        selection_service: S,
    ) -> Result<Self, ControllerStartError>
    where
        D: LocalDiscoveryService,
        S: SelectedDeviceService,
    {
        Self::start_with_services_and_capacity(
            discovery_service,
            selection_service,
            DEFAULT_COMMAND_CAPACITY,
        )
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
    discovery_service: Arc<dyn LocalDiscoveryService>,
    selection_service: Arc<dyn SelectedDeviceService>,
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
    selected_device: Option<DeviceId>,
    selected_lineup: SelectedLineupState,
    selected_snapshot: Option<ResolvedDeviceSnapshot>,
    active_discovery: Option<ActiveDiscovery>,
    active_selection: Option<ActiveSelection>,
}

impl ControllerActor {
    fn new(
        discovery_service: Arc<dyn LocalDiscoveryService>,
        selection_service: Arc<dyn SelectedDeviceService>,
        commands: mpsc::Receiver<ControllerCommand>,
        shutdown: CancellationToken,
        snapshots: watch::Sender<Arc<ApplicationSnapshot>>,
    ) -> Self {
        Self {
            discovery_service,
            selection_service,
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
            selected_device: None,
            selected_lineup: SelectedLineupState::unselected(OperationGeneration::INITIAL),
            selected_snapshot: None,
            active_discovery: None,
            active_selection: None,
        }
    }

    async fn run(mut self) -> Result<(), ControllerRuntimeError> {
        loop {
            let event = match (
                self.active_discovery.as_mut(),
                self.active_selection.as_mut(),
            ) {
                (Some(discovery), Some(selection)) => {
                    tokio::select! {
                        biased;
                        () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                        command = self.commands.recv() => ActorEvent::Command(command),
                        completion = &mut discovery.task => ActorEvent::Discovery(completion),
                        completion = &mut selection.task => ActorEvent::Selection(completion),
                    }
                }
                (Some(discovery), None) => {
                    tokio::select! {
                        biased;
                        () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                        command = self.commands.recv() => ActorEvent::Command(command),
                        completion = &mut discovery.task => ActorEvent::Discovery(completion),
                    }
                }
                (None, Some(selection)) => {
                    tokio::select! {
                        biased;
                        () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                        command = self.commands.recv() => ActorEvent::Command(command),
                        completion = &mut selection.task => ActorEvent::Selection(completion),
                    }
                }
                (None, None) => {
                    tokio::select! {
                        biased;
                        () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                        command = self.commands.recv() => ActorEvent::Command(command),
                    }
                }
            };

            match event {
                ActorEvent::Shutdown | ActorEvent::Command(None) => {
                    self.cancel_all_operations().await;
                    return Ok(());
                }
                ActorEvent::Command(Some(ControllerCommand::RefreshLocalDiscovery)) => {
                    self.start_local_refresh().await?;
                }
                ActorEvent::Command(Some(ControllerCommand::CancelLocalDiscovery)) => {
                    self.cancel_local_refresh().await?;
                }
                ActorEvent::Command(Some(ControllerCommand::SelectDevice(device_id))) => {
                    self.select_device(device_id).await?;
                }
                ActorEvent::Command(Some(ControllerCommand::ClearSelection)) => {
                    self.clear_selection().await?;
                }
                ActorEvent::Discovery(completion) => {
                    self.finish_local_refresh(completion).await?;
                }
                ActorEvent::Selection(completion) => {
                    self.finish_selection(completion)?;
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
        let service = Arc::clone(&self.discovery_service);
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

    async fn cancel_active_selection(&mut self) {
        let Some(active) = self.active_selection.take() else {
            return;
        };
        active.cancellation.cancel();
        let _ = active.task.await;
    }

    async fn cancel_all_operations(&mut self) {
        let discovery = self.active_discovery.take();
        let selection = self.active_selection.take();
        if let Some(active) = &discovery {
            active.cancellation.cancel();
        }
        if let Some(active) = &selection {
            active.cancellation.cancel();
        }

        match (discovery, selection) {
            (Some(discovery), Some(selection)) => {
                let _ = tokio::join!(discovery.task, selection.task);
            }
            (Some(discovery), None) => {
                let _ = discovery.task.await;
            }
            (None, Some(selection)) => {
                let _ = selection.task.await;
            }
            (None, None) => {}
        }
    }

    async fn finish_local_refresh(
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
        self.apply_discovery_completion(completion).await?;
        Ok(())
    }

    async fn apply_discovery_completion(
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
                match self.build_discovery_projection(report) {
                    Ok((registry, devices)) => {
                        let selected_device = self.selected_device;
                        if selected_device.is_some() {
                            self.cancel_active_selection().await;
                            if self.shutdown.is_cancelled() {
                                return Ok(false);
                            }
                        }

                        self.registry = registry;
                        self.devices = devices;
                        self.discovery =
                            DiscoveryState::ready(self.discovery_generation, issue_count);
                        let pending_selection = if let Some(device_id) = selected_device {
                            let generation = self.next_selection_generation()?;
                            self.prepare_selection(device_id, generation)
                        } else {
                            None
                        };
                        self.publish()?;
                        self.spawn_selection(pending_selection);
                        return Ok(true);
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

    fn build_discovery_projection(
        &self,
        mut report: DiscoveryReport,
    ) -> Result<(DeviceRegistry, Vec<DeviceSummary>), ()> {
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
        Ok((candidate, projection))
    }

    async fn select_device(&mut self, device_id: DeviceId) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_selection().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }

        let generation = self.next_selection_generation()?;
        let pending = self.prepare_selection(device_id, generation);
        self.publish()?;
        self.spawn_selection(pending);
        Ok(())
    }

    async fn clear_selection(&mut self) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_selection().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }

        let generation = self.next_selection_generation()?;
        self.selected_device = None;
        self.selected_lineup = SelectedLineupState::unselected(generation);
        self.selected_snapshot = None;
        self.publish()
    }

    fn prepare_selection(
        &mut self,
        device_id: DeviceId,
        generation: OperationGeneration,
    ) -> Option<PendingSelection> {
        self.selected_snapshot = None;
        let Some(device) = self.registry.get(device_id) else {
            self.selected_device = None;
            self.selected_lineup = SelectedLineupState::unselected(generation);
            return None;
        };

        self.selected_device = Some(device_id);
        let target = match DeviceSnapshotTarget::from_registered(device) {
            Ok(target) => target,
            Err(DeviceSnapshotTargetError::NoLocators) => {
                self.selected_lineup = SelectedLineupState::failed(
                    device_id,
                    generation,
                    LineupFailure::NoSupportedLocator,
                );
                return None;
            }
            Err(DeviceSnapshotTargetError::TooManyLocators { .. }) => {
                self.selected_lineup =
                    SelectedLineupState::failed(device_id, generation, LineupFailure::Internal);
                return None;
            }
        };
        let supported_locator_count = target.supported_locator_count();
        if supported_locator_count == 0 {
            self.selected_lineup = SelectedLineupState::failed(
                device_id,
                generation,
                LineupFailure::NoSupportedLocator,
            );
            return None;
        }

        self.selected_lineup = SelectedLineupState::loading(device_id, generation);
        Some(PendingSelection {
            generation,
            device_id,
            supported_locator_count,
            target,
        })
    }

    fn spawn_selection(&mut self, pending: Option<PendingSelection>) {
        let Some(pending) = pending else {
            return;
        };
        if self.shutdown.is_cancelled() {
            return;
        }

        let cancellation = self.shutdown.child_token();
        let task_cancellation = cancellation.clone();
        let service = Arc::clone(&self.selection_service);
        let PendingSelection {
            generation,
            device_id,
            supported_locator_count,
            target,
        } = pending;
        let task = tokio::spawn(async move {
            let result = if task_cancellation.is_cancelled() {
                Err(DeviceSnapshotResolutionError::Cancelled)
            } else {
                service.resolve_selected(target, task_cancellation).await
            };
            SelectionCompletion {
                generation,
                device_id,
                result,
            }
        });
        self.active_selection = Some(ActiveSelection {
            generation,
            device_id,
            supported_locator_count,
            cancellation,
            task,
        });
    }

    fn finish_selection(
        &mut self,
        completion: Result<SelectionCompletion, tokio::task::JoinError>,
    ) -> Result<(), ControllerRuntimeError> {
        let Some(active) = self.active_selection.take() else {
            return Ok(());
        };
        match completion {
            Ok(completion) => {
                self.apply_selection_completion(completion, active.supported_locator_count)?;
            }
            Err(_) => {
                if self.selection_is_current(active.generation, active.device_id) {
                    self.selected_snapshot = None;
                    self.selected_lineup = SelectedLineupState::failed(
                        active.device_id,
                        active.generation,
                        LineupFailure::Internal,
                    );
                    self.publish()?;
                }
            }
        }
        Ok(())
    }

    fn apply_selection_completion(
        &mut self,
        completion: SelectionCompletion,
        supported_locator_count: usize,
    ) -> Result<bool, ControllerRuntimeError> {
        if !self.selection_is_current(completion.generation, completion.device_id) {
            return Ok(false);
        }

        match completion.result {
            Ok(resolved) if resolved.device_id() == completion.device_id => {
                if self.accept_selected_snapshot(resolved).is_err() {
                    self.selected_snapshot = None;
                    self.selected_lineup = SelectedLineupState::failed(
                        completion.device_id,
                        completion.generation,
                        LineupFailure::Internal,
                    );
                }
            }
            Ok(_) => {
                self.selected_snapshot = None;
                self.selected_lineup = SelectedLineupState::failed(
                    completion.device_id,
                    completion.generation,
                    LineupFailure::IdentityMismatch,
                );
            }
            Err(error) => {
                self.selected_snapshot = None;
                self.selected_lineup = SelectedLineupState::failed(
                    completion.device_id,
                    completion.generation,
                    project_resolution_failure(
                        &error,
                        completion.device_id,
                        supported_locator_count,
                    ),
                );
            }
        }
        self.publish()?;
        Ok(true)
    }

    fn selection_is_current(&self, generation: OperationGeneration, device_id: DeviceId) -> bool {
        generation == self.selection_generation
            && self.selected_device == Some(device_id)
            && self.selected_lineup.device_id() == Some(device_id)
            && self.selected_lineup.status() == SelectedLineupStatus::Loading
    }

    fn accept_selected_snapshot(
        &mut self,
        resolved: ResolvedDeviceSnapshot,
    ) -> Result<(), StateError> {
        let device_id = resolved.device_id();
        let device_index = self
            .devices
            .binary_search_by_key(&device_id, DeviceSummary::device_id)
            .map_err(|_| StateError::SelectedDeviceMissing(device_id))?;
        let current = &self.devices[device_index];
        let info = resolved.snapshot().info();
        let summary = DeviceSummary::new(
            device_id,
            info.friendly_name().map(str::to_owned),
            info.model_number().map(str::to_owned),
            info.tuner_count(),
            current.preferred_locator(),
            current.locator_count(),
        )?;
        let channels = resolved
            .snapshot()
            .lineup()
            .channels()
            .iter()
            .map(|channel| {
                ChannelSummary::new(
                    channel.key().clone(),
                    channel.name().to_owned(),
                    channel.is_favorite(),
                    channel.is_drm(),
                    channel.is_hd(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let lineup = SelectedLineupState::ready(device_id, self.selection_generation, channels)?;

        self.devices[device_index] = summary;
        self.selected_lineup = lineup;
        self.selected_snapshot = Some(resolved);
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

    fn next_selection_generation(&mut self) -> Result<OperationGeneration, ControllerRuntimeError> {
        let generation = self
            .selection_generation
            .checked_next()
            .ok_or(ControllerRuntimeError::SelectionGenerationExhausted)?;
        self.selection_generation = generation;
        Ok(generation)
    }

    fn publish(&mut self) -> Result<(), ControllerRuntimeError> {
        self.validate_selection_retention()?;
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
            self.selected_device,
            self.selected_lineup.clone(),
        )?;
        self.revision = revision;
        self.snapshots.send_replace(Arc::new(snapshot));
        Ok(())
    }

    fn validate_selection_retention(&self) -> Result<(), ControllerRuntimeError> {
        match (
            self.selected_lineup.status(),
            self.selected_device,
            self.selected_snapshot.as_ref(),
        ) {
            (SelectedLineupStatus::Ready, Some(selected), Some(snapshot))
                if snapshot.device_id() == selected
                    && self.selected_lineup.device_id() == Some(selected) =>
            {
                Ok(())
            }
            (SelectedLineupStatus::Ready, _, _) | (_, _, Some(_)) => {
                Err(ControllerRuntimeError::SelectionSnapshotInvariant)
            }
            (_, _, None) => Ok(()),
        }
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

fn project_resolution_failure(
    error: &DeviceSnapshotResolutionError,
    expected_device_id: DeviceId,
    supported_locator_count: usize,
) -> LineupFailure {
    if supported_locator_count == 0 {
        return LineupFailure::NoSupportedLocator;
    }
    let DeviceSnapshotResolutionError::Unavailable(unavailable) = error else {
        return match error {
            DeviceSnapshotResolutionError::Deadline { .. } => LineupFailure::Unreachable,
            DeviceSnapshotResolutionError::Cancelled => LineupFailure::Internal,
            DeviceSnapshotResolutionError::Unavailable(_) => unreachable!(),
        };
    };
    if unavailable.device_id() != expected_device_id {
        return LineupFailure::Internal;
    }
    let issues = unavailable.issues();
    if issues
        .iter()
        .any(|issue| issue.kind() == DeviceSnapshotIssueKind::IdentityMismatch)
    {
        LineupFailure::IdentityMismatch
    } else if issues
        .iter()
        .any(|issue| issue.kind() == DeviceSnapshotIssueKind::LineupInvalid)
    {
        LineupFailure::InvalidLineup
    } else if issues
        .iter()
        .any(|issue| issue.kind() == DeviceSnapshotIssueKind::MetadataInvalid)
    {
        LineupFailure::InvalidMetadata
    } else if issues.iter().any(|issue| {
        matches!(
            issue.kind(),
            DeviceSnapshotIssueKind::MetadataUnreachable
                | DeviceSnapshotIssueKind::LineupUnreachable
        )
    }) {
        LineupFailure::Unreachable
    } else {
        LineupFailure::Internal
    }
}

enum ActorEvent {
    Shutdown,
    Command(Option<ControllerCommand>),
    Discovery(Result<DiscoveryCompletion, tokio::task::JoinError>),
    Selection(Result<SelectionCompletion, tokio::task::JoinError>),
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

struct PendingSelection {
    generation: OperationGeneration,
    device_id: DeviceId,
    supported_locator_count: usize,
    target: DeviceSnapshotTarget,
}

struct ActiveSelection {
    generation: OperationGeneration,
    device_id: DeviceId,
    supported_locator_count: usize,
    cancellation: CancellationToken,
    task: JoinHandle<SelectionCompletion>,
}

struct SelectionCompletion {
    generation: OperationGeneration,
    device_id: DeviceId,
    result: Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>,
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

    #[derive(Clone)]
    struct ScriptedSelectionService {
        shared: Arc<ScriptedSelectionState>,
    }

    struct ScriptedSelectionState {
        steps: Mutex<VecDeque<SelectionStep>>,
        calls: AtomicUsize,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        started: std_mpsc::Sender<SelectionStart>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SelectionStart {
        call: usize,
        device_id: DeviceId,
        locator_count: usize,
    }

    enum SelectionStep {
        Immediate(Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>),
        Gated {
            release:
                oneshot::Receiver<Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>>,
            cancelled: std_mpsc::Sender<()>,
            cancellation_result: Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>,
        },
        CancellationBarrier {
            cancellation_observed: std_mpsc::Sender<()>,
            finish_cancellation: oneshot::Receiver<()>,
            cancellation_result: Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError>,
        },
        Panic,
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

    impl ScriptedSelectionService {
        fn new(
            steps: impl IntoIterator<Item = SelectionStep>,
        ) -> (Self, std_mpsc::Receiver<SelectionStart>) {
            let (started, starts) = std_mpsc::channel();
            (
                Self {
                    shared: Arc::new(ScriptedSelectionState {
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

    impl SelectedDeviceService for ScriptedSelectionService {
        fn resolve_selected(
            &self,
            target: DeviceSnapshotTarget,
            cancellation: CancellationToken,
        ) -> SelectedDeviceFuture {
            let state = Arc::clone(&self.shared);
            let step = state
                .steps
                .lock()
                .expect("selection script mutex should not be poisoned")
                .pop_front()
                .expect("test should provide one selection step per call");
            let call = state.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
            state.maximum_active.fetch_max(active, Ordering::SeqCst);
            state
                .started
                .send(SelectionStart {
                    call,
                    device_id: target.device_id(),
                    locator_count: target.locator_count(),
                })
                .expect("selection start receiver should remain open");

            Box::pin(async move {
                let result = match step {
                    SelectionStep::Immediate(result) => result,
                    SelectionStep::Gated {
                        release,
                        cancelled,
                        cancellation_result,
                    } => {
                        tokio::select! {
                            result = release => result.expect("selection release should remain open"),
                            () = cancellation.cancelled() => {
                                let _ = cancelled.send(());
                                cancellation_result
                            }
                        }
                    }
                    SelectionStep::CancellationBarrier {
                        cancellation_observed,
                        finish_cancellation,
                        cancellation_result,
                    } => {
                        cancellation.cancelled().await;
                        let _ = cancellation_observed.send(());
                        finish_cancellation
                            .await
                            .expect("selection cancellation release should remain open");
                        cancellation_result
                    }
                    SelectionStep::Panic => panic!("scripted selection panic"),
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
                advertised_base_url: None,
                advertised_lineup_url: None,
            }],
            ..DiscoveryReport::default()
        }
    }

    fn unsupported_report(device_id: DeviceId, source: &str) -> DiscoveryReport {
        let mut report = report(device_id, source, 4);
        report.observations[0].advertised_base_url =
            Some("https://operator:secret@invalid.example/".to_owned());
        report
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

    fn recv_selection_start(starts: &std_mpsc::Receiver<SelectionStart>) -> SelectionStart {
        starts
            .recv_timeout(WAIT)
            .expect("selection service should start within the test deadline")
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
    fn construction_is_inert() {
        let (service, _) = ScriptedService::new([]);
        let controller = ControllerRuntime::start(service.clone()).unwrap();
        let handle = controller.handle();

        assert_eq!(service.calls(), 0);
        assert_eq!(*handle.snapshot(), ApplicationSnapshot::initial());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_and_unsupported_selection_never_call_the_resolver() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            unsupported_report(first_id(), "192.0.2.10:65001"),
        ))]);
        let (selection, _selection_starts) = ScriptedSelectionService::new([]);
        let observed_selection = selection.clone();
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        let unknown = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(1)
        })
        .await;
        assert_eq!(unknown.selected_device(), None);
        assert_eq!(
            unknown.selected_lineup().status(),
            SelectedLineupStatus::Unselected
        );

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        let unsupported = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(2)
                && snapshot.selected_lineup().status()
                    == SelectedLineupStatus::Failed(LineupFailure::NoSupportedLocator)
        })
        .await;

        assert_eq!(unsupported.selected_device(), Some(first_id()));
        assert!(unsupported.selected_lineup().channels().is_empty());
        assert_eq!(observed_selection.calls(), 0);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn selected_device_projects_metadata_and_channels_without_urls() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "192.0.2.10:65001", 4),
        ))]);
        let (release, release_rx) = oneshot::channel();
        let (cancelled, _cancelled_rx) = std_mpsc::channel();
        let (selection, selection_starts) = ScriptedSelectionService::new([SelectionStep::Gated {
            release: release_rx,
            cancelled,
            cancellation_result: Err(DeviceSnapshotResolutionError::Cancelled),
        }]);
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        assert_eq!(
            recv_selection_start(&selection_starts).device_id,
            first_id()
        );
        let loading = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Loading
        })
        .await;
        assert_eq!(loading.selection_generation(), OperationGeneration::new(1));
        assert!(loading.selected_lineup().channels().is_empty());

        release
            .send(Ok(ResolvedDeviceSnapshot::controller_test_fixture(
                first_id(),
            )))
            .unwrap();
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;

        assert_eq!(ready.selected_device(), Some(first_id()));
        assert_eq!(
            ready.devices()[0].friendly_name(),
            Some("private fixture name")
        );
        assert_eq!(ready.devices()[0].tuner_count(), Some(1));
        assert_eq!(ready.selected_lineup().channels().len(), 1);
        let channel = &ready.selected_lineup().channels()[0];
        assert_eq!(channel.key().device_id(), first_id());
        assert!(channel.is_favorite());
        assert!(channel.is_drm());
        assert!(channel.is_hd());
        let rendered = format!("{ready:?}");
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains("auto/v5.1"));
        controller.shutdown().unwrap();
    }

    #[test]
    fn resolver_failures_map_to_truthful_fixed_lineup_categories() {
        let unavailable =
            |kind| DeviceSnapshotResolutionError::controller_test_unavailable(first_id(), &[kind]);
        for (error, expected) in [
            (
                unavailable(DeviceSnapshotIssueKind::IdentityMismatch),
                LineupFailure::IdentityMismatch,
            ),
            (
                unavailable(DeviceSnapshotIssueKind::LineupInvalid),
                LineupFailure::InvalidLineup,
            ),
            (
                unavailable(DeviceSnapshotIssueKind::MetadataInvalid),
                LineupFailure::InvalidMetadata,
            ),
            (
                unavailable(DeviceSnapshotIssueKind::MetadataUnreachable),
                LineupFailure::Unreachable,
            ),
            (
                unavailable(DeviceSnapshotIssueKind::LineupUnreachable),
                LineupFailure::Unreachable,
            ),
            (
                DeviceSnapshotResolutionError::Deadline {
                    deadline: Duration::from_secs(1),
                },
                LineupFailure::Unreachable,
            ),
            (
                DeviceSnapshotResolutionError::Cancelled,
                LineupFailure::Internal,
            ),
        ] {
            assert_eq!(project_resolution_failure(&error, first_id(), 1), expected);
        }
        let wrong_identity = DeviceSnapshotResolutionError::controller_test_unavailable(
            second_id(),
            &[DeviceSnapshotIssueKind::IdentityMismatch],
        );
        assert_eq!(
            project_resolution_failure(&wrong_identity, first_id(), 1),
            LineupFailure::Internal
        );
        assert_eq!(
            project_resolution_failure(
                &unavailable(DeviceSnapshotIssueKind::UnsupportedEndpoint),
                first_id(),
                0,
            ),
            LineupFailure::NoSupportedLocator
        );
    }

    #[test]
    fn retained_snapshot_invariant_requires_exact_ready_identity() {
        let (discovery, _) = ScriptedService::new([]);
        let (selection, _) = ScriptedSelectionService::new([]);
        let (_commands, receiver) = mpsc::channel(1);
        let (snapshots, _snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        let mut actor = ControllerActor::new(
            Arc::new(discovery),
            Arc::new(selection),
            receiver,
            CancellationToken::new(),
            snapshots,
        );

        actor.selected_snapshot = Some(ResolvedDeviceSnapshot::controller_test_fixture(first_id()));
        assert_eq!(
            actor.validate_selection_retention(),
            Err(ControllerRuntimeError::SelectionSnapshotInvariant)
        );
        actor.selected_device = Some(first_id());
        actor.selected_lineup =
            SelectedLineupState::ready(first_id(), OperationGeneration::INITIAL, []).unwrap();
        assert_eq!(actor.validate_selection_retention(), Ok(()));
        actor.selected_snapshot =
            Some(ResolvedDeviceSnapshot::controller_test_fixture(second_id()));
        assert_eq!(
            actor.validate_selection_retention(),
            Err(ControllerRuntimeError::SelectionSnapshotInvariant)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clear_cancels_and_joins_selection_before_publishing_unselected() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "192.0.2.10:65001", 4),
        ))]);
        let (cancellation_observed, cancellation_observed_rx) = std_mpsc::channel();
        let (finish_cancellation, finish_cancellation_rx) = oneshot::channel();
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::CancellationBarrier {
                cancellation_observed,
                finish_cancellation: finish_cancellation_rx,
                cancellation_result: Ok(
                    ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
                ),
            }]);
        let observed_selection = selection.clone();
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        recv_selection_start(&selection_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Loading
        })
        .await;
        handle.try_send(ControllerCommand::ClearSelection).unwrap();
        cancellation_observed_rx
            .recv_timeout(WAIT)
            .expect("clear should cancel selected-device work");
        assert_eq!(
            handle.snapshot().selected_lineup().status(),
            SelectedLineupStatus::Loading
        );

        finish_cancellation.send(()).unwrap();
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(2)
                && snapshot.selected_lineup().status() == SelectedLineupStatus::Unselected
        })
        .await;
        assert_eq!(cleared.selected_device(), None);
        assert_eq!(observed_selection.maximum_active(), 1);
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
    async fn failed_refresh_preserves_ready_selection_and_retained_metadata() {
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Err(DiscoveryFailure::Network)),
        ]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
            ))]);
        let observed_selection = selection.clone();
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        recv_selection_start(&selection_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        let failed = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Failed(DiscoveryFailure::Network)
        })
        .await;

        assert_eq!(failed.selection_generation(), OperationGeneration::new(1));
        assert_eq!(
            failed.selected_lineup().status(),
            SelectedLineupStatus::Ready
        );
        assert_eq!(
            failed.devices()[0].friendly_name(),
            Some("private fixture name")
        );
        assert_eq!(observed_selection.calls(), 1);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_refresh_cancels_joins_and_reresolves_selection() {
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.20:65001", 4))),
        ]);
        let (cancellation_observed, cancellation_observed_rx) = std_mpsc::channel();
        let (finish_cancellation, finish_cancellation_rx) = oneshot::channel();
        let (selection, selection_starts) = ScriptedSelectionService::new([
            SelectionStep::CancellationBarrier {
                cancellation_observed,
                finish_cancellation: finish_cancellation_rx,
                cancellation_result: Ok(
                    ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
                ),
            },
            SelectionStep::Immediate(Ok(ResolvedDeviceSnapshot::controller_test_fixture(
                first_id(),
            ))),
        ]);
        let observed_selection = selection.clone();
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        assert_eq!(recv_selection_start(&selection_starts).call, 1);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Loading
        })
        .await;

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(recv_start(&discovery_starts).call, 2);
        cancellation_observed_rx
            .recv_timeout(WAIT)
            .expect("successful refresh should cancel the stale selected target");
        let blocked = handle.snapshot();
        assert_eq!(blocked.discovery().status(), DiscoveryStatus::Refreshing);
        assert_eq!(blocked.selection_generation(), OperationGeneration::new(1));

        finish_cancellation.send(()).unwrap();
        assert_eq!(recv_selection_start(&selection_starts).call, 2);
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
                && snapshot.selection_generation() == OperationGeneration::new(2)
                && snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;

        assert_eq!(
            ready.devices()[0].preferred_locator(),
            "192.0.2.20:65001".parse().unwrap()
        );
        assert_eq!(observed_selection.calls(), 2);
        assert_eq!(observed_selection.maximum_active(), 1);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_refresh_clears_selection_when_device_disappears() {
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
        ]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
            ))]);
        let observed_selection = selection.clone();
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        recv_selection_start(&selection_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
                && snapshot.selection_generation() == OperationGeneration::new(2)
        })
        .await;

        assert!(cleared.devices().is_empty());
        assert_eq!(cleared.selected_device(), None);
        assert_eq!(
            cleared.selected_lineup().status(),
            SelectedLineupStatus::Unselected
        );
        assert_eq!(observed_selection.calls(), 1);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn selected_device_task_panic_projects_internal_failure() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "192.0.2.10:65001", 4),
        ))]);
        let (selection, selection_starts) = ScriptedSelectionService::new([SelectionStep::Panic]);
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        recv_selection_start(&selection_starts);
        let failed = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status()
                == SelectedLineupStatus::Failed(LineupFailure::Internal)
        })
        .await;

        assert!(failed.selected_lineup().channels().is_empty());
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

    #[tokio::test(flavor = "current_thread")]
    async fn stale_completion_cannot_change_registry_or_snapshot() {
        let (service, _) = ScriptedService::new([]);
        let (selection, _) = ScriptedSelectionService::new([]);
        let (_commands, receiver) = mpsc::channel(1);
        let (snapshots, snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        let mut actor = ControllerActor::new(
            Arc::new(service),
            Arc::new(selection),
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
                .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_cancels_both_operation_lanes_before_joining() {
        let (discovery_cancelled, discovery_cancelled_rx) = std_mpsc::channel();
        let (finish_discovery, finish_discovery_rx) = oneshot::channel();
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::CancellationBarrier {
                cancellation_observed: discovery_cancelled,
                finish_cancellation: finish_discovery_rx,
                cancellation_result: Err(DiscoveryFailure::Internal),
            },
        ]);
        let (selection_cancelled, selection_cancelled_rx) = std_mpsc::channel();
        let (finish_selection, finish_selection_rx) = oneshot::channel();
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::CancellationBarrier {
                cancellation_observed: selection_cancelled,
                finish_cancellation: finish_selection_rx,
                cancellation_result: Err(DeviceSnapshotResolutionError::Cancelled),
            }]);
        let controller = ControllerRuntime::start_with_test_services(discovery, selection).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        recv_selection_start(&selection_starts);
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&discovery_starts);

        controller.begin_shutdown();
        discovery_cancelled_rx
            .recv_timeout(WAIT)
            .expect("shutdown should cancel discovery");
        selection_cancelled_rx
            .recv_timeout(WAIT)
            .expect("shutdown should cancel selected-device resolution");
        finish_discovery.send(()).unwrap();
        finish_selection.send(()).unwrap();
        controller.join().unwrap();
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
