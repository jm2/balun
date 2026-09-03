//! The routed-discovery lane's service boundary and its Linux supervisor.
//!
//! The controller actor runs on one current-thread runtime, so routed work,
//! whose approval store performs synchronous locked file I/O and whose
//! observer pair lives for the whole session, is owned by a separate
//! supervisor thread. The actor talks to it only through
//! [`RoutedDiscoveryService`], a packet-free boundary whose every call is
//! bounded by a cancellation token. Nothing crossing this boundary carries
//! topology except the origin list a caller explicitly asks for through a
//! private reply.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::state::{DiscoveryFailure, RoutedApprovalToken, RoutedUnavailableReason};
use crate::discovery::{
    DiscoveryReport, RoutedProposalOriginSummary, RoutedProposalSummary, RoutedScanTrigger,
};

/// One bounded routed operation whose failure is already a fixed category.
pub type RoutedFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, DiscoveryFailure>> + Send + 'static>>;

/// One proposal the user may approve, identified by a session-unique token.
#[derive(Clone, Eq, PartialEq)]
pub struct RoutedProposal {
    token: RoutedApprovalToken,
    summary: RoutedProposalSummary,
}

impl RoutedProposal {
    #[must_use]
    pub const fn new(token: RoutedApprovalToken, summary: RoutedProposalSummary) -> Self {
        Self { token, summary }
    }

    #[must_use]
    pub const fn token(&self) -> RoutedApprovalToken {
        self.token
    }

    #[must_use]
    pub const fn summary(&self) -> &RoutedProposalSummary {
        &self.summary
    }
}

impl fmt::Debug for RoutedProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedProposal")
            .field("token", &self.token)
            .field("summary", &self.summary)
            .finish()
    }
}

/// What one routed run did, as the discovery lane needs to know it.
pub enum RoutedRunOutcome {
    /// The scan ran to its settled completion. `interfaces` names the tunnel
    /// interfaces the approved proposal was bound to, so a later network
    /// change can expire exactly this evidence; they never enter a snapshot.
    Report {
        report: DiscoveryReport,
        interfaces: Vec<String>,
    },
    /// The current proposal has no remembered approval; nothing was sent.
    NeedsApproval,
    /// Automatic runs are cooling down; nothing was sent.
    CoolingDown { remaining: Duration },
    /// Another reservation is active; nothing was sent.
    Busy,
    /// The store published a reservation it could not confirm; nothing was sent.
    Unconfirmed,
}

impl fmt::Debug for RoutedRunOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Report { report, interfaces } => formatter
                .debug_struct("Report")
                .field("observations", &report.observations.len())
                .field("issues", &report.issues.len())
                .field("interface_count", &interfaces.len())
                .finish(),
            Self::NeedsApproval => formatter.write_str("NeedsApproval"),
            Self::CoolingDown { remaining } => formatter
                .debug_struct("CoolingDown")
                .field("remaining", remaining)
                .finish(),
            Self::Busy => formatter.write_str("Busy"),
            Self::Unconfirmed => formatter.write_str("Unconfirmed"),
        }
    }
}

/// Packet-free boundary the controller uses for routed discovery.
///
/// Constructing an implementation must not inspect routes, open sockets, or
/// touch the approval store; the first call does. Every method observes its
/// cancellation token promptly, and `run` is the only method that may send
/// datagrams.
pub trait RoutedDiscoveryService: Send + Sync + 'static {
    /// Whether routed discovery can be offered at all, without any I/O.
    fn availability(&self) -> Result<(), RoutedUnavailableReason>;

    /// Build a fresh proposal from the current tunnel routes.
    fn propose(&self, cancellation: CancellationToken) -> RoutedFuture<RoutedProposal>;

    /// Remember approval of the proposal identified by `token`.
    fn approve(
        &self,
        token: RoutedApprovalToken,
        cancellation: CancellationToken,
    ) -> RoutedFuture<()>;

    /// Propose afresh and run the approved scan.
    fn run(
        &self,
        trigger: RoutedScanTrigger,
        cancellation: CancellationToken,
    ) -> RoutedFuture<RoutedRunOutcome>;

    /// Forget every remembered approval.
    fn revoke_all(&self, cancellation: CancellationToken) -> RoutedFuture<()>;

    /// The origins (tunnel interface and network) behind the proposal
    /// identified by `token`, for the approval dialog only.
    fn origins(
        &self,
        token: RoutedApprovalToken,
        cancellation: CancellationToken,
    ) -> RoutedFuture<Vec<RoutedProposalOriginSummary>>;
}

/// A routed service for systems or configurations that cannot offer it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnavailableRoutedDiscovery {
    reason: RoutedUnavailableReason,
}

impl UnavailableRoutedDiscovery {
    #[must_use]
    pub const fn new(reason: RoutedUnavailableReason) -> Self {
        Self { reason }
    }

    fn unavailable<T: Send + 'static>() -> RoutedFuture<T> {
        Box::pin(async { Err(DiscoveryFailure::RoutedUnavailable) })
    }
}

impl RoutedDiscoveryService for UnavailableRoutedDiscovery {
    fn availability(&self) -> Result<(), RoutedUnavailableReason> {
        Err(self.reason)
    }

    fn propose(&self, _cancellation: CancellationToken) -> RoutedFuture<RoutedProposal> {
        Self::unavailable()
    }

    fn approve(
        &self,
        _token: RoutedApprovalToken,
        _cancellation: CancellationToken,
    ) -> RoutedFuture<()> {
        Self::unavailable()
    }

    fn run(
        &self,
        _trigger: RoutedScanTrigger,
        _cancellation: CancellationToken,
    ) -> RoutedFuture<RoutedRunOutcome> {
        Self::unavailable()
    }

    fn revoke_all(&self, _cancellation: CancellationToken) -> RoutedFuture<()> {
        Self::unavailable()
    }

    fn origins(
        &self,
        _token: RoutedApprovalToken,
        _cancellation: CancellationToken,
    ) -> RoutedFuture<Vec<RoutedProposalOriginSummary>> {
        Self::unavailable()
    }
}

/// Receives the origins of one proposal outside the snapshot channel.
#[derive(Debug)]
pub struct RoutedOriginsReceiver {
    receiver: oneshot::Receiver<Result<Vec<RoutedProposalOriginSummary>, DiscoveryFailure>>,
}

impl RoutedOriginsReceiver {
    pub(super) const fn new(
        receiver: oneshot::Receiver<Result<Vec<RoutedProposalOriginSummary>, DiscoveryFailure>>,
    ) -> Self {
        Self { receiver }
    }

    /// Wait for the origins; a controller that stopped first is an internal
    /// failure.
    pub async fn receive(self) -> Result<Vec<RoutedProposalOriginSummary>, DiscoveryFailure> {
        self.receiver
            .await
            .unwrap_or(Err(DiscoveryFailure::Internal))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use thiserror::Error;
    use tokio::runtime::Builder;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    use super::{RoutedDiscoveryService, RoutedFuture, RoutedProposal, RoutedRunOutcome};
    use crate::controller::state::{
        DiscoveryFailure, RoutedApprovalToken, RoutedUnavailableReason,
    };
    use crate::discovery::approval::{
        ApprovalStore, CompletedRoutedRun, LinuxObserverPairFactory, MonitoredRoutedDiscovery,
        MonitoredRoutedError, MonitoredRoutedRun, PinnedSocketProber, RoutedObserverCoordinator,
        RoutedProposalError, StoreError, StorePaths, StoredRoutedProposal, SystemRoutedClock,
    };
    use crate::discovery::{
        DiscoveryClient, DiscoveryError, ProbeConfig, RoutedProposalOriginSummary,
        RoutedProposalSummary, RoutedScanConfig, RoutedScanTrigger,
    };

    const SUPERVISOR_THREAD_NAME: &str = "balun-routed";
    const SUPERVISOR_COMMAND_CAPACITY: usize = 8;
    const OBSERVERS_UNTRIED: u8 = 0;
    const OBSERVERS_LIVE: u8 = 1;
    const OBSERVERS_FAILED: u8 = 2;
    const DIRECTORY_UNAVAILABLE: u8 = 3;
    /// Per-target probe budget: the same fixed traffic bound as an exact probe.
    const ROUTED_PROBE_ATTEMPTS: u8 = 2;
    const ROUTED_PROBE_RESPONSE_WINDOW: Duration = Duration::from_millis(200);
    const ROUTED_PROBE_MAX_RECEIVED_DATAGRAMS: usize = 16;
    const ROUTED_PROBE_MAX_UNIQUE_DEVICES: usize = 1;

    type Runner = MonitoredRoutedDiscovery<LinuxObserverPairFactory, PinnedSocketProber>;

    /// Why the routed supervisor thread could not start.
    #[derive(Debug, Error)]
    pub enum RoutedStartError {
        #[error("could not spawn the routed discovery thread: {0}")]
        ThreadSpawn(std::io::Error),
    }

    enum SupervisorCommand {
        Propose {
            reply: oneshot::Sender<Result<RoutedProposal, DiscoveryFailure>>,
        },
        Approve {
            token: RoutedApprovalToken,
            reply: oneshot::Sender<Result<(), DiscoveryFailure>>,
        },
        Run {
            trigger: RoutedScanTrigger,
            cancellation: CancellationToken,
            reply: oneshot::Sender<Result<RoutedRunOutcome, DiscoveryFailure>>,
        },
        RevokeAll {
            reply: oneshot::Sender<Result<(), DiscoveryFailure>>,
        },
        Origins {
            token: RoutedApprovalToken,
            reply: oneshot::Sender<Result<Vec<RoutedProposalOriginSummary>, DiscoveryFailure>>,
        },
    }

    /// The production routed service: one supervisor thread owning the
    /// monitored runner, the approval store, and the observer pair.
    ///
    /// Starting it spawns the thread and nothing else; the first command
    /// creates the private store directory and establishes the observers.
    pub struct LinuxRoutedDiscovery {
        commands: mpsc::Sender<SupervisorCommand>,
        shutdown: CancellationToken,
        thread: Mutex<Option<thread::JoinHandle<()>>>,
        observers: Arc<AtomicU8>,
    }

    impl LinuxRoutedDiscovery {
        /// Start the supervisor for the approval store at `directory`, whose
        /// parent is the per-user configuration directory.
        pub fn start(directory: PathBuf) -> Result<Self, RoutedStartError> {
            let (commands, receiver) = mpsc::channel(SUPERVISOR_COMMAND_CAPACITY);
            let shutdown = CancellationToken::new();
            let observers = Arc::new(AtomicU8::new(OBSERVERS_UNTRIED));
            let supervisor = Supervisor {
                directory,
                shutdown: shutdown.clone(),
                observers: Arc::clone(&observers),
                runner: None,
                latest: None,
                next_token: 1,
            };
            let thread = thread::Builder::new()
                .name(SUPERVISOR_THREAD_NAME.to_owned())
                .spawn(move || {
                    let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                        return;
                    };
                    runtime.block_on(supervisor.run(receiver));
                })
                .map_err(RoutedStartError::ThreadSpawn)?;
            Ok(Self {
                commands,
                shutdown,
                thread: Mutex::new(Some(thread)),
                observers,
            })
        }

        fn request<T: Send + 'static>(
            &self,
            cancellation: CancellationToken,
            abandon_on_cancel: bool,
            build: impl FnOnce(oneshot::Sender<Result<T, DiscoveryFailure>>) -> SupervisorCommand,
        ) -> RoutedFuture<T> {
            let (reply, receiver) = oneshot::channel();
            let command = build(reply);
            let sender = self.commands.clone();
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(DiscoveryFailure::Internal);
                }
                sender
                    .send(command)
                    .await
                    .map_err(|_| DiscoveryFailure::RoutedUnavailable)?;
                if abandon_on_cancel {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Err(DiscoveryFailure::Internal),
                        result = receiver => result.unwrap_or(Err(DiscoveryFailure::RoutedUnavailable)),
                    }
                } else {
                    receiver
                        .await
                        .unwrap_or(Err(DiscoveryFailure::RoutedUnavailable))
                }
            })
        }
    }

    impl Drop for LinuxRoutedDiscovery {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Ok(mut thread) = self.thread.lock()
                && let Some(thread) = thread.take()
            {
                let _ = thread.join();
            }
        }
    }

    impl std::fmt::Debug for LinuxRoutedDiscovery {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("LinuxRoutedDiscovery(<redacted>)")
        }
    }

    impl RoutedDiscoveryService for LinuxRoutedDiscovery {
        fn availability(&self) -> Result<(), RoutedUnavailableReason> {
            match self.observers.load(Ordering::SeqCst) {
                OBSERVERS_FAILED => Err(RoutedUnavailableReason::ObserversUnavailable),
                DIRECTORY_UNAVAILABLE => Err(RoutedUnavailableReason::NoPrivateDirectory),
                _ => Ok(()),
            }
        }

        fn propose(&self, cancellation: CancellationToken) -> RoutedFuture<RoutedProposal> {
            self.request(cancellation, true, |reply| SupervisorCommand::Propose {
                reply,
            })
        }

        fn approve(
            &self,
            token: RoutedApprovalToken,
            cancellation: CancellationToken,
        ) -> RoutedFuture<()> {
            self.request(cancellation, true, |reply| SupervisorCommand::Approve {
                token,
                reply,
            })
        }

        fn run(
            &self,
            trigger: RoutedScanTrigger,
            cancellation: CancellationToken,
        ) -> RoutedFuture<RoutedRunOutcome> {
            // The token travels with the run so the runner settles its
            // reservation on cancellation; the reply is always awaited.
            let run_cancellation = cancellation.clone();
            self.request(cancellation, false, move |reply| SupervisorCommand::Run {
                trigger,
                cancellation: run_cancellation,
                reply,
            })
        }

        fn revoke_all(&self, cancellation: CancellationToken) -> RoutedFuture<()> {
            self.request(cancellation, true, |reply| SupervisorCommand::RevokeAll {
                reply,
            })
        }

        fn origins(
            &self,
            token: RoutedApprovalToken,
            cancellation: CancellationToken,
        ) -> RoutedFuture<Vec<RoutedProposalOriginSummary>> {
            self.request(cancellation, true, |reply| SupervisorCommand::Origins {
                token,
                reply,
            })
        }
    }

    struct LatestProposal {
        token: RoutedApprovalToken,
        proposal: StoredRoutedProposal,
        origins: Vec<RoutedProposalOriginSummary>,
    }

    struct Supervisor {
        directory: PathBuf,
        shutdown: CancellationToken,
        observers: Arc<AtomicU8>,
        runner: Option<Runner>,
        latest: Option<LatestProposal>,
        next_token: u64,
    }

    impl Supervisor {
        async fn run(mut self, mut commands: mpsc::Receiver<SupervisorCommand>) {
            loop {
                let command = tokio::select! {
                    biased;
                    () = self.shutdown.cancelled() => break,
                    command = commands.recv() => match command {
                        Some(command) => command,
                        None => break,
                    },
                    _observing = await_replacement(self.runner.as_mut()) => continue,
                };
                self.handle(command).await;
            }
            if let Some(runner) = self.runner.take() {
                runner.shutdown().await;
            }
        }

        async fn handle(&mut self, command: SupervisorCommand) {
            match command {
                SupervisorCommand::Propose { reply } => {
                    let _ = reply.send(self.propose().await);
                }
                SupervisorCommand::Approve { token, reply } => {
                    let _ = reply.send(self.approve(token).await);
                }
                SupervisorCommand::Run {
                    trigger,
                    cancellation,
                    reply,
                } => {
                    let _ = reply.send(self.scan(trigger, &cancellation).await);
                }
                SupervisorCommand::RevokeAll { reply } => {
                    let _ = reply.send(self.revoke_all().await);
                }
                SupervisorCommand::Origins { token, reply } => {
                    let origins = match &self.latest {
                        Some(latest) if latest.token == token => Ok(latest.origins.clone()),
                        _ => Err(DiscoveryFailure::RoutedProposalChanged),
                    };
                    let _ = reply.send(origins);
                }
            }
        }

        async fn propose(&mut self) -> Result<RoutedProposal, DiscoveryFailure> {
            let runner = self.runner().await?;
            let proposal = runner
                .propose(routed_probe_config(), RoutedScanConfig::default())
                .await
                .map_err(failure)?;
            let token = RoutedApprovalToken::new(self.next_token);
            self.next_token = self.next_token.checked_add(1).unwrap_or(1);
            let summary: RoutedProposalSummary = proposal.summary().clone();
            self.latest = Some(LatestProposal {
                token,
                origins: summary.origins().to_vec(),
                proposal,
            });
            Ok(RoutedProposal::new(token, summary))
        }

        async fn approve(&mut self, token: RoutedApprovalToken) -> Result<(), DiscoveryFailure> {
            let Some(latest) = self.latest.as_ref().filter(|latest| latest.token == token) else {
                return Err(DiscoveryFailure::RoutedProposalChanged);
            };
            let proposal = latest.proposal.clone();
            let runner = self.runner().await?;
            let commit = runner.approve(&proposal).await.map_err(failure)?;
            if commit.is_confirmed() {
                Ok(())
            } else {
                Err(DiscoveryFailure::RoutedUnconfirmed)
            }
        }

        async fn scan(
            &mut self,
            trigger: RoutedScanTrigger,
            cancellation: &CancellationToken,
        ) -> Result<RoutedRunOutcome, DiscoveryFailure> {
            let runner = self.runner().await?;
            let proposal = runner
                .propose(routed_probe_config(), RoutedScanConfig::default())
                .await
                .map_err(failure)?;
            let interfaces = proposal
                .summary()
                .origins()
                .iter()
                .map(|origin| origin.interface_name().to_owned())
                .collect::<Vec<_>>();
            let run = runner
                .run(proposal, trigger, cancellation)
                .await
                .map_err(failure)?;
            Ok(match run {
                MonitoredRoutedRun::Completed(CompletedRoutedRun { result, .. }) => {
                    RoutedRunOutcome::Report {
                        report: result.map_err(scan_failure)?,
                        interfaces,
                    }
                }
                MonitoredRoutedRun::NeedsApproval(_) => RoutedRunOutcome::NeedsApproval,
                MonitoredRoutedRun::CoolingDown { remaining } => {
                    RoutedRunOutcome::CoolingDown { remaining }
                }
                MonitoredRoutedRun::Busy => RoutedRunOutcome::Busy,
                MonitoredRoutedRun::PublishedWithoutPermit { .. } => RoutedRunOutcome::Unconfirmed,
            })
        }

        async fn revoke_all(&mut self) -> Result<(), DiscoveryFailure> {
            let runner = self.runner().await?;
            runner.revoke_all().await.map_err(failure)?;
            self.latest = None;
            Ok(())
        }

        /// Establish the runner on first use: the private directory's parent
        /// must exist, then the observer pair is started.
        async fn runner(&mut self) -> Result<&mut Runner, DiscoveryFailure> {
            if self.runner.is_none() {
                let Some(parent) = self.directory.parent() else {
                    self.observers
                        .store(DIRECTORY_UNAVAILABLE, Ordering::SeqCst);
                    return Err(DiscoveryFailure::RoutedUnavailable);
                };
                if std::fs::create_dir_all(parent).is_err() {
                    self.observers
                        .store(DIRECTORY_UNAVAILABLE, Ordering::SeqCst);
                    return Err(DiscoveryFailure::RoutedUnavailable);
                }
                // Load once before observing so the private directory and its
                // lock exist; otherwise the observer's own exact reread would
                // create them and reject its baseline as a concurrent change.
                let store = Arc::new(ApprovalStore::new(StorePaths::new(self.directory.clone())));
                if store.load().is_err() {
                    self.observers
                        .store(DIRECTORY_UNAVAILABLE, Ordering::SeqCst);
                    return Err(DiscoveryFailure::RoutedUnavailable);
                }
                let started = MonitoredRoutedDiscovery::start(
                    Arc::new(RoutedObserverCoordinator::new()),
                    store,
                    Arc::new(SystemRoutedClock),
                    Arc::new(LinuxObserverPairFactory),
                    Arc::new(PinnedSocketProber::new(DiscoveryClient::new(
                        routed_probe_config(),
                    ))),
                )
                .await;
                match started {
                    Ok(runner) => {
                        self.observers.store(OBSERVERS_LIVE, Ordering::SeqCst);
                        self.runner = Some(runner);
                    }
                    Err(_) => {
                        self.observers.store(OBSERVERS_FAILED, Ordering::SeqCst);
                        return Err(DiscoveryFailure::RoutedUnavailable);
                    }
                }
            }
            self.runner
                .as_mut()
                .ok_or(DiscoveryFailure::RoutedUnavailable)
        }
    }

    async fn await_replacement(runner: Option<&mut Runner>) -> bool {
        match runner {
            Some(runner) => runner.await_replacement().await,
            None => std::future::pending().await,
        }
    }

    fn routed_probe_config() -> ProbeConfig {
        ProbeConfig::new(
            ROUTED_PROBE_ATTEMPTS,
            ROUTED_PROBE_RESPONSE_WINDOW,
            ROUTED_PROBE_MAX_RECEIVED_DATAGRAMS,
            ROUTED_PROBE_MAX_UNIQUE_DEVICES,
        )
        .expect("fixed routed probe budget must be valid")
    }

    fn failure(error: MonitoredRoutedError) -> DiscoveryFailure {
        match error {
            MonitoredRoutedError::NotObserving | MonitoredRoutedError::Observers(_) => {
                DiscoveryFailure::RoutedUnavailable
            }
            MonitoredRoutedError::Candidates(_)
            | MonitoredRoutedError::Store(StoreError::InvalidProposal(
                RoutedProposalError::EmptyProposal,
            )) => DiscoveryFailure::RoutedNoCandidates,
            MonitoredRoutedError::Coordinator(_)
            | MonitoredRoutedError::Admission(_)
            | MonitoredRoutedError::Store(_)
            | MonitoredRoutedError::Targets(_) => DiscoveryFailure::Internal,
        }
    }

    fn scan_failure(error: DiscoveryError) -> DiscoveryFailure {
        match error {
            DiscoveryError::Interfaces(_) => DiscoveryFailure::InterfaceEnumeration,
            DiscoveryError::Io { .. }
            | DiscoveryError::ShortSend { .. }
            | DiscoveryError::RoutedScanDeadline { .. } => DiscoveryFailure::Network,
            DiscoveryError::InvalidEndpoint { .. }
            | DiscoveryError::Task(_)
            | DiscoveryError::Cancelled
            | DiscoveryError::Protocol(_) => DiscoveryFailure::Internal,
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{LinuxRoutedDiscovery, RoutedStartError};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_service_reports_its_reason_and_refuses_everything() {
        let service = UnavailableRoutedDiscovery::new(RoutedUnavailableReason::UnsupportedPlatform);
        assert_eq!(
            service.availability(),
            Err(RoutedUnavailableReason::UnsupportedPlatform)
        );
        let token = CancellationToken::new();
        assert!(matches!(
            service.propose(token.clone()).await,
            Err(DiscoveryFailure::RoutedUnavailable)
        ));
        assert!(matches!(
            service
                .approve(RoutedApprovalToken::new(1), token.clone())
                .await,
            Err(DiscoveryFailure::RoutedUnavailable)
        ));
        assert!(matches!(
            service
                .run(RoutedScanTrigger::ExplicitRefresh, token.clone())
                .await,
            Err(DiscoveryFailure::RoutedUnavailable)
        ));
        assert!(matches!(
            service.revoke_all(token.clone()).await,
            Err(DiscoveryFailure::RoutedUnavailable)
        ));
        assert!(matches!(
            service.origins(RoutedApprovalToken::new(1), token).await,
            Err(DiscoveryFailure::RoutedUnavailable)
        ));
    }

    #[tokio::test]
    async fn dropped_origins_reply_is_an_internal_failure() {
        let (sender, receiver) = oneshot::channel();
        drop(sender);
        assert_eq!(
            RoutedOriginsReceiver::new(receiver).receive().await,
            Err(DiscoveryFailure::Internal)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_supervisor_starts_inert_and_answers_from_the_real_observers() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("settings").join("routed-approvals");
        let service = LinuxRoutedDiscovery::start(directory.clone()).unwrap();
        assert_eq!(service.availability(), Ok(()));
        assert!(
            !directory.parent().unwrap().exists(),
            "starting performs no I/O"
        );

        let token = CancellationToken::new();
        let proposal = service.propose(token.clone()).await;
        match &proposal {
            Ok(proposal) => assert!(proposal.summary().candidate_count() > 0),
            Err(DiscoveryFailure::RoutedNoCandidates) => {}
            Err(failure) => panic!("unexpected proposal failure {failure:?}"),
        }
        assert!(
            directory.exists(),
            "the first command creates the private store"
        );
        assert_eq!(
            service
                .approve(RoutedApprovalToken::new(u64::MAX), token.clone())
                .await,
            Err(DiscoveryFailure::RoutedProposalChanged)
        );
        assert_eq!(
            service
                .origins(RoutedApprovalToken::new(u64::MAX), token.clone())
                .await,
            Err(DiscoveryFailure::RoutedProposalChanged)
        );
        let run = service
            .run(RoutedScanTrigger::ExplicitRefresh, token.clone())
            .await;
        assert!(
            matches!(
                run,
                Err(DiscoveryFailure::RoutedNoCandidates)
                    | Ok(RoutedRunOutcome::NeedsApproval | RoutedRunOutcome::Report { .. })
            ),
            "{run:?}"
        );
        assert_eq!(service.revoke_all(token).await, Ok(()));
        drop(service);
    }
}
