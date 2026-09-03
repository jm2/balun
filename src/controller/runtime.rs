//! Packet-free controller-thread ownership and command admission.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::runtime::Builder;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::resolution::HostnameResolutionReceiver;
use super::routed::{
    RoutedDiscoveryService, RoutedOriginsReceiver, RoutedProposal, RoutedRunOutcome,
    UnavailableRoutedDiscovery,
};
use super::{
    ApplicationSnapshot, ChannelSummary, DeviceSummary, DiscoveryFailure, DiscoveryKind,
    DiscoveryState, DiscoveryStatus, LineupFailure, OperationGeneration, SelectedLineupState,
    SelectedLineupStatus, SnapshotRevision, StateError, StreamHandoff, StreamHandoffError,
    StreamHandoffReceiver, StreamSelection,
};
use super::{
    RoutedApprovalToken, RoutedAvailability, RoutedDiscoveryState, RoutedProposalState,
    RoutedProposalStatus, RoutedUnavailableReason,
};
use crate::discovery::{
    DeviceRegistry, DiscoveryClient, DiscoveryError, DiscoveryMethod, DiscoveryObservation,
    DiscoveryReport, ExactDiscoveryTarget, HostnameResolutionError, HostnameTarget, ProbeConfig,
    RegistryInstant, resolve_hostname,
};
use crate::discovery::{MAX_ROUTED_CANDIDATES, RoutedProposalOriginSummary, RoutedScanTrigger};
use crate::domain::{ChannelKey, DeviceId};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;
use crate::hdhr::{
    DeviceEndpoint, DeviceSnapshotIssueKind, DeviceSnapshotResolutionError, DeviceSnapshotResolver,
    DeviceSnapshotTarget, DeviceSnapshotTargetError, ResolvedDeviceSnapshot,
};

/// Name assigned to Balun's GTK-independent controller thread.
pub const CONTROLLER_THREAD_NAME: &str = "balun-controller";
/// Default upper bound for commands waiting to enter the controller actor.
pub const DEFAULT_COMMAND_CAPACITY: usize = 8;
/// Largest command queue accepted by the controller constructor.
pub const MAX_COMMAND_CAPACITY: usize = 1_024;
/// Maximum distinct exact addresses admitted during one controller session.
pub const MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION: usize = 32;

const EXACT_DISCOVERY_ATTEMPTS: u8 = 2;
const EXACT_DISCOVERY_RESPONSE_WINDOW: Duration = Duration::from_millis(200);
const EXACT_DISCOVERY_MAX_RECEIVED_DATAGRAMS: usize = 16;
const EXACT_DISCOVERY_MAX_UNIQUE_DEVICES: usize = 1;
const MAX_RETAINED_LOCAL_OBSERVATIONS: usize = match DeviceRegistry::DEFAULT_MAX_DEVICES
    .checked_mul(DeviceRegistry::DEFAULT_MAX_LOCATORS_PER_DEVICE)
{
    Some(limit) => limit,
    None => panic!("default discovery registry limits must have a representable product"),
};
const MAX_RETAINED_EXACT_OBSERVATIONS: usize = 1;
const MAX_RETAINED_ROUTED_OBSERVATIONS: usize = MAX_ROUTED_CANDIDATES;
/// Private approval-store directory under the per-user settings directory.
const ROUTED_APPROVAL_DIRECTORY: &str = "routed-approvals";

/// Owned, `'static` future returned by an injected discovery service.
pub type DiscoveryFuture =
    Pin<Box<dyn Future<Output = Result<DiscoveryReport, DiscoveryFailure>> + Send + 'static>>;

/// Async discovery behind a packet-free controller boundary.
///
/// The controller invokes this service only after admitting an explicit local
/// or exact-address command. Implementations must observe `cancellation`
/// promptly. In particular, constructing the service or controller must not
/// enumerate interfaces, open sockets, or send packets. A production
/// implementation is also the trusted traffic boundary: one exact operation
/// must send no more than two request datagrams, use response windows no longer
/// than 200 ms each, inspect no more than 16 received datagrams, and accept no
/// more than one device identity.
pub trait DiscoveryService: Send + Sync + 'static {
    fn discover_local(&self, cancellation: CancellationToken) -> DiscoveryFuture;

    /// Probe one already-validated exact address.
    ///
    /// Implementations must apply `expected_device` during discovery when it
    /// is present. The returned report may contain zero or one observation. An
    /// observation must be a direct targeted reply from `target` on the
    /// HDHomeRun discovery port, with no interface annotation; the controller
    /// independently checks those properties before retention.
    fn discover_exact(
        &self,
        target: ExactDiscoveryTarget,
        expected_device: Option<DeviceId>,
        cancellation: CancellationToken,
    ) -> DiscoveryFuture;
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

impl DiscoveryService for DiscoveryClient {
    fn discover_local(&self, cancellation: CancellationToken) -> DiscoveryFuture {
        let client = self.clone();
        Box::pin(async move {
            DiscoveryClient::discover_local(&client, &cancellation)
                .await
                .map_err(discovery_failure)
        })
    }

    fn discover_exact(
        &self,
        target: ExactDiscoveryTarget,
        expected_device: Option<DeviceId>,
        cancellation: CancellationToken,
    ) -> DiscoveryFuture {
        let client = DiscoveryClient::new(exact_probe_config());
        Box::pin(async move {
            DiscoveryClient::discover_target(
                &client,
                target.socket_addr(),
                expected_device,
                &cancellation,
            )
            .await
            .map_err(discovery_failure)
        })
    }
}

/// The production routed service: the Linux supervisor over the private
/// approval store beside the settings file, or a fixed reason it is
/// unavailable. Starting it performs no I/O.
#[cfg(target_os = "linux")]
fn default_routed_service() -> Arc<dyn RoutedDiscoveryService> {
    let Some(directory) = crate::settings::default_directory() else {
        return Arc::new(UnavailableRoutedDiscovery::new(
            RoutedUnavailableReason::NoPrivateDirectory,
        ));
    };
    match super::routed::LinuxRoutedDiscovery::start(directory.join(ROUTED_APPROVAL_DIRECTORY)) {
        Ok(service) => Arc::new(service),
        Err(_) => Arc::new(UnavailableRoutedDiscovery::new(
            RoutedUnavailableReason::ObserversUnavailable,
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn default_routed_service() -> Arc<dyn RoutedDiscoveryService> {
    Arc::new(UnavailableRoutedDiscovery::new(
        RoutedUnavailableReason::UnsupportedPlatform,
    ))
}

fn exact_probe_config() -> ProbeConfig {
    ProbeConfig::new(
        EXACT_DISCOVERY_ATTEMPTS,
        EXACT_DISCOVERY_RESPONSE_WINDOW,
        EXACT_DISCOVERY_MAX_RECEIVED_DATAGRAMS,
        EXACT_DISCOVERY_MAX_UNIQUE_DEVICES,
    )
    .expect("fixed exact-discovery probe budget must be valid")
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
    /// Supersede any current discovery operation and run ordinary local discovery.
    RefreshLocalDiscovery,
    /// Supersede any current discovery operation and probe one exact address.
    DiscoverExact(ExactDiscoveryTarget),
    /// Cancel the current discovery operation without discarding last-good devices.
    CancelDiscovery,
    /// Resolve and retain exactly this registered device's lineup.
    SelectDevice(DeviceId),
    /// Cancel selected-device work and discard its retained snapshot.
    ClearSelection,
    /// Build a fresh routed proposal from the current tunnel routes; sends nothing.
    ProposeRoutedDiscovery,
    /// Remember approval of the routed proposal identified by this token.
    ApproveRoutedDiscovery(RoutedApprovalToken),
    /// Supersede any current discovery operation and run the approved routed scan.
    RunRoutedDiscovery(RoutedScanTrigger),
    /// Forget every remembered routed approval.
    RevokeRoutedApprovals,
}

/// Private queue payload. Stream replies deliberately cannot enter the public
/// command enum or immutable GTK-facing snapshot channel.
enum ActorCommand {
    Controller(ControllerCommand),
    RequestStream {
        selection: StreamSelection,
        reply: oneshot::Sender<Result<StreamHandoff, StreamHandoffError>>,
    },
    ResolveHostname {
        target: HostnameTarget,
        reply: oneshot::Sender<Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError>>,
    },
    RoutedProposalOrigins {
        token: RoutedApprovalToken,
        reply: oneshot::Sender<Result<Vec<RoutedProposalOriginSummary>, DiscoveryFailure>>,
    },
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
    #[error("discovery generation is exhausted")]
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
    commands: mpsc::Sender<ActorCommand>,
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
    /// Start an inert controller with Balun's production discovery client.
    /// No network work begins until an explicit local or exact command.
    pub fn start_default() -> Result<Self, ControllerStartError> {
        Self::start_with_services_and_capacity(
            DiscoveryClient::default(),
            DeviceSnapshotResolver::default(),
            default_routed_service(),
            DEFAULT_COMMAND_CAPACITY,
        )
    }

    /// Start an inert controller with the default bounded command capacity.
    ///
    /// When `service` is a [`DiscoveryClient`], its configured probe budget is
    /// used for local discovery. Exact-address work deliberately uses Balun's
    /// stricter fixed traffic budget instead.
    pub fn start<S>(service: S) -> Result<Self, ControllerStartError>
    where
        S: DiscoveryService,
    {
        Self::start_with_capacity(service, DEFAULT_COMMAND_CAPACITY)
    }

    /// Start an inert controller with an explicit bounded command capacity.
    ///
    /// When `service` is a [`DiscoveryClient`], its configured probe budget is
    /// used for local discovery. Exact-address work deliberately uses Balun's
    /// stricter fixed traffic budget instead.
    pub fn start_with_capacity<S>(
        service: S,
        command_capacity: usize,
    ) -> Result<Self, ControllerStartError>
    where
        S: DiscoveryService,
    {
        Self::start_with_services_and_capacity(
            service,
            DeviceSnapshotResolver::default(),
            Arc::new(UnavailableRoutedDiscovery::new(
                RoutedUnavailableReason::NotConfigured,
            )),
            command_capacity,
        )
    }

    fn start_with_services_and_capacity<D, S>(
        discovery_service: D,
        selection_service: S,
        routed_service: Arc<dyn RoutedDiscoveryService>,
        command_capacity: usize,
    ) -> Result<Self, ControllerStartError>
    where
        D: DiscoveryService,
        S: SelectedDeviceService,
    {
        if !(1..=MAX_COMMAND_CAPACITY).contains(&command_capacity) {
            return Err(ControllerStartError::InvalidCommandCapacity {
                value: command_capacity,
                maximum: MAX_COMMAND_CAPACITY,
            });
        }

        let discovery_service: Arc<dyn DiscoveryService> = Arc::new(discovery_service);
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
                    routed_service,
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
    fn start_with_test_services<D, S, R>(
        discovery_service: D,
        selection_service: S,
        routed_service: R,
    ) -> Result<Self, ControllerStartError>
    where
        D: DiscoveryService,
        S: SelectedDeviceService,
        R: RoutedDiscoveryService,
    {
        Self::start_with_services_and_capacity(
            discovery_service,
            selection_service,
            Arc::new(routed_service),
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
        self.try_send_actor(ActorCommand::Controller(command))
    }

    /// Admit one private, one-shot stream request into the controller FIFO.
    ///
    /// The actor resolves the URL-free selection only against the matching
    /// current complete selected snapshot. No URL is published through [`Self::subscribe`] or
    /// [`Self::snapshot`], and this call performs no HTTP or tuner work.
    pub fn try_request_stream(
        &self,
        selection: StreamSelection,
    ) -> Result<StreamHandoffReceiver, ControllerCommandError> {
        let (reply, receiver) = oneshot::channel();
        self.try_send_actor(ActorCommand::RequestStream { selection, reply })?;
        Ok(StreamHandoffReceiver::new(receiver))
    }

    /// Resolve one validated hostname on the controller runtime into at most
    /// a few usable exact targets, delivered only to this caller.
    ///
    /// The lookup is bounded by the resolver timeout, never enters the
    /// shared snapshot, and grants no scan authority: each result is an
    /// ordinary exact target the caller may submit one at a time.
    pub fn try_resolve_hostname(
        &self,
        target: HostnameTarget,
    ) -> Result<HostnameResolutionReceiver, ControllerCommandError> {
        let (reply, receiver) = oneshot::channel();
        self.try_send_actor(ActorCommand::ResolveHostname { target, reply })?;
        Ok(HostnameResolutionReceiver::new(receiver))
    }

    /// Ask for the origins behind the routed proposal identified by `token`.
    ///
    /// The origins name tunnel interfaces and networks, so they travel only
    /// through this private reply and never through the snapshot channel.
    pub fn try_routed_proposal_origins(
        &self,
        token: RoutedApprovalToken,
    ) -> Result<RoutedOriginsReceiver, ControllerCommandError> {
        let (reply, receiver) = oneshot::channel();
        self.try_send_actor(ActorCommand::RoutedProposalOrigins { token, reply })?;
        Ok(RoutedOriginsReceiver::new(receiver))
    }

    fn try_send_actor(&self, command: ActorCommand) -> Result<(), ControllerCommandError> {
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
    discovery_service: Arc<dyn DiscoveryService>,
    selection_service: Arc<dyn SelectedDeviceService>,
    routed_service: Arc<dyn RoutedDiscoveryService>,
    commands: mpsc::Receiver<ActorCommand>,
    shutdown: CancellationToken,
    snapshots: watch::Sender<Arc<ApplicationSnapshot>>,
    registry: DeviceRegistry,
    registry_epoch: Instant,
    local_batch: Option<RetainedDiscoveryBatch>,
    routed_batch: Option<RetainedDiscoveryBatch>,
    attempted_exact_targets: BTreeSet<ExactDiscoveryTarget>,
    exact_sources: BTreeMap<ExactDiscoveryTarget, RetainedExactSource>,
    revision: SnapshotRevision,
    discovery_generation: OperationGeneration,
    selection_generation: OperationGeneration,
    discovery: DiscoveryState,
    devices: Vec<DeviceSummary>,
    selected_device: Option<DeviceId>,
    selected_lineup: SelectedLineupState,
    selected_snapshot: Option<RetainedSelectedSnapshot>,
    active_discovery: Option<ActiveDiscovery>,
    active_selection: Option<ActiveSelection>,
    routed: RoutedDiscoveryState,
    active_routed_control: Option<ActiveRoutedControl>,
}

impl ControllerActor {
    fn new(
        discovery_service: Arc<dyn DiscoveryService>,
        selection_service: Arc<dyn SelectedDeviceService>,
        routed_service: Arc<dyn RoutedDiscoveryService>,
        commands: mpsc::Receiver<ActorCommand>,
        shutdown: CancellationToken,
        snapshots: watch::Sender<Arc<ApplicationSnapshot>>,
    ) -> Self {
        let availability = match routed_service.availability() {
            Ok(()) => RoutedAvailability::Available,
            Err(reason) => RoutedAvailability::Unavailable(reason),
        };
        Self {
            discovery_service,
            selection_service,
            routed_service,
            commands,
            shutdown,
            snapshots,
            registry: DeviceRegistry::default(),
            registry_epoch: Instant::now(),
            local_batch: None,
            routed_batch: None,
            attempted_exact_targets: BTreeSet::new(),
            exact_sources: BTreeMap::new(),
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
            routed: RoutedDiscoveryState::new(availability, RoutedProposalStatus::None, None),
            active_routed_control: None,
        }
    }

    async fn run(mut self) -> Result<(), ControllerRuntimeError> {
        loop {
            let event = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => ActorEvent::Shutdown,
                command = self.commands.recv() => ActorEvent::Command(command),
                completion = join_optional(
                    self.active_discovery.as_mut().map(|active| &mut active.task),
                ) => ActorEvent::Discovery(completion),
                completion = join_optional(
                    self.active_selection.as_mut().map(|active| &mut active.task),
                ) => ActorEvent::Selection(completion),
                completion = join_optional(
                    self.active_routed_control.as_mut().map(|active| &mut active.task),
                ) => ActorEvent::RoutedControl(completion),
            };

            match event {
                ActorEvent::Shutdown | ActorEvent::Command(None) => {
                    self.cancel_all_operations().await;
                    return Ok(());
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::RefreshLocalDiscovery,
                ))) => {
                    self.start_discovery(DiscoveryScope::Local).await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::DiscoverExact(target),
                ))) => {
                    self.start_discovery(DiscoveryScope::Exact(target)).await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::CancelDiscovery,
                ))) => {
                    self.cancel_discovery().await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::SelectDevice(device_id),
                ))) => {
                    self.select_device(device_id).await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::ClearSelection,
                ))) => {
                    self.clear_selection().await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::ProposeRoutedDiscovery,
                ))) => {
                    self.propose_routed().await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::ApproveRoutedDiscovery(token),
                ))) => {
                    self.approve_routed(token).await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::RunRoutedDiscovery(trigger),
                ))) => {
                    self.start_discovery(DiscoveryScope::Routed(trigger))
                        .await?;
                }
                ActorEvent::Command(Some(ActorCommand::Controller(
                    ControllerCommand::RevokeRoutedApprovals,
                ))) => {
                    self.revoke_routed().await?;
                }
                ActorEvent::Command(Some(ActorCommand::RoutedProposalOrigins { token, reply })) => {
                    let service = Arc::clone(&self.routed_service);
                    let cancellation = self.shutdown.child_token();
                    tokio::spawn(async move {
                        let _ = reply.send(service.origins(token, cancellation).await);
                    });
                }
                ActorEvent::Command(Some(ActorCommand::RequestStream { selection, reply })) => {
                    let _ = reply.send(self.resolve_stream_handoff(selection));
                }
                ActorEvent::Command(Some(ActorCommand::ResolveHostname { target, reply })) => {
                    // Resolution runs beside the actor so a slow resolver never
                    // delays commands; shutdown abandons it with a fixed error.
                    let shutdown = self.shutdown.clone();
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            biased;
                            () = shutdown.cancelled() => {
                                Err(HostnameResolutionError::ControllerStopped)
                            }
                            result = resolve_hostname(&target) => result,
                        };
                        let _ = reply.send(result);
                    });
                }
                ActorEvent::Discovery(completion) => {
                    self.finish_discovery(completion).await?;
                }
                ActorEvent::Selection(completion) => {
                    self.finish_selection(completion)?;
                }
                ActorEvent::RoutedControl(completion) => {
                    self.finish_routed_control(completion)?;
                }
            }
        }
    }

    /// Build a fresh routed proposal beside the discovery lane.
    async fn propose_routed(&mut self) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_routed_control().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        self.set_routed_proposal(RoutedProposalStatus::Proposing)?;
        let cancellation = self.shutdown.child_token();
        let service = Arc::clone(&self.routed_service);
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            RoutedControlCompletion::Proposed(service.propose(task_cancellation).await)
        });
        self.active_routed_control = Some(ActiveRoutedControl { cancellation, task });
        Ok(())
    }

    /// Remember approval of the proposal currently shown.
    async fn approve_routed(
        &mut self,
        token: RoutedApprovalToken,
    ) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_routed_control().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        let shown = matches!(self.routed.proposal(), RoutedProposalStatus::Proposed(state)
            if state.token() == token);
        if !shown {
            return self.set_routed_proposal(RoutedProposalStatus::Failed(
                DiscoveryFailure::RoutedProposalChanged,
            ));
        }
        let cancellation = self.shutdown.child_token();
        let service = Arc::clone(&self.routed_service);
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            RoutedControlCompletion::Approved {
                token,
                result: service.approve(token, task_cancellation).await,
            }
        });
        self.active_routed_control = Some(ActiveRoutedControl { cancellation, task });
        Ok(())
    }

    /// Forget every remembered routed approval.
    async fn revoke_routed(&mut self) -> Result<(), ControllerRuntimeError> {
        self.cancel_active_routed_control().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        let cancellation = self.shutdown.child_token();
        let service = Arc::clone(&self.routed_service);
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            RoutedControlCompletion::Revoked(service.revoke_all(task_cancellation).await)
        });
        self.active_routed_control = Some(ActiveRoutedControl { cancellation, task });
        Ok(())
    }

    async fn cancel_active_routed_control(&mut self) {
        let Some(active) = self.active_routed_control.take() else {
            return;
        };
        active.cancellation.cancel();
        let _ = active.task.await;
    }

    fn finish_routed_control(
        &mut self,
        completion: Result<RoutedControlCompletion, tokio::task::JoinError>,
    ) -> Result<(), ControllerRuntimeError> {
        if self.active_routed_control.take().is_none() {
            return Ok(());
        }
        let proposal = match completion {
            Ok(RoutedControlCompletion::Proposed(Ok(proposal))) => {
                let summary = proposal.summary();
                RoutedProposalStatus::Proposed(RoutedProposalState::new(
                    proposal.token(),
                    summary.candidate_count(),
                    summary.maximum_request_datagrams(),
                    summary.wire_datagrams_per_second(),
                    summary.max_in_flight(),
                    summary.overall_deadline(),
                    summary.origins().len(),
                ))
            }
            Ok(RoutedControlCompletion::Approved { token, result }) => {
                match (self.routed.proposal(), result) {
                    (RoutedProposalStatus::Proposed(state), Ok(())) if state.token() == token => {
                        RoutedProposalStatus::Proposed(state.with_approved(true))
                    }
                    (_, Ok(())) => {
                        RoutedProposalStatus::Failed(DiscoveryFailure::RoutedProposalChanged)
                    }
                    (_, Err(failure)) => RoutedProposalStatus::Failed(failure),
                }
            }
            Ok(RoutedControlCompletion::Revoked(Ok(()))) => RoutedProposalStatus::None,
            Ok(
                RoutedControlCompletion::Proposed(Err(failure))
                | RoutedControlCompletion::Revoked(Err(failure)),
            ) => RoutedProposalStatus::Failed(failure),
            Err(_) => RoutedProposalStatus::Failed(DiscoveryFailure::Internal),
        };
        self.set_routed_proposal(proposal)
    }

    fn set_routed_proposal(
        &mut self,
        proposal: RoutedProposalStatus,
    ) -> Result<(), ControllerRuntimeError> {
        let cooldown = match proposal {
            RoutedProposalStatus::None => None,
            _ => self.routed.cooldown_seconds(),
        };
        self.routed = RoutedDiscoveryState::new(self.routed.availability(), proposal, cooldown);
        self.publish()
    }

    async fn start_discovery(
        &mut self,
        scope: DiscoveryScope,
    ) -> Result<(), ControllerRuntimeError> {
        let exact_target_limit_reached = matches!(scope, DiscoveryScope::Exact(target)
            if !self.attempted_exact_targets.contains(&target)
                && self.attempted_exact_targets.len()
                    >= MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION);

        self.cancel_active_discovery().await;
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        let generation = self.next_discovery_generation()?;
        if exact_target_limit_reached {
            self.discovery = DiscoveryState::failed_for(
                generation,
                DiscoveryKind::Exact,
                DiscoveryFailure::ExactTargetLimitReached,
            );
            self.publish()?;
            return Ok(());
        }
        if let DiscoveryScope::Exact(target) = scope {
            self.attempted_exact_targets.insert(target);
        }

        let expected_device = match scope {
            DiscoveryScope::Local | DiscoveryScope::Routed(_) => None,
            DiscoveryScope::Exact(target) => self.expected_device_for_exact_target(target),
        };
        if matches!(scope, DiscoveryScope::Routed(_)) {
            self.routed =
                RoutedDiscoveryState::new(self.routed.availability(), self.routed.proposal(), None);
        }
        self.discovery = DiscoveryState::refreshing_for(generation, scope.kind());
        self.publish()?;

        let cancellation = self.shutdown.child_token();
        let service = Arc::clone(&self.discovery_service);
        let routed_service = Arc::clone(&self.routed_service);
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let (result, cooldown) = if task_cancellation.is_cancelled() {
                (Err(DiscoveryFailure::Internal), None)
            } else {
                match scope {
                    DiscoveryScope::Local => {
                        (service.discover_local(task_cancellation).await, None)
                    }
                    DiscoveryScope::Exact(target) => (
                        service
                            .discover_exact(target, expected_device, task_cancellation)
                            .await,
                        None,
                    ),
                    DiscoveryScope::Routed(trigger) => {
                        match routed_service.run(trigger, task_cancellation).await {
                            Ok(RoutedRunOutcome::Report(report)) => (Ok(report), None),
                            Ok(RoutedRunOutcome::NeedsApproval) => {
                                (Err(DiscoveryFailure::RoutedNotApproved), None)
                            }
                            Ok(RoutedRunOutcome::CoolingDown { remaining }) => {
                                (Err(DiscoveryFailure::RoutedCoolingDown), Some(remaining))
                            }
                            Ok(RoutedRunOutcome::Busy) => (Err(DiscoveryFailure::RoutedBusy), None),
                            Ok(RoutedRunOutcome::Unconfirmed) => {
                                (Err(DiscoveryFailure::RoutedUnconfirmed), None)
                            }
                            Err(failure) => (Err(failure), None),
                        }
                    }
                }
            };
            DiscoveryCompletion {
                generation,
                scope,
                result,
                cooldown,
            }
        });
        self.active_discovery = Some(ActiveDiscovery {
            generation,
            scope,
            cancellation,
            task,
        });
        Ok(())
    }

    async fn cancel_discovery(&mut self) -> Result<(), ControllerRuntimeError> {
        let Some(kind) = self
            .active_discovery
            .as_ref()
            .map(|active| active.scope.kind())
        else {
            return Ok(());
        };
        self.cancel_active_discovery().await;
        let generation = self.next_discovery_generation()?;
        self.discovery = DiscoveryState::idle_for(generation, kind);
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
        let routed_control = self.active_routed_control.take();
        if let Some(active) = &discovery {
            active.cancellation.cancel();
        }
        if let Some(active) = &selection {
            active.cancellation.cancel();
        }
        if let Some(active) = routed_control {
            active.cancellation.cancel();
            let _ = active.task.await;
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

    async fn finish_discovery(
        &mut self,
        completion: Result<DiscoveryCompletion, tokio::task::JoinError>,
    ) -> Result<(), ControllerRuntimeError> {
        let Some(active) = self.active_discovery.take() else {
            return Ok(());
        };
        let completion = match completion {
            Ok(completion)
                if completion.generation == active.generation
                    && completion.scope == active.scope =>
            {
                completion
            }
            Ok(_) | Err(_) => DiscoveryCompletion {
                generation: active.generation,
                scope: active.scope,
                result: Err(DiscoveryFailure::Internal),
                cooldown: None,
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
            || self.discovery.kind() != completion.scope.kind()
        {
            return Ok(false);
        }
        if let Some(remaining) = completion.cooldown {
            let seconds = u16::try_from(remaining.as_secs()).unwrap_or(u16::MAX);
            self.routed = RoutedDiscoveryState::new(
                self.routed.availability(),
                self.routed.proposal(),
                Some(seconds),
            );
        }

        match completion.result {
            Ok(report) => {
                let issue_count = u16::try_from(report.issues.len()).unwrap_or(u16::MAX);
                let no_response = report.observations.is_empty();
                match self.build_discovery_update(completion.scope, report) {
                    Ok(mut update) => match completion.scope {
                        DiscoveryScope::Local => {
                            let selected_device = self.selected_device;
                            if selected_device.is_some() {
                                self.cancel_active_selection().await;
                                if self.shutdown.is_cancelled() {
                                    return Ok(false);
                                }
                            }

                            self.commit_discovery_update(update);
                            self.discovery = DiscoveryState::ready_for(
                                self.discovery_generation,
                                DiscoveryKind::Local,
                                issue_count,
                            );
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
                        DiscoveryScope::Exact(_) | DiscoveryScope::Routed(_) => {
                            let kind = completion.scope.kind();
                            let selected_changed = self.selected_device.is_some_and(|device_id| {
                                self.registry.get(device_id) != update.registry.get(device_id)
                            });
                            if selected_changed {
                                self.cancel_active_selection().await;
                                if self.shutdown.is_cancelled() {
                                    return Ok(false);
                                }
                            } else if let Some(device_id) = self.selected_device {
                                preserve_device_summary(
                                    &self.devices,
                                    &mut update.devices,
                                    device_id,
                                )?;
                            }

                            self.commit_discovery_update(update);
                            self.discovery = if no_response {
                                DiscoveryState::no_response_for(
                                    self.discovery_generation,
                                    kind,
                                    issue_count,
                                )
                            } else {
                                DiscoveryState::ready_for(
                                    self.discovery_generation,
                                    kind,
                                    issue_count,
                                )
                            };
                            if selected_changed {
                                let generation = self.next_selection_generation()?;
                                self.selected_device = None;
                                self.selected_lineup = SelectedLineupState::unselected(generation);
                                self.selected_snapshot = None;
                            }
                            self.publish()?;
                            return Ok(true);
                        }
                    },
                    Err(()) => {
                        self.discovery = DiscoveryState::failed_for(
                            self.discovery_generation,
                            completion.scope.kind(),
                            DiscoveryFailure::Internal,
                        );
                    }
                }
            }
            Err(failure) => {
                self.discovery = DiscoveryState::failed_for(
                    self.discovery_generation,
                    completion.scope.kind(),
                    failure,
                );
            }
        }
        self.publish()?;
        Ok(true)
    }

    fn build_discovery_update(
        &self,
        scope: DiscoveryScope,
        report: DiscoveryReport,
    ) -> Result<DiscoveryUpdate, ()> {
        let observation_limit = match scope {
            DiscoveryScope::Local => MAX_RETAINED_LOCAL_OBSERVATIONS,
            DiscoveryScope::Exact(_) => MAX_RETAINED_EXACT_OBSERVATIONS,
            DiscoveryScope::Routed(_) => MAX_RETAINED_ROUTED_OBSERVATIONS,
        };
        if report.observations.len() > observation_limit {
            return Err(());
        }

        let seen_at = RegistryInstant::from_duration(self.registry_epoch.elapsed());
        let batch = RetainedDiscoveryBatch::new(seen_at, report.observations);
        let mut local_batch = self.local_batch.clone();
        let mut routed_batch = self.routed_batch.clone();
        let mut exact_sources = self.exact_sources.clone();
        match scope {
            DiscoveryScope::Local => local_batch = batch,
            DiscoveryScope::Routed(_) => {
                if let Some(batch) = &batch {
                    validate_routed_batch(batch)?;
                }
                routed_batch = batch;
            }
            DiscoveryScope::Exact(target) => match batch {
                Some(batch) => {
                    if !exact_sources.contains_key(&target)
                        && exact_sources.len() >= MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION
                    {
                        return Err(());
                    }
                    exact_sources
                        .entry(target)
                        .or_default()
                        .replace_batch(target, Some(batch))?;
                }
                None => {
                    if let Some(source) = exact_sources.get_mut(&target) {
                        source.replace_batch(target, None)?;
                    }
                }
            },
        }

        let registry =
            rebuild_registry(local_batch.as_ref(), routed_batch.as_ref(), &exact_sources)?;
        let devices = project_devices(&registry)?;
        Ok(DiscoveryUpdate {
            local_batch,
            routed_batch,
            exact_sources,
            registry,
            devices,
        })
    }

    fn commit_discovery_update(&mut self, update: DiscoveryUpdate) {
        self.local_batch = update.local_batch;
        self.routed_batch = update.routed_batch;
        self.exact_sources = update.exact_sources;
        self.registry = update.registry;
        self.devices = update.devices;
    }

    fn expected_device_for_exact_target(&self, target: ExactDiscoveryTarget) -> Option<DeviceId> {
        self.exact_sources
            .get(&target)
            .and_then(|source| source.bound_device)
            .or_else(|| self.registry_owner_for_exact_target(target))
    }

    fn registry_owner_for_exact_target(&self, target: ExactDiscoveryTarget) -> Option<DeviceId> {
        let mut expected_source = target.socket_addr();
        expected_source.set_port(DISCOVERY_UDP_PORT);
        self.registry.devices().find_map(|device| {
            device
                .locators()
                .any(|locator| locator.source() == expected_source)
                .then_some(device.device_id())
        })
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
        self.selected_snapshot = Some(RetainedSelectedSnapshot {
            generation: self.selection_generation,
            resolved,
        });
        Ok(())
    }

    fn resolve_stream_handoff(
        &self,
        selection: StreamSelection,
    ) -> Result<StreamHandoff, StreamHandoffError> {
        let (key, expected_generation) = selection.into_parts();
        if expected_generation != self.selection_generation {
            return Err(StreamHandoffError::SelectionChanged);
        }
        if self.selected_lineup.status() != SelectedLineupStatus::Ready {
            return Err(StreamHandoffError::SelectionNotReady);
        }
        self.validate_selection_retention()
            .map_err(|_| StreamHandoffError::Internal)?;

        let selected = self.selected_device.ok_or(StreamHandoffError::Internal)?;
        if key.device_id() != selected {
            return Err(StreamHandoffError::DeviceMismatch);
        }
        let retained = self
            .selected_snapshot
            .as_ref()
            .ok_or(StreamHandoffError::Internal)?;
        let resolved = &retained.resolved;
        let selected_source = resolved.selected_source();

        let registered = self
            .registry
            .get(selected)
            .ok_or(StreamHandoffError::OriginRejected)?;
        if !registered.locators().any(|locator| {
            locator.source().ip() == selected_source
                && DeviceEndpoint::from_locator(locator).is_ok()
        }) {
            return Err(StreamHandoffError::OriginRejected);
        }

        let projected = self
            .selected_lineup
            .channels()
            .binary_search_by(|channel| channel.key().cmp(&key))
            .ok()
            .and_then(|index| self.selected_lineup.channels().get(index))
            .ok_or(StreamHandoffError::ChannelUnavailable)?;
        let complete_lineup = resolved.snapshot().lineup();
        let channel = complete_lineup
            .channels()
            .binary_search_by(|channel| channel.key().cmp(&key))
            .ok()
            .and_then(|index| complete_lineup.channels().get(index))
            .ok_or(StreamHandoffError::ChannelUnavailable)?;
        if projected.is_drm() != channel.is_drm() {
            return Err(StreamHandoffError::Internal);
        }
        if channel.is_drm() {
            return Err(StreamHandoffError::Protected);
        }

        let stream_url = channel.stream_url();
        if !stream_url_matches(stream_url, selected_source, &key) {
            return Err(StreamHandoffError::OriginRejected);
        }

        Ok(StreamHandoff::new(
            key,
            self.selection_generation,
            stream_url.as_str(),
        ))
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
        )?
        .with_routed(self.routed);
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
                if snapshot.generation == self.selection_generation
                    && snapshot.generation == self.selected_lineup.generation()
                    && snapshot.resolved.device_id() == selected
                    && snapshot.resolved.snapshot().lineup().device_id() == selected
                    && self.selected_lineup.device_id() == Some(selected)
                    && selected_projection_matches(&self.selected_lineup, &snapshot.resolved) =>
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

fn selected_projection_matches(
    projected: &SelectedLineupState,
    resolved: &ResolvedDeviceSnapshot,
) -> bool {
    let complete = resolved.snapshot().lineup().channels();
    projected.channels().len() == complete.len()
        && projected
            .channels()
            .iter()
            .zip(complete)
            .all(|(summary, channel)| {
                summary.key() == channel.key()
                    && summary.name() == channel.name()
                    && summary.is_favorite() == channel.is_favorite()
                    && summary.is_drm() == channel.is_drm()
                    && summary.is_hd() == channel.is_hd()
            })
}

fn stream_url_matches(url: &reqwest::Url, selected_source: IpAddr, key: &ChannelKey) -> bool {
    let host = url
        .host_str()
        .map(|value| {
            value
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(value)
        })
        .and_then(|value| value.parse::<IpAddr>().ok());
    url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() == Some(5_004)
        && host == Some(selected_source)
        && url.path() == format!("/auto/v{}", key.guide_number())
}

fn preserve_device_summary(
    previous: &[DeviceSummary],
    candidate: &mut [DeviceSummary],
    device_id: DeviceId,
) -> Result<(), ControllerRuntimeError> {
    let previous = previous
        .binary_search_by_key(&device_id, DeviceSummary::device_id)
        .ok()
        .and_then(|index| previous.get(index))
        .ok_or(ControllerRuntimeError::SelectionSnapshotInvariant)?;
    let candidate = candidate
        .binary_search_by_key(&device_id, DeviceSummary::device_id)
        .ok()
        .and_then(|index| candidate.get_mut(index))
        .ok_or(ControllerRuntimeError::SelectionSnapshotInvariant)?;
    *candidate = previous.clone();
    Ok(())
}

fn rebuild_registry(
    local_batch: Option<&RetainedDiscoveryBatch>,
    routed_batch: Option<&RetainedDiscoveryBatch>,
    exact_sources: &BTreeMap<ExactDiscoveryTarget, RetainedExactSource>,
) -> Result<DeviceRegistry, ()> {
    let mut batches = Vec::with_capacity(2 + exact_sources.len());
    if let Some(batch) = local_batch {
        batches.push((batch.seen_at, DiscoveryScope::Local, batch));
    }
    if let Some(batch) = routed_batch {
        batches.push((
            batch.seen_at,
            DiscoveryScope::Routed(RoutedScanTrigger::Automatic),
            batch,
        ));
    }
    batches.extend(exact_sources.iter().filter_map(|(target, source)| {
        source
            .batch
            .as_ref()
            .map(|batch| (batch.seen_at, DiscoveryScope::Exact(*target), batch))
    }));
    batches.sort_by_key(|(seen_at, scope, _)| (*seen_at, *scope));

    let mut registry = DeviceRegistry::default();
    for (seen_at, _, batch) in batches {
        for observation in &batch.observations {
            registry
                .observe(observation.clone(), seen_at)
                .map_err(|_| ())?;
        }
    }
    Ok(registry)
}

/// A routed batch may hold many devices, but every observation must be a
/// direct routed reply on the discovery port with no interface annotation,
/// and no `(device, source)` pair may repeat.
fn validate_routed_batch(batch: &RetainedDiscoveryBatch) -> Result<(), ()> {
    if batch.observations.iter().any(|observation| {
        observation.method != DiscoveryMethod::RoutedTargeted
            || observation.interface.is_some()
            || observation.source.port() != DISCOVERY_UDP_PORT
    }) {
        return Err(());
    }
    if batch
        .observations
        .windows(2)
        .any(|pair| pair[0].device_id == pair[1].device_id && pair[0].source == pair[1].source)
    {
        return Err(());
    }
    Ok(())
}

/// Await an optional task, or never resolve when there is none.
async fn join_optional<T>(task: Option<&mut JoinHandle<T>>) -> Result<T, tokio::task::JoinError> {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
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
    Command(Option<ActorCommand>),
    Discovery(Result<DiscoveryCompletion, tokio::task::JoinError>),
    Selection(Result<SelectionCompletion, tokio::task::JoinError>),
    RoutedControl(Result<RoutedControlCompletion, tokio::task::JoinError>),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DiscoveryScope {
    Local,
    Exact(ExactDiscoveryTarget),
    Routed(RoutedScanTrigger),
}

impl DiscoveryScope {
    const fn kind(self) -> DiscoveryKind {
        match self {
            Self::Local => DiscoveryKind::Local,
            Self::Exact(_) => DiscoveryKind::Exact,
            Self::Routed(_) => DiscoveryKind::Routed,
        }
    }
}

/// One routed proposal, approval, or revocation running beside the lanes.
struct ActiveRoutedControl {
    cancellation: CancellationToken,
    task: JoinHandle<RoutedControlCompletion>,
}

enum RoutedControlCompletion {
    Proposed(Result<RoutedProposal, DiscoveryFailure>),
    Approved {
        token: RoutedApprovalToken,
        result: Result<(), DiscoveryFailure>,
    },
    Revoked(Result<(), DiscoveryFailure>),
}

#[derive(Clone, Eq, PartialEq)]
struct RetainedDiscoveryBatch {
    seen_at: RegistryInstant,
    observations: Vec<DiscoveryObservation>,
}

impl RetainedDiscoveryBatch {
    fn new(seen_at: RegistryInstant, mut observations: Vec<DiscoveryObservation>) -> Option<Self> {
        if observations.is_empty() {
            return None;
        }
        observations.sort_by(|left, right| {
            left.device_id
                .cmp(&right.device_id)
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.method.cmp(&right.method))
                .then_with(|| left.interface.cmp(&right.interface))
                .then_with(|| left.device_types.cmp(&right.device_types))
                .then_with(|| left.tuner_count.cmp(&right.tuner_count))
                .then_with(|| left.advertised_base_url.cmp(&right.advertised_base_url))
                .then_with(|| left.advertised_lineup_url.cmp(&right.advertised_lineup_url))
        });
        Some(Self {
            seen_at,
            observations,
        })
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
struct RetainedExactSource {
    bound_device: Option<DeviceId>,
    batch: Option<RetainedDiscoveryBatch>,
}

impl RetainedExactSource {
    fn replace_batch(
        &mut self,
        target: ExactDiscoveryTarget,
        batch: Option<RetainedDiscoveryBatch>,
    ) -> Result<(), ()> {
        if let Some(batch) = &batch {
            let device_id = batch.observations.first().ok_or(())?.device_id;
            let mut expected_source = target.socket_addr();
            expected_source.set_port(DISCOVERY_UDP_PORT);
            if batch.observations.iter().any(|observation| {
                observation.device_id != device_id
                    || observation.method != DiscoveryMethod::Targeted
                    || observation.interface.is_some()
                    || observation.source != expected_source
            }) || self
                .bound_device
                .is_some_and(|expected| expected != device_id)
            {
                return Err(());
            }
            self.bound_device = Some(device_id);
        }
        self.batch = batch;
        Ok(())
    }
}

struct DiscoveryUpdate {
    local_batch: Option<RetainedDiscoveryBatch>,
    routed_batch: Option<RetainedDiscoveryBatch>,
    exact_sources: BTreeMap<ExactDiscoveryTarget, RetainedExactSource>,
    registry: DeviceRegistry,
    devices: Vec<DeviceSummary>,
}

struct ActiveDiscovery {
    generation: OperationGeneration,
    scope: DiscoveryScope,
    cancellation: CancellationToken,
    task: JoinHandle<DiscoveryCompletion>,
}

struct DiscoveryCompletion {
    generation: OperationGeneration,
    scope: DiscoveryScope,
    result: Result<DiscoveryReport, DiscoveryFailure>,
    /// Remaining automatic cooldown reported by a refused routed run.
    cooldown: Option<Duration>,
}

struct RetainedSelectedSnapshot {
    generation: OperationGeneration,
    resolved: ResolvedDeviceSnapshot,
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
    use crate::discovery::{DiscoveryMethod, DiscoveryObservation, LocatorOrigin};

    /// Upper bound on every positive wait in these tests. It only shortens a
    /// failing run, so it is generous: on the Windows CI runners a scripted
    /// panic inside the controller thread symbolizes its backtrace before the
    /// task can be joined, which has taken longer than three seconds.
    const WAIT: Duration = Duration::from_secs(10);

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
        request: DiscoveryRequest,
        thread_name: Option<String>,
        runtime_flavor: RuntimeFlavor,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum DiscoveryRequest {
        Local,
        Exact {
            target: ExactDiscoveryTarget,
            expected_device: Option<DeviceId>,
        },
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

    impl DiscoveryService for ScriptedService {
        fn discover_local(&self, cancellation: CancellationToken) -> DiscoveryFuture {
            self.start(DiscoveryRequest::Local, cancellation)
        }

        fn discover_exact(
            &self,
            target: ExactDiscoveryTarget,
            expected_device: Option<DeviceId>,
            cancellation: CancellationToken,
        ) -> DiscoveryFuture {
            self.start(
                DiscoveryRequest::Exact {
                    target,
                    expected_device,
                },
                cancellation,
            )
        }
    }

    impl ScriptedService {
        fn start(
            &self,
            request: DiscoveryRequest,
            cancellation: CancellationToken,
        ) -> DiscoveryFuture {
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
                    request,
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

    fn retained_fixture(device_id: DeviceId) -> RetainedSelectedSnapshot {
        RetainedSelectedSnapshot {
            generation: OperationGeneration::INITIAL,
            resolved: ResolvedDeviceSnapshot::controller_test_fixture(device_id),
        }
    }

    fn second_id() -> DeviceId {
        DeviceId::new(0x105A_1243).unwrap()
    }

    fn exact_target(last_octet: u8) -> ExactDiscoveryTarget {
        ExactDiscoveryTarget::parse(&format!("198.51.100.{last_octet}")).unwrap()
    }

    fn unavailable_routed() -> UnavailableRoutedDiscovery {
        UnavailableRoutedDiscovery::new(RoutedUnavailableReason::NotConfigured)
    }

    fn test_actor() -> ControllerActor {
        let (service, _) = ScriptedService::new([]);
        let (selection, _) = ScriptedSelectionService::new([]);
        let (_commands, receiver) = mpsc::channel(1);
        let (snapshots, _snapshot_receiver) =
            watch::channel(Arc::new(ApplicationSnapshot::initial()));
        ControllerActor::new(
            Arc::new(service),
            Arc::new(selection),
            Arc::new(UnavailableRoutedDiscovery::new(
                RoutedUnavailableReason::NotConfigured,
            )),
            receiver,
            CancellationToken::new(),
            snapshots,
        )
    }

    fn ready_stream_actor(
        protected: bool,
        selected_source: &str,
        registry_source: &str,
    ) -> ControllerActor {
        let mut actor = test_actor();
        let mut observation = report(first_id(), &format!("{registry_source}:65001"), 4)
            .observations
            .remove(0);
        observation.interface = None;
        actor
            .registry
            .observe(observation, RegistryInstant::from_duration(Duration::ZERO))
            .unwrap();
        actor.devices = project_devices(&actor.registry).unwrap();
        actor.selection_generation = OperationGeneration::new(1);
        actor.selected_device = Some(first_id());
        actor
            .accept_selected_snapshot(ResolvedDeviceSnapshot::controller_stream_test_fixture(
                first_id(),
                protected,
                selected_source.parse().unwrap(),
            ))
            .unwrap();
        actor
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

    fn exact_report(
        target: ExactDiscoveryTarget,
        device_id: DeviceId,
        tuner_count: u8,
    ) -> DiscoveryReport {
        let mut report = report(device_id, "192.0.2.1:65001", tuner_count);
        let mut source = target.socket_addr();
        source.set_port(DISCOVERY_UDP_PORT);
        report.observations[0].source = source;
        report.observations[0].interface = None;
        report
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

    #[test]
    fn exact_probe_budget_is_fixed_and_neighbor_friendly() {
        let config = exact_probe_config();

        assert_eq!(config.attempts(), EXACT_DISCOVERY_ATTEMPTS);
        assert_eq!(config.response_window(), EXACT_DISCOVERY_RESPONSE_WINDOW);
        assert_eq!(
            config.max_received_datagrams(),
            EXACT_DISCOVERY_MAX_RECEIVED_DATAGRAMS
        );
        assert_eq!(
            config.max_unique_devices(),
            EXACT_DISCOVERY_MAX_UNIQUE_DEVICES
        );
    }

    #[test]
    fn retained_report_bounds_match_registry_capacity_and_exact_cardinality() {
        let actor = test_actor();
        let observation = report(first_id(), "192.0.2.10:65001", 4)
            .observations
            .pop()
            .unwrap();
        let at_local_limit = DiscoveryReport {
            observations: vec![observation.clone(); MAX_RETAINED_LOCAL_OBSERVATIONS],
            ..DiscoveryReport::default()
        };
        assert!(
            actor
                .build_discovery_update(DiscoveryScope::Local, at_local_limit)
                .is_ok()
        );

        let over_local_limit = DiscoveryReport {
            observations: vec![observation.clone(); MAX_RETAINED_LOCAL_OBSERVATIONS + 1],
            ..DiscoveryReport::default()
        };
        assert!(
            actor
                .build_discovery_update(DiscoveryScope::Local, over_local_limit)
                .is_err()
        );

        let target = exact_target(1);
        let exact_observation = exact_report(target, first_id(), 4)
            .observations
            .pop()
            .unwrap();
        let exact_one = DiscoveryReport {
            observations: vec![exact_observation.clone()],
            ..DiscoveryReport::default()
        };
        assert!(
            actor
                .build_discovery_update(DiscoveryScope::Exact(target), exact_one)
                .is_ok()
        );

        let exact_two = DiscoveryReport {
            observations: vec![exact_observation; MAX_RETAINED_EXACT_OBSERVATIONS + 1],
            ..DiscoveryReport::default()
        };
        assert!(
            actor
                .build_discovery_update(DiscoveryScope::Exact(target), exact_two)
                .is_err()
        );
    }

    #[test]
    fn exact_retention_revalidates_source_port_and_provenance() {
        let mut actor = test_actor();
        let target = exact_target(2);
        let valid = actor
            .build_discovery_update(
                DiscoveryScope::Exact(target),
                exact_report(target, first_id(), 4),
            )
            .unwrap();
        actor.commit_discovery_update(valid);
        let ipv6_target = ExactDiscoveryTarget::parse("2001:db8::2").unwrap();
        let valid_ipv6 = actor
            .build_discovery_update(
                DiscoveryScope::Exact(ipv6_target),
                exact_report(ipv6_target, first_id(), 4),
            )
            .unwrap();
        actor.commit_discovery_update(valid_ipv6);
        let prior_registry = actor.registry.clone();
        let prior_devices = actor.devices.clone();
        let prior_sources = actor.exact_sources.clone();

        let mut wrong_address = exact_report(target, first_id(), 4);
        wrong_address.observations[0].source = "198.51.100.3:65001".parse().unwrap();
        let mut wrong_port = exact_report(target, first_id(), 4);
        wrong_port.observations[0].source.set_port(65_000);
        let mut wrong_method = exact_report(target, first_id(), 4);
        wrong_method.observations[0].method = DiscoveryMethod::RoutedTargeted;
        let mut wrong_interface = exact_report(target, first_id(), 4);
        wrong_interface.observations[0].interface = Some("synthetic0".to_owned());

        for report in [wrong_address, wrong_port, wrong_method, wrong_interface] {
            assert!(
                actor
                    .build_discovery_update(DiscoveryScope::Exact(target), report)
                    .is_err()
            );
            assert!(actor.registry == prior_registry);
            assert!(actor.devices == prior_devices);
            assert!(actor.exact_sources == prior_sources);
            assert_eq!(
                actor.expected_device_for_exact_target(target),
                Some(first_id())
            );
        }

        let mut scoped_ipv6 = exact_report(ipv6_target, first_id(), 4);
        let std::net::SocketAddr::V6(source) = &mut scoped_ipv6.observations[0].source else {
            panic!("test target must be IPv6");
        };
        source.set_scope_id(7);
        assert!(
            actor
                .build_discovery_update(DiscoveryScope::Exact(ipv6_target), scoped_ipv6)
                .is_err()
        );
        assert!(actor.registry == prior_registry);
        assert!(actor.devices == prior_devices);
        assert!(actor.exact_sources == prior_sources);
        assert_eq!(
            actor.expected_device_for_exact_target(ipv6_target),
            Some(first_id())
        );
    }

    #[test]
    fn retained_batches_replay_by_time_and_local_precedes_exact_on_a_tie() {
        let tied_target = exact_target(3);
        let older_target = exact_target(4);
        let newer_target = exact_target(5);
        let tied_at = RegistryInstant::from_duration(Duration::from_secs(20));
        let older_at = RegistryInstant::from_duration(Duration::from_secs(10));
        let newer_at = RegistryInstant::from_duration(Duration::from_secs(30));

        let mut local_observation = exact_report(tied_target, first_id(), 2)
            .observations
            .pop()
            .unwrap();
        local_observation.method = DiscoveryMethod::Ipv4Broadcast;
        local_observation.interface = Some("synthetic0".to_owned());
        let local_batch = RetainedDiscoveryBatch::new(tied_at, vec![local_observation]).unwrap();
        let tied_exact = RetainedDiscoveryBatch::new(
            tied_at,
            exact_report(tied_target, first_id(), 4).observations,
        )
        .unwrap();
        let older_exact = RetainedDiscoveryBatch::new(
            older_at,
            exact_report(older_target, first_id(), 1).observations,
        )
        .unwrap();
        let newer_exact = RetainedDiscoveryBatch::new(
            newer_at,
            exact_report(newer_target, first_id(), 8).observations,
        )
        .unwrap();
        let exact_sources = BTreeMap::from([
            (
                tied_target,
                RetainedExactSource {
                    bound_device: Some(first_id()),
                    batch: Some(tied_exact),
                },
            ),
            (
                older_target,
                RetainedExactSource {
                    bound_device: Some(first_id()),
                    batch: Some(older_exact),
                },
            ),
            (
                newer_target,
                RetainedExactSource {
                    bound_device: Some(first_id()),
                    batch: Some(newer_exact),
                },
            ),
        ]);

        let registry = rebuild_registry(Some(&local_batch), None, &exact_sources).unwrap();
        assert_eq!(registry.clock(), Some(newer_at));
        let device = registry.get(first_id()).unwrap();
        let tied_source = exact_report(tied_target, first_id(), 4).observations[0].source;
        let tied_claim = device
            .locators()
            .find(|claim| claim.source() == tied_source)
            .unwrap();
        assert_eq!(tied_claim.tuner_count(), Some(4));
        let local_origin = LocatorOrigin {
            method: DiscoveryMethod::Ipv4Broadcast,
            interface: Some("synthetic0".to_owned()),
        };
        let exact_origin = LocatorOrigin {
            method: DiscoveryMethod::Targeted,
            interface: None,
        };
        assert_eq!(tied_claim.origin_first_seen(&local_origin), Some(tied_at));
        assert_eq!(tied_claim.origin_last_seen(&local_origin), Some(tied_at));
        assert_eq!(tied_claim.origin_first_seen(&exact_origin), Some(tied_at));
        assert_eq!(tied_claim.origin_last_seen(&exact_origin), Some(tied_at));
        assert_eq!(
            device.preferred_locator().map(|claim| claim.source()),
            Some(exact_report(newer_target, first_id(), 8).observations[0].source)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_and_unsupported_selection_never_call_the_resolver() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            unsupported_report(first_id(), "192.0.2.10:65001"),
        ))]);
        let (selection, _selection_starts) = ScriptedSelectionService::new([]);
        let observed_selection = selection.clone();
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
            report(first_id(), "127.0.0.1:65001", 4),
        ))]);
        let (release, release_rx) = oneshot::channel();
        let (cancelled, _cancelled_rx) = std_mpsc::channel();
        let (selection, selection_starts) = ScriptedSelectionService::new([SelectionStep::Gated {
            release: release_rx,
            cancelled,
            cancellation_result: Err(DeviceSnapshotResolutionError::Cancelled),
        }]);
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        assert!(!rendered.contains("http://"));
        assert!(!rendered.contains("auto/v5.1"));

        let revision = ready.revision();
        let response = handle
            .try_request_stream(StreamSelection::new(
                channel.key().clone(),
                ready.selection_generation(),
            ))
            .unwrap()
            .receive()
            .await;
        assert!(matches!(response, Err(StreamHandoffError::Protected)));
        assert_eq!(handle.snapshot().revision(), revision);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actor_returns_one_successful_private_stream_handoff() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "127.0.0.1:65001", 4),
        ))]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_stream_test_fixture(
                    first_id(),
                    false,
                    "127.0.0.1".parse().unwrap(),
                ),
            ))]);
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;
        let key = ready.selected_lineup().channels()[0].key().clone();
        let revision = ready.revision();

        let handoff = handle
            .try_request_stream(StreamSelection::new(
                key.clone(),
                ready.selection_generation(),
            ))
            .unwrap()
            .receive()
            .await
            .unwrap();

        assert_eq!(handoff.channel_key(), &key);
        assert_eq!(handoff.selection_generation(), ready.selection_generation());
        let rendered = format!("{handoff:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains("auto/v5.1"));
        assert_eq!(handle.snapshot().revision(), revision);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stream_requests_share_fifo_with_selection_and_clear_commands() {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "127.0.0.1:65001", 4),
        ))]);
        let selected_snapshot = || -> Result<_, DeviceSnapshotResolutionError> {
            Ok(ResolvedDeviceSnapshot::controller_stream_test_fixture(
                first_id(),
                false,
                "127.0.0.1".parse().unwrap(),
            ))
        };
        let (selection, selection_starts) = ScriptedSelectionService::new([
            SelectionStep::Immediate(selected_snapshot()),
            SelectionStep::Immediate(selected_snapshot()),
        ]);
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let first_ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;
        let key = first_ready.selected_lineup().channels()[0].key().clone();
        let first_generation = first_ready.selection_generation();

        let request_before_clear = handle
            .try_request_stream(StreamSelection::new(key.clone(), first_generation))
            .unwrap();
        handle.try_send(ControllerCommand::ClearSelection).unwrap();
        let handoff = request_before_clear.receive().await.unwrap();
        assert_eq!(handoff.selection_generation(), first_generation);
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(2)
                && snapshot.selected_lineup().status() == SelectedLineupStatus::Unselected
        })
        .await;

        handle
            .try_send(ControllerCommand::SelectDevice(first_id()))
            .unwrap();
        let request_after_select = handle
            .try_request_stream(StreamSelection::new(
                key.clone(),
                cleared.selection_generation(),
            ))
            .unwrap();
        assert!(matches!(
            request_after_select.receive().await,
            Err(StreamHandoffError::SelectionChanged)
        ));
        recv_selection_start(&selection_starts);
        let second_ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(3)
                && snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;

        handle.try_send(ControllerCommand::ClearSelection).unwrap();
        let request_after_clear = handle
            .try_request_stream(StreamSelection::new(
                key,
                second_ready.selection_generation(),
            ))
            .unwrap();
        assert!(matches!(
            request_after_clear.receive().await,
            Err(StreamHandoffError::SelectionChanged)
        ));
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selection_generation() == OperationGeneration::new(4)
                && snapshot.selected_lineup().status() == SelectedLineupStatus::Unselected
        })
        .await;
        controller.shutdown().unwrap();
    }

    #[test]
    fn stream_handoff_is_current_generation_origin_checked_and_url_redacted() {
        let actor = ready_stream_actor(false, "127.0.0.1", "127.0.0.1");
        let key = actor.selected_lineup.channels()[0].key().clone();
        let revision = actor.revision;

        let handoff = actor
            .resolve_stream_handoff(StreamSelection::new(
                key.clone(),
                actor.selection_generation,
            ))
            .unwrap();
        assert_eq!(handoff.channel_key(), &key);
        assert_eq!(handoff.selection_generation(), actor.selection_generation);
        let rendered = format!("{handoff:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains("auto/v5.1"));
        assert_eq!(actor.revision, revision, "handoff must not publish state");

        assert!(matches!(
            actor.resolve_stream_handoff(StreamSelection::new(
                key.clone(),
                OperationGeneration::INITIAL,
            )),
            Err(StreamHandoffError::SelectionChanged)
        ));
        let cross_device = ChannelKey::new(second_id(), key.guide_number().clone());
        assert!(matches!(
            actor.resolve_stream_handoff(StreamSelection::new(
                cross_device,
                actor.selection_generation,
            )),
            Err(StreamHandoffError::DeviceMismatch)
        ));
        let absent = ChannelKey::new(first_id(), crate::domain::GuideNumber::new("99.9").unwrap());
        assert!(matches!(
            actor.resolve_stream_handoff(StreamSelection::new(absent, actor.selection_generation,)),
            Err(StreamHandoffError::ChannelUnavailable)
        ));

        let protected = ready_stream_actor(true, "127.0.0.1", "127.0.0.1");
        let protected_key = protected.selected_lineup.channels()[0].key().clone();
        assert!(matches!(
            protected.resolve_stream_handoff(StreamSelection::new(
                protected_key,
                protected.selection_generation,
            )),
            Err(StreamHandoffError::Protected)
        ));

        let stale_locator = ready_stream_actor(false, "127.0.0.1", "192.0.2.10");
        let stale_key = stale_locator.selected_lineup.channels()[0].key().clone();
        assert!(matches!(
            stale_locator.resolve_stream_handoff(StreamSelection::new(
                stale_key,
                stale_locator.selection_generation,
            )),
            Err(StreamHandoffError::OriginRejected)
        ));

        let wrong_origin = ready_stream_actor(false, "192.0.2.10", "192.0.2.10");
        let wrong_origin_key = wrong_origin.selected_lineup.channels()[0].key().clone();
        assert!(matches!(
            wrong_origin.resolve_stream_handoff(StreamSelection::new(
                wrong_origin_key,
                wrong_origin.selection_generation,
            )),
            Err(StreamHandoffError::OriginRejected)
        ));
    }

    #[test]
    fn stream_url_revalidation_rejects_every_origin_and_path_escape() {
        let key = ChannelKey::new(first_id(), crate::domain::GuideNumber::new("5.1").unwrap());
        let source = "192.0.2.10".parse().unwrap();
        assert!(stream_url_matches(
            &reqwest::Url::parse("http://192.0.2.10:5004/auto/v5.1").unwrap(),
            source,
            &key,
        ));

        for rejected in [
            "https://192.0.2.10:5004/auto/v5.1",
            "http://fixture.invalid:5004/auto/v5.1",
            "http://192.0.2.11:5004/auto/v5.1",
            "http://192.0.2.10/auto/v5.1",
            "http://192.0.2.10:5005/auto/v5.1",
            "http://user@192.0.2.10:5004/auto/v5.1",
            "http://192.0.2.10:5004/auto/v5.1?token=private",
            "http://192.0.2.10:5004/auto/v5.1#private",
            "http://192.0.2.10:5004/auto/v5.2",
            "http://192.0.2.10:5004/not-auto/v5.1",
            "http://192.0.2.10:5004/auto/v%35.1",
        ] {
            assert!(!stream_url_matches(
                &reqwest::Url::parse(rejected).unwrap(),
                source,
                &key,
            ));
        }
    }

    #[test]
    fn stream_url_revalidation_accepts_an_ipv6_origin() {
        let key = ChannelKey::new(first_id(), crate::domain::GuideNumber::new("5.1").unwrap());
        let source = "2001:db8::10".parse().unwrap();
        let url = reqwest::Url::parse("http://[2001:db8::10]:5004/auto/v5.1").unwrap();

        assert!(stream_url_matches(&url, source, &key));
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
            Arc::new(unavailable_routed()),
            receiver,
            CancellationToken::new(),
            snapshots,
        );

        actor.selected_snapshot = Some(retained_fixture(first_id()));
        assert_eq!(
            actor.validate_selection_retention(),
            Err(ControllerRuntimeError::SelectionSnapshotInvariant)
        );
        actor.selected_device = Some(first_id());
        actor.selected_lineup = SelectedLineupState::ready(
            first_id(),
            OperationGeneration::INITIAL,
            [ChannelSummary::new(
                ChannelKey::new(first_id(), crate::domain::GuideNumber::new("5.1").unwrap()),
                "private channel".to_owned(),
                true,
                true,
                true,
            )
            .unwrap()],
        )
        .unwrap();
        assert_eq!(actor.validate_selection_retention(), Ok(()));
        actor.selected_snapshot = Some(retained_fixture(second_id()));
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        assert_send::<StreamHandoff>();

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
            controller.try_send(ControllerCommand::CancelDiscovery),
            Err(ControllerCommandError::Full)
        );
        let selection = StreamSelection::new(
            ChannelKey::new(first_id(), crate::domain::GuideNumber::new("5.1").unwrap()),
            OperationGeneration::INITIAL,
        );
        assert!(matches!(
            controller.try_request_stream(selection.clone()),
            Err(ControllerCommandError::Full)
        ));
        shutdown.cancel();
        assert_eq!(
            controller.try_send(ControllerCommand::RefreshLocalDiscovery),
            Err(ControllerCommandError::ShuttingDown)
        );
        assert!(matches!(
            controller.try_request_stream(selection),
            Err(ControllerCommandError::ShuttingDown)
        ));
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
    async fn exact_discovery_binds_identity_and_rejects_a_mismatched_completion() {
        let target = exact_target(7);
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(exact_report(target, first_id(), 4))),
            ServiceStep::Immediate(Ok(exact_report(target, second_id(), 2))),
        ]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: None,
            }
        );
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(1)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(ready.devices().len(), 1);
        assert_eq!(ready.devices()[0].device_id(), first_id());

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: Some(first_id()),
            }
        );
        let failed = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status()
                    == DiscoveryStatus::Failed(DiscoveryFailure::Internal)
        })
        .await;

        assert_eq!(failed.devices().len(), 1);
        assert_eq!(failed.devices()[0].device_id(), first_id());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_expected_identity_uses_registry_owner_and_survives_no_response() {
        let target = exact_target(8);
        let exact_source = exact_report(target, first_id(), 4).observations[0].source;
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), &exact_source.to_string(), 4))),
            ServiceStep::Immediate(Ok(exact_report(target, first_id(), 4))),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
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
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: Some(first_id()),
            }
        );
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(3)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        recv_start(&starts);
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(4)
                && snapshot.discovery().status() == DiscoveryStatus::NoResponse
        })
        .await;
        assert!(cleared.devices().is_empty());

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: Some(first_id()),
            }
        );
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(5)
                && snapshot.discovery().status() == DiscoveryStatus::NoResponse
        })
        .await;
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authoritative_source_batches_union_replace_and_clear_independently() {
        let first_target = exact_target(10);
        let second_target = exact_target(11);
        let (service, starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(exact_report(first_target, second_id(), 2))),
            ServiceStep::Immediate(Ok(exact_report(first_target, second_id(), 4))),
            ServiceStep::Immediate(Ok(exact_report(second_target, first_id(), 4))),
            ServiceStep::Immediate(Err(DiscoveryFailure::Network)),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
        ]);
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(recv_start(&starts).request, DiscoveryRequest::Local);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(1)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        handle
            .try_send(ControllerCommand::DiscoverExact(first_target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target: first_target,
                expected_device: None,
            }
        );
        let union = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(
            union
                .devices()
                .iter()
                .map(DeviceSummary::device_id)
                .collect::<Vec<_>>(),
            vec![first_id(), second_id()]
        );

        handle
            .try_send(ControllerCommand::DiscoverExact(first_target))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target: first_target,
                expected_device: Some(second_id()),
            }
        );
        let replaced = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(3)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(
            replaced
                .devices()
                .iter()
                .find(|device| device.device_id() == second_id())
                .and_then(DeviceSummary::tuner_count),
            Some(4)
        );

        handle
            .try_send(ControllerCommand::DiscoverExact(second_target))
            .unwrap();
        recv_start(&starts);
        let two_origins = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(4)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(two_origins.devices()[0].device_id(), first_id());
        assert_eq!(two_origins.devices()[0].locator_count(), 2);

        handle
            .try_send(ControllerCommand::DiscoverExact(first_target))
            .unwrap();
        recv_start(&starts);
        let failed = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(5)
                && snapshot.discovery().status()
                    == DiscoveryStatus::Failed(DiscoveryFailure::Network)
        })
        .await;
        assert_eq!(failed.devices(), two_origins.devices());

        handle
            .try_send(ControllerCommand::DiscoverExact(first_target))
            .unwrap();
        recv_start(&starts);
        let no_response = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(6)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status() == DiscoveryStatus::NoResponse
        })
        .await;
        assert_eq!(no_response.devices().len(), 1);
        assert_eq!(no_response.devices()[0].device_id(), first_id());
        assert_eq!(no_response.devices()[0].locator_count(), 2);

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        recv_start(&starts);
        let exact_only = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(7)
                && snapshot.discovery().kind() == DiscoveryKind::Local
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(exact_only.devices().len(), 1);
        assert_eq!(exact_only.devices()[0].device_id(), first_id());
        assert_eq!(exact_only.devices()[0].locator_count(), 1);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_target_attempt_cap_precedes_io_and_allows_bounded_retries() {
        let steps = (0..MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION)
            .map(|_| ServiceStep::Immediate(Ok(DiscoveryReport::default())))
            .chain([
                ServiceStep::Immediate(Err(DiscoveryFailure::Network)),
                ServiceStep::Immediate(Ok(DiscoveryReport::default())),
            ]);
        let (service, starts) = ScriptedService::new(steps);
        let observed_service = service.clone();
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        for index in 1..=MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION {
            let target = exact_target(u8::try_from(index).unwrap());
            handle
                .try_send(ControllerCommand::DiscoverExact(target))
                .unwrap();
            assert_eq!(
                recv_start(&starts).request,
                DiscoveryRequest::Exact {
                    target,
                    expected_device: None,
                }
            );
            wait_for_snapshot(&mut snapshots, |snapshot| {
                snapshot.discovery_generation()
                    == OperationGeneration::new(u64::try_from(index).unwrap())
                    && snapshot.discovery().status() == DiscoveryStatus::NoResponse
            })
            .await;
        }

        let rejected = exact_target(33);
        handle
            .try_send(ControllerCommand::DiscoverExact(rejected))
            .unwrap();
        let capped = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(33)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status()
                    == DiscoveryStatus::Failed(DiscoveryFailure::ExactTargetLimitReached)
        })
        .await;
        assert!(capped.devices().is_empty());
        assert_eq!(observed_service.calls(), 32);
        assert!(starts.try_recv().is_err());

        let repeated = exact_target(1);
        handle
            .try_send(ControllerCommand::DiscoverExact(repeated))
            .unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target: repeated,
                expected_device: None,
            }
        );
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(34)
                && snapshot.discovery().status()
                    == DiscoveryStatus::Failed(DiscoveryFailure::Network)
        })
        .await;

        handle
            .try_send(ControllerCommand::DiscoverExact(repeated))
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(35)
                && snapshot.discovery().status() == DiscoveryStatus::NoResponse
        })
        .await;
        assert_eq!(observed_service.calls(), 34);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn over_cap_exact_command_cancels_and_joins_active_lane_without_replacement_io() {
        let (cancellation_observed, cancellation_observed_rx) = std_mpsc::channel();
        let (finish_cancellation, finish_cancellation_rx) = oneshot::channel();
        let steps = (0..MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION)
            .map(|_| ServiceStep::Immediate(Ok(DiscoveryReport::default())))
            .chain([ServiceStep::CancellationBarrier {
                cancellation_observed,
                finish_cancellation: finish_cancellation_rx,
                cancellation_result: Err(DiscoveryFailure::Internal),
            }]);
        let (service, starts) = ScriptedService::new(steps);
        let observed_service = service.clone();
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        for index in 1..=MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION {
            handle
                .try_send(ControllerCommand::DiscoverExact(exact_target(
                    u8::try_from(index).unwrap(),
                )))
                .unwrap();
            recv_start(&starts);
            wait_for_snapshot(&mut snapshots, |snapshot| {
                snapshot.discovery_generation()
                    == OperationGeneration::new(u64::try_from(index).unwrap())
                    && snapshot.discovery().status() == DiscoveryStatus::NoResponse
            })
            .await;
        }

        handle
            .try_send(ControllerCommand::DiscoverExact(exact_target(1)))
            .unwrap();
        recv_start(&starts);
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(33)
                && snapshot.discovery().status() == DiscoveryStatus::Refreshing
        })
        .await;
        handle
            .try_send(ControllerCommand::DiscoverExact(exact_target(33)))
            .unwrap();
        cancellation_observed_rx
            .recv_timeout(WAIT)
            .expect("over-cap supersession must cancel the active discovery");
        assert_eq!(
            handle.snapshot().discovery().status(),
            DiscoveryStatus::Refreshing
        );
        assert_eq!(observed_service.calls(), 33);
        assert!(starts.try_recv().is_err());

        finish_cancellation.send(()).unwrap();
        let capped = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(34)
                && snapshot.discovery().status()
                    == DiscoveryStatus::Failed(DiscoveryFailure::ExactTargetLimitReached)
        })
        .await;
        assert!(capped.devices().is_empty());
        assert_eq!(observed_service.calls(), 33);
        assert!(starts.try_recv().is_err());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_success_never_auto_selects_or_starts_http_resolution() {
        let target = exact_target(20);
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            exact_report(target, first_id(), 4),
        ))]);
        let (selection, _selection_starts) = ScriptedSelectionService::new([]);
        let observed_selection = selection.clone();
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        recv_start(&discovery_starts);
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        assert_eq!(ready.selected_device(), None);
        assert_eq!(
            ready.selected_lineup().status(),
            SelectedLineupStatus::Unselected
        );
        assert_eq!(observed_selection.calls(), 0);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unrelated_exact_success_preserves_ready_selection_without_http() {
        let target = exact_target(21);
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(exact_report(target, second_id(), 4))),
        ]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
            ))]);
        let observed_selection = selection.clone();
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let selected = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;
        assert_eq!(selected.selection_generation(), OperationGeneration::new(1));

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        recv_start(&discovery_starts);
        let exact_ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        assert_eq!(exact_ready.selected_device(), Some(first_id()));
        assert_eq!(
            exact_ready.selected_lineup().status(),
            SelectedLineupStatus::Ready
        );
        assert_eq!(
            exact_ready.selection_generation(),
            OperationGeneration::new(1)
        );
        assert_eq!(
            exact_ready.devices()[0].friendly_name(),
            Some("private fixture name")
        );
        assert_eq!(observed_selection.calls(), 1);
        assert!(selection_starts.try_recv().is_err());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_mutation_of_selected_evidence_clears_without_http() {
        let target = exact_target(22);
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(report(first_id(), "192.0.2.10:65001", 4))),
            ServiceStep::Immediate(Ok(exact_report(target, first_id(), 4))),
        ]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
            ))]);
        let observed_selection = selection.clone();
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        recv_start(&discovery_starts);
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::Ready
                && snapshot.selection_generation() == OperationGeneration::new(2)
        })
        .await;

        assert_eq!(cleared.selected_device(), None);
        assert_eq!(
            cleared.selected_lineup().status(),
            SelectedLineupStatus::Unselected
        );
        assert_eq!(observed_selection.calls(), 1);
        assert!(selection_starts.try_recv().is_err());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_no_response_removes_selected_evidence_and_clears_without_http() {
        let target = exact_target(23);
        let (discovery, discovery_starts) = ScriptedService::new([
            ServiceStep::Immediate(Ok(exact_report(target, first_id(), 4))),
            ServiceStep::Immediate(Ok(DiscoveryReport::default())),
        ]);
        let (selection, selection_starts) =
            ScriptedSelectionService::new([SelectionStep::Immediate(Ok(
                ResolvedDeviceSnapshot::controller_test_fixture(first_id()),
            ))]);
        let observed_selection = selection.clone();
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::DiscoverExact(target))
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
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        assert_eq!(
            recv_start(&discovery_starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: Some(first_id()),
            }
        );
        let cleared = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().status() == DiscoveryStatus::NoResponse
                && snapshot.selection_generation() == OperationGeneration::new(2)
        })
        .await;

        assert!(cleared.devices().is_empty());
        assert_eq!(cleared.selected_device(), None);
        assert_eq!(observed_selection.calls(), 1);
        assert!(selection_starts.try_recv().is_err());
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
    async fn local_and_exact_operations_share_one_joined_superseding_lane() {
        let target = exact_target(24);
        let (cancellation_observed, cancellation_observed_rx) = std_mpsc::channel();
        let (finish_cancellation, finish_cancellation_rx) = oneshot::channel();
        let (service, starts) = ScriptedService::new([
            ServiceStep::CancellationBarrier {
                cancellation_observed,
                finish_cancellation: finish_cancellation_rx,
                cancellation_result: Err(DiscoveryFailure::Internal),
            },
            ServiceStep::Immediate(Ok(exact_report(target, first_id(), 4))),
        ]);
        let observed_service = service.clone();
        let controller = ControllerRuntime::start(service).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        assert_eq!(recv_start(&starts).request, DiscoveryRequest::Local);
        handle
            .try_send(ControllerCommand::DiscoverExact(target))
            .unwrap();
        cancellation_observed_rx
            .recv_timeout(WAIT)
            .expect("exact discovery must cancel the active local operation");
        assert!(starts.try_recv().is_err());
        finish_cancellation.send(()).unwrap();
        assert_eq!(
            recv_start(&starts).request,
            DiscoveryRequest::Exact {
                target,
                expected_device: None,
            }
        );
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery_generation() == OperationGeneration::new(2)
                && snapshot.discovery().kind() == DiscoveryKind::Exact
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;

        assert_eq!(observed_service.maximum_active(), 1);
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
        handle.try_send(ControllerCommand::CancelDiscovery).unwrap();
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
            Arc::new(unavailable_routed()),
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
                    scope: DiscoveryScope::Local,
                    result: Ok(report(first_id(), "192.0.2.10:65001", 4)),
                    cooldown: None,
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
        let controller =
            ControllerRuntime::start_with_test_services(discovery, selection, unavailable_routed())
                .unwrap();
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
            handle.try_send(ControllerCommand::CancelDiscovery),
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

    // ---- routed lane -------------------------------------------------------

    use super::super::routed::RoutedFuture;
    use crate::discovery::approval::{RouteFingerprintKey, RoutedScanProposal};
    use crate::discovery::{
        InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteScope,
        RouteSnapshot, RoutedProposalSummary, RoutedScanConfig, select_route_candidates,
    };

    enum RoutedStep<T> {
        Immediate(Result<T, DiscoveryFailure>),
        /// Hold the call in flight until the test releases its result, so a
        /// transient snapshot stays observable through the watch channel.
        Gated {
            release: oneshot::Receiver<Result<T, DiscoveryFailure>>,
        },
        UntilCancelled {
            started: std_mpsc::Sender<()>,
            cancelled: std_mpsc::Sender<()>,
        },
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RoutedCall {
        Propose,
        Approve(RoutedApprovalToken),
        Run(RoutedScanTrigger),
        RevokeAll,
        Origins(RoutedApprovalToken),
    }

    struct ScriptedRoutedState {
        proposals: Mutex<VecDeque<RoutedStep<RoutedProposal>>>,
        approvals: Mutex<VecDeque<RoutedStep<()>>>,
        runs: Mutex<VecDeque<RoutedStep<RoutedRunOutcome>>>,
        revokes: Mutex<VecDeque<RoutedStep<()>>>,
        origins: Vec<RoutedProposalOriginSummary>,
        calls: Mutex<Vec<RoutedCall>>,
    }

    #[derive(Clone)]
    struct ScriptedRoutedService {
        shared: Arc<ScriptedRoutedState>,
    }

    impl ScriptedRoutedService {
        fn new(origins: Vec<RoutedProposalOriginSummary>) -> Self {
            Self {
                shared: Arc::new(ScriptedRoutedState {
                    proposals: Mutex::new(VecDeque::new()),
                    approvals: Mutex::new(VecDeque::new()),
                    runs: Mutex::new(VecDeque::new()),
                    revokes: Mutex::new(VecDeque::new()),
                    origins,
                    calls: Mutex::new(Vec::new()),
                }),
            }
        }

        fn script_proposal(&self, step: RoutedStep<RoutedProposal>) {
            self.shared.proposals.lock().unwrap().push_back(step);
        }

        fn script_approval(&self, step: RoutedStep<()>) {
            self.shared.approvals.lock().unwrap().push_back(step);
        }

        fn script_run(&self, step: RoutedStep<RoutedRunOutcome>) {
            self.shared.runs.lock().unwrap().push_back(step);
        }

        fn script_revoke(&self, step: RoutedStep<()>) {
            self.shared.revokes.lock().unwrap().push_back(step);
        }

        fn calls(&self) -> Vec<RoutedCall> {
            self.shared.calls.lock().unwrap().clone()
        }

        fn play<T: Send + 'static>(
            &self,
            call: RoutedCall,
            step: Option<RoutedStep<T>>,
            cancellation: CancellationToken,
        ) -> RoutedFuture<T> {
            self.shared.calls.lock().unwrap().push(call);
            let step = step.expect("test should script one routed step per call");
            Box::pin(async move {
                match step {
                    RoutedStep::Immediate(result) => result,
                    RoutedStep::Gated { release } => {
                        tokio::select! {
                            biased;
                            () = cancellation.cancelled() => Err(DiscoveryFailure::Internal),
                            result = release => {
                                result.expect("test release sender should remain open")
                            }
                        }
                    }
                    RoutedStep::UntilCancelled { started, cancelled } => {
                        let _ = started.send(());
                        cancellation.cancelled().await;
                        let _ = cancelled.send(());
                        Err(DiscoveryFailure::Internal)
                    }
                }
            })
        }
    }

    impl RoutedDiscoveryService for ScriptedRoutedService {
        fn availability(&self) -> Result<(), RoutedUnavailableReason> {
            Ok(())
        }

        fn propose(&self, cancellation: CancellationToken) -> RoutedFuture<RoutedProposal> {
            let step = self.shared.proposals.lock().unwrap().pop_front();
            self.play(RoutedCall::Propose, step, cancellation)
        }

        fn approve(
            &self,
            token: RoutedApprovalToken,
            cancellation: CancellationToken,
        ) -> RoutedFuture<()> {
            let step = self.shared.approvals.lock().unwrap().pop_front();
            self.play(RoutedCall::Approve(token), step, cancellation)
        }

        fn run(
            &self,
            trigger: RoutedScanTrigger,
            cancellation: CancellationToken,
        ) -> RoutedFuture<RoutedRunOutcome> {
            let step = self.shared.runs.lock().unwrap().pop_front();
            self.play(RoutedCall::Run(trigger), step, cancellation)
        }

        fn revoke_all(&self, cancellation: CancellationToken) -> RoutedFuture<()> {
            let step = self.shared.revokes.lock().unwrap().pop_front();
            self.play(RoutedCall::RevokeAll, step, cancellation)
        }

        fn origins(
            &self,
            token: RoutedApprovalToken,
            cancellation: CancellationToken,
        ) -> RoutedFuture<Vec<RoutedProposalOriginSummary>> {
            let origins = self.shared.origins.clone();
            self.play(
                RoutedCall::Origins(token),
                Some(RoutedStep::Immediate(Ok(origins))),
                cancellation,
            )
        }
    }

    fn tunnel_summary() -> RoutedProposalSummary {
        let interface = InterfaceId::new(7);
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![NetworkInterface::new(
                interface,
                "synthetic-controller-tunnel",
                InterfaceKind::Tunnel,
                true,
                ["10.250.0.2/32".parse().unwrap()],
            )],
            vec![NetworkRoute::effective(
                "172.31.90.8/30".parse().unwrap(),
                Some(interface),
                RouteKind::Unicast,
                RouteScope::OnLink,
            )],
        );
        let candidates = select_route_candidates(&snapshot, &[]).unwrap();
        RoutedScanProposal::from_route_candidates(
            &snapshot,
            &candidates,
            &RouteFingerprintKey::from_bytes([7; 32]),
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
        .unwrap()
        .summary()
        .clone()
    }

    fn routed_observation(device_id: DeviceId, source: &str) -> DiscoveryObservation {
        DiscoveryObservation {
            device_id,
            source: source.parse().unwrap(),
            method: DiscoveryMethod::RoutedTargeted,
            interface: None,
            device_types: vec![1],
            tuner_count: Some(2),
            advertised_base_url: None,
            advertised_lineup_url: None,
        }
    }

    fn routed_report(observations: Vec<DiscoveryObservation>) -> DiscoveryReport {
        DiscoveryReport {
            observations,
            ..DiscoveryReport::default()
        }
    }

    fn start_routed(
        routed: ScriptedRoutedService,
    ) -> (
        ControllerRuntime,
        ScriptedService,
        std_mpsc::Receiver<ServiceStart>,
    ) {
        let (discovery, discovery_starts) = ScriptedService::new([ServiceStep::Immediate(Ok(
            report(first_id(), "127.0.0.1:65001", 4),
        ))]);
        let (selection, _) = ScriptedSelectionService::new([]);
        let controller =
            ControllerRuntime::start_with_test_services(discovery.clone(), selection, routed)
                .unwrap();
        (controller, discovery, discovery_starts)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_proposal_and_approval_travel_as_scalars_with_origins_on_request() {
        let summary = tunnel_summary();
        let routed = ScriptedRoutedService::new(summary.origins().to_vec());
        let token = RoutedApprovalToken::new(41);
        routed.script_proposal(RoutedStep::Immediate(Ok(RoutedProposal::new(
            token,
            summary.clone(),
        ))));
        routed.script_approval(RoutedStep::Immediate(Ok(())));
        let (controller, _discovery, _starts) = start_routed(routed.clone());
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::ProposeRoutedDiscovery)
            .unwrap();
        let proposing = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.routed().proposal() != RoutedProposalStatus::None
        })
        .await;
        assert_eq!(
            proposing.routed().availability(),
            RoutedAvailability::Available
        );
        let proposed = wait_for_snapshot(&mut snapshots, |snapshot| {
            matches!(
                snapshot.routed().proposal(),
                RoutedProposalStatus::Proposed(_)
            )
        })
        .await;
        let RoutedProposalStatus::Proposed(state) = proposed.routed().proposal() else {
            unreachable!()
        };
        assert_eq!(state.token(), token);
        assert_eq!(
            usize::from(state.candidate_count()),
            summary.candidate_count()
        );
        assert_eq!(
            usize::from(state.maximum_request_datagrams()),
            summary.maximum_request_datagrams()
        );
        assert_eq!(usize::from(state.origin_count()), summary.origins().len());
        assert!(!state.approved());
        assert_eq!(proposed.discovery().kind(), DiscoveryKind::Local);
        let rendered = format!("{proposed:?}");
        assert!(!rendered.contains("synthetic-controller-tunnel"));
        assert!(!rendered.contains("172.31.90"));

        let origins = handle
            .try_routed_proposal_origins(token)
            .unwrap()
            .receive()
            .await
            .unwrap();
        assert_eq!(origins.len(), summary.origins().len());
        assert_eq!(origins[0].interface_name(), "synthetic-controller-tunnel");

        handle
            .try_send(ControllerCommand::ApproveRoutedDiscovery(
                RoutedApprovalToken::new(40),
            ))
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.routed().proposal()
                == RoutedProposalStatus::Failed(DiscoveryFailure::RoutedProposalChanged)
        })
        .await;
        assert!(
            !routed
                .calls()
                .contains(&RoutedCall::Approve(RoutedApprovalToken::new(40)))
        );

        routed.script_proposal(RoutedStep::Immediate(Ok(RoutedProposal::new(
            token,
            summary.clone(),
        ))));
        handle
            .try_send(ControllerCommand::ProposeRoutedDiscovery)
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            matches!(snapshot.routed().proposal(), RoutedProposalStatus::Proposed(state) if !state.approved())
        })
        .await;
        handle
            .try_send(ControllerCommand::ApproveRoutedDiscovery(token))
            .unwrap();
        let approved = wait_for_snapshot(&mut snapshots, |snapshot| {
            matches!(snapshot.routed().proposal(), RoutedProposalStatus::Proposed(state) if state.approved())
        })
        .await;
        assert_eq!(
            approved.discovery_generation(),
            OperationGeneration::INITIAL
        );
        assert!(routed.calls().contains(&RoutedCall::Approve(token)));
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_run_admits_only_routed_replies_on_the_discovery_port() {
        let routed = ScriptedRoutedService::new(Vec::new());
        let (release_run, release) = oneshot::channel();
        routed.script_run(RoutedStep::Gated { release });
        let (controller, _discovery, _starts) = start_routed(routed.clone());
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            ))
            .unwrap();
        let refreshing = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Refreshing
        })
        .await;
        assert_eq!(refreshing.discovery().kind(), DiscoveryKind::Routed);
        release_run
            .send(Ok(RoutedRunOutcome::Report(routed_report(vec![
                routed_observation(first_id(), "172.31.90.9:65001"),
                routed_observation(second_id(), "172.31.90.10:65001"),
            ]))))
            .expect("the gated routed run should still be in flight");
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(ready.discovery().kind(), DiscoveryKind::Routed);
        assert_eq!(ready.devices().len(), 2);
        assert!(
            ready
                .devices()
                .iter()
                .all(|device| device.preferred_locator().port() == DISCOVERY_UDP_PORT)
        );
        assert_eq!(
            routed.calls(),
            vec![RoutedCall::Run(RoutedScanTrigger::ExplicitRefresh)]
        );

        // A reply that is not a routed reply cannot enter the registry.
        let mut wrong_method = routed_observation(first_id(), "172.31.90.9:65001");
        wrong_method.method = DiscoveryMethod::Targeted;
        routed.script_run(RoutedStep::Immediate(Ok(RoutedRunOutcome::Report(
            routed_report(vec![wrong_method]),
        ))));
        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::Automatic,
            ))
            .unwrap();
        let rejected = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Failed(DiscoveryFailure::Internal)
        })
        .await;
        assert_eq!(rejected.discovery().kind(), DiscoveryKind::Routed);
        assert_eq!(
            rejected.devices().len(),
            2,
            "the retained batch is untouched"
        );
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_decisions_become_failures_and_cooldown_is_shown_then_cleared() {
        let routed = ScriptedRoutedService::new(Vec::new());
        routed.script_run(RoutedStep::Immediate(Ok(RoutedRunOutcome::NeedsApproval)));
        routed.script_run(RoutedStep::Immediate(Ok(RoutedRunOutcome::CoolingDown {
            remaining: Duration::from_secs(90),
        })));
        let (release_run, release) = oneshot::channel();
        routed.script_run(RoutedStep::Gated { release });
        let (controller, _discovery, _starts) = start_routed(routed.clone());
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::Automatic,
            ))
            .unwrap();
        let unapproved = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status()
                == DiscoveryStatus::Failed(DiscoveryFailure::RoutedNotApproved)
        })
        .await;
        assert_eq!(unapproved.routed().cooldown_seconds(), None);

        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::Automatic,
            ))
            .unwrap();
        let cooling = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status()
                == DiscoveryStatus::Failed(DiscoveryFailure::RoutedCoolingDown)
        })
        .await;
        assert_eq!(cooling.routed().cooldown_seconds(), Some(90));

        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            ))
            .unwrap();
        let refreshing = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Refreshing
        })
        .await;
        assert_eq!(refreshing.routed().cooldown_seconds(), None);
        release_run
            .send(Ok(RoutedRunOutcome::Report(routed_report(Vec::new()))))
            .expect("the gated routed run should still be in flight");
        let empty = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::NoResponse
        })
        .await;
        assert_eq!(empty.discovery().kind(), DiscoveryKind::Routed);
        assert!(empty.devices().is_empty());
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_run_shares_the_single_superseding_discovery_lane() {
        let routed = ScriptedRoutedService::new(Vec::new());
        let (started, started_rx) = std_mpsc::channel();
        let (cancelled, cancelled_rx) = std_mpsc::channel();
        routed.script_run(RoutedStep::UntilCancelled { started, cancelled });
        let (controller, _discovery, starts) = start_routed(routed);
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            ))
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().kind() == DiscoveryKind::Routed
                && snapshot.discovery().status() == DiscoveryStatus::Refreshing
        })
        .await;
        started_rx
            .recv_timeout(WAIT)
            .expect("the routed run is in flight before it is superseded");
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .unwrap();
        cancelled_rx
            .recv_timeout(WAIT)
            .expect("the routed run observes supersession");
        recv_start(&starts);
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(ready.discovery().kind(), DiscoveryKind::Local);
        assert_eq!(ready.devices().len(), 1);
        controller.shutdown().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn revocation_forgets_the_shown_proposal_and_unavailable_services_say_why() {
        let summary = tunnel_summary();
        let routed = ScriptedRoutedService::new(Vec::new());
        routed.script_proposal(RoutedStep::Immediate(Ok(RoutedProposal::new(
            RoutedApprovalToken::new(1),
            summary,
        ))));
        routed.script_revoke(RoutedStep::Immediate(Ok(())));
        let (controller, _discovery, _starts) = start_routed(routed.clone());
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::ProposeRoutedDiscovery)
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            matches!(
                snapshot.routed().proposal(),
                RoutedProposalStatus::Proposed(_)
            )
        })
        .await;
        handle
            .try_send(ControllerCommand::RevokeRoutedApprovals)
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.routed().proposal() == RoutedProposalStatus::None
        })
        .await;
        assert!(routed.calls().contains(&RoutedCall::RevokeAll));
        controller.shutdown().unwrap();

        let (discovery, _starts) = ScriptedService::new([]);
        let controller = ControllerRuntime::start(discovery).unwrap();
        let handle = controller.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            ))
            .unwrap();
        let unavailable = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.discovery().status()
                == DiscoveryStatus::Failed(DiscoveryFailure::RoutedUnavailable)
        })
        .await;
        assert_eq!(unavailable.discovery().kind(), DiscoveryKind::Routed);
        assert_eq!(
            unavailable.routed().availability(),
            RoutedAvailability::Unavailable(RoutedUnavailableReason::NotConfigured)
        );
        handle
            .try_send(ControllerCommand::ProposeRoutedDiscovery)
            .unwrap();
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.routed().proposal()
                == RoutedProposalStatus::Failed(DiscoveryFailure::RoutedUnavailable)
        })
        .await;
        controller.shutdown().unwrap();
    }

    #[test]
    fn routed_batches_are_validated_before_the_registry_is_rebuilt() {
        let actor = test_actor();
        let scope = DiscoveryScope::Routed(RoutedScanTrigger::Automatic);
        let accepted = actor
            .build_discovery_update(
                scope,
                routed_report(vec![
                    routed_observation(first_id(), "172.31.90.9:65001"),
                    routed_observation(second_id(), "172.31.90.10:65001"),
                ]),
            )
            .unwrap();
        assert_eq!(accepted.devices.len(), 2);
        assert!(accepted.routed_batch.is_some());

        let mut with_interface = routed_observation(first_id(), "172.31.90.9:65001");
        with_interface.interface = Some("synthetic0".to_owned());
        assert!(
            actor
                .build_discovery_update(scope, routed_report(vec![with_interface]))
                .is_err()
        );
        assert!(
            actor
                .build_discovery_update(
                    scope,
                    routed_report(vec![routed_observation(first_id(), "172.31.90.9:5004")]),
                )
                .is_err()
        );
        assert!(
            actor
                .build_discovery_update(
                    scope,
                    routed_report(vec![
                        routed_observation(first_id(), "172.31.90.9:65001"),
                        routed_observation(first_id(), "172.31.90.9:65001"),
                    ]),
                )
                .is_err()
        );
        let too_many = (0..=MAX_RETAINED_ROUTED_OBSERVATIONS)
            .map(|index| {
                let octet = u8::try_from(index % 200).unwrap();
                let block = index / 200;
                routed_observation(first_id(), &format!("10.{block}.1.{octet}:65001"))
            })
            .collect::<Vec<_>>();
        assert!(
            actor
                .build_discovery_update(scope, routed_report(too_many))
                .is_err()
        );
    }
}
