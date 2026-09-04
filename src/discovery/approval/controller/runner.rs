//! The monitored Linux routed-discovery runner.
//!
//! One runner owns the live route/store observer pair, the durable approval
//! store, the admission clock, and the probe transport, and turns one approved
//! proposal into exactly one bounded scan:
//!
//! 1. reserve the run durably while a registration in the current combined
//!    epoch guards the write;
//! 2. retire the observer pair that saw Balun's own publication and establish
//!    a replacement whose exact reread matches the published reservation and
//!    revalidates it against the fresh route snapshot, inside the store
//!    watcher's own sandwich;
//! 3. register in the fresh combined epoch, fix one absolute deadline at the
//!    final pre-send clock boundary, and probe every revalidated target
//!    through a socket pinned to that target's interface, re-checking
//!    authority, the deadline, and the pin immediately before every datagram;
//! 4. settle completion durably on every exit path, including drops; and
//! 5. rebaseline the observer pair again so the next run starts healthy.
//!
//! Nothing here prints or retains topology; addresses stay in the report
//! handed back to the caller.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::discovery::approval::gate::{RevalidatedRoutedScan, RevalidatedRoutedTarget};
use crate::discovery::approval::run::{
    AdmissionTimeline, AdmittedRoutedScan, RoutedAdmissionClock, RoutedAdmissionError,
    RoutedClockReadError, RoutedClockSample, checkpoint, ensure_live, sample_live,
};
use crate::discovery::approval::store::{
    ApprovalStore, CommitDurability, StoreCommit, StoreError, StoredBeginDecision,
    StoredCompletionDecision, StoredPublishedReservation, StoredRoutedProposal,
};
use crate::discovery::approval::{
    RESERVATION_LEASE, RouteFingerprint, RoutedPolicyTime, RoutedProposalSummary, RoutedRunId,
    RoutedScanOutcome, RoutedScanTrigger,
};
use crate::discovery::client::{DiscoveryClient, DiscoveryError, DiscoveryReport, ProbeConfig};
use crate::discovery::routed::linux::{
    PinnedRoutedUdpSocketError, PreSendAuthority, open_pinned_routed_udp_socket,
};
use crate::discovery::routed::{
    ApprovedIpv4Targets, RoutedScanConfig, RoutedTargetsError, scan_approved_targets_until,
};
use crate::discovery::routes::{
    InterfaceId, RouteCandidateError, RouteSnapshot, select_route_candidates,
};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;

use super::pair::{LinuxObserverPair, LinuxObserverPairError, LinuxObserverPairEvent};
use super::{
    HealthyRoutedEpoch, ObserverCoordinatorError, RoutedAuthorityRegistration,
    RoutedObserverCoordinator,
};

/// Opaque, topology-free reason an observer pair could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObserverPairFailure(LinuxObserverPairError);

impl fmt::Display for ObserverPairFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for ObserverPairFailure {}

/// One coalesced signal that the whole observer pair must be replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObserverPairEvent {
    ReplacementRequired,
}

/// Establishes one live route/store observer pair around an exact reread.
///
/// The production factory drives the real rtnetlink and inotify observers;
/// tests substitute a pair that mints genuine coordinator epochs without the
/// kernel sources.
pub(crate) trait ObserverPairFactory: Send + Sync + 'static {
    type Pair: ObserverPair;

    fn prepare_and_activate<R, E, Read>(
        &self,
        coordinator: Arc<RoutedObserverCoordinator>,
        store: Arc<ApprovalStore>,
        exact_reread: Read,
    ) -> impl Future<
        Output = Result<(RouteSnapshot, R, HealthyRoutedEpoch, Self::Pair), ObserverPairFailure>,
    > + Send
    where
        R: Send + 'static,
        E: Send + 'static,
        Read: FnOnce(&ApprovalStore, &RouteSnapshot) -> Result<R, E> + Send + 'static;
}

/// One live observer pair whose replacement the runner owns.
pub(crate) trait ObserverPair: Send + 'static {
    fn next_event(&mut self) -> impl Future<Output = ObserverPairEvent> + Send;

    fn shutdown(self) -> impl Future<Output = ()> + Send;
}

/// The production factory over the real Linux observers.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LinuxObserverPairFactory;

impl ObserverPairFactory for LinuxObserverPairFactory {
    type Pair = LinuxObserverPair;

    async fn prepare_and_activate<R, E, Read>(
        &self,
        coordinator: Arc<RoutedObserverCoordinator>,
        store: Arc<ApprovalStore>,
        exact_reread: Read,
    ) -> Result<(RouteSnapshot, R, HealthyRoutedEpoch, Self::Pair), ObserverPairFailure>
    where
        R: Send + 'static,
        E: Send + 'static,
        Read: FnOnce(&ApprovalStore, &RouteSnapshot) -> Result<R, E> + Send + 'static,
    {
        LinuxObserverPair::prepare_and_activate(coordinator, store, exact_reread)
            .await
            .map_err(ObserverPairFailure)
    }
}

impl ObserverPair for LinuxObserverPair {
    async fn next_event(&mut self) -> ObserverPairEvent {
        let LinuxObserverPairEvent::ReplacementRequired = Self::next_event(self).await;
        ObserverPairEvent::ReplacementRequired
    }

    fn shutdown(self) -> impl Future<Output = ()> + Send {
        let shutdown = Self::shutdown(self);
        async move {
            let _terminations = shutdown.await;
        }
    }
}

/// One revalidated target and the exact interface its probe must leave by.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RoutedProbeTarget {
    address: Ipv4Addr,
    interface_id: InterfaceId,
    interface_name: String,
}

impl RoutedProbeTarget {
    #[must_use]
    pub(crate) const fn address(&self) -> Ipv4Addr {
        self.address
    }

    #[must_use]
    pub(crate) const fn interface_id(&self) -> InterfaceId {
        self.interface_id
    }

    #[must_use]
    pub(crate) fn interface_name(&self) -> &str {
        &self.interface_name
    }

    fn from_revalidated(target: &RevalidatedRoutedTarget) -> Self {
        Self {
            address: target.address(),
            interface_id: target.interface_id(),
            interface_name: target.interface_name().to_owned(),
        }
    }
}

impl fmt::Debug for RoutedProbeTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedProbeTarget(<redacted>)")
    }
}

/// Sends one target's discovery attempts through a socket pinned to that
/// target's interface, consulting `authority` before every datagram.
pub(crate) trait RoutedTargetProber: Send + Sync + 'static {
    fn probe(
        &self,
        target: RoutedProbeTarget,
        authority: PreSendAuthority,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<DiscoveryReport, DiscoveryError>> + Send;
}

/// The production prober: one freshly pinned socket per target probe.
#[derive(Clone, Debug)]
pub(crate) struct PinnedSocketProber {
    client: DiscoveryClient,
}

impl PinnedSocketProber {
    #[must_use]
    pub(crate) const fn new(client: DiscoveryClient) -> Self {
        Self { client }
    }
}

impl RoutedTargetProber for PinnedSocketProber {
    fn probe(
        &self,
        target: RoutedProbeTarget,
        authority: PreSendAuthority,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<DiscoveryReport, DiscoveryError>> + Send {
        let client = self.client.clone();
        async move {
            let destination =
                SocketAddr::V4(SocketAddrV4::new(target.address(), DISCOVERY_UDP_PORT));
            let pinned =
                open_pinned_routed_udp_socket(target.interface_id(), target.interface_name())
                    .map_err(|error| pin_failure(destination, error))?;
            let socket = pinned
                .into_probe_socket(authority)
                .map_err(|error| pin_failure(destination, error))?;
            client
                .discover_routed_target_through(&socket, target.address(), &cancellation)
                .await
        }
    }
}

fn pin_failure(destination: SocketAddr, error: PinnedRoutedUdpSocketError) -> DiscoveryError {
    DiscoveryError::Io {
        operation: "pin the routed discovery socket",
        endpoint: destination,
        source: io::Error::other(error),
    }
}

/// The production admission clock: one monotonic reading taken before the
/// wall-clock reading, so every derived deadline is conservative.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SystemRoutedClock;

impl RoutedAdmissionClock for SystemRoutedClock {
    fn sample(&self) -> Result<RoutedClockSample, RoutedClockReadError> {
        let monotonic = Instant::now();
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RoutedClockReadError)?;
        Ok(RoutedClockSample::new(
            RoutedPolicyTime::from_seconds(wall.as_secs()),
            wall.subsec_nanos(),
            monotonic,
        ))
    }
}

/// Topology-free reason one monitored operation did not complete.
#[derive(Debug, Error)]
pub(crate) enum MonitoredRoutedError {
    #[error("routed observation is not established")]
    NotObserving,
    #[error("the observer coordinator rejected the routed run: {0}")]
    Coordinator(ObserverCoordinatorError),
    #[error("the observer pair could not be established: {0}")]
    Observers(ObserverPairFailure),
    #[error("routed admission failed: {0}")]
    Admission(RoutedAdmissionError),
    #[error("approval storage failed: {0}")]
    Store(StoreError),
    #[error("no routed candidates could be selected: {0}")]
    Candidates(RouteCandidateError),
    #[error("the revalidated targets were rejected: {0}")]
    Targets(RoutedTargetsError),
}

/// What one monitored run did.
#[derive(Debug)]
pub(crate) enum MonitoredRoutedRun {
    /// The reservation was published, the scan ran, and completion settled.
    Completed(CompletedRoutedRun),
    /// No approval exists for this exact proposal; nothing was published.
    #[allow(
        dead_code,
        reason = "the runner result mirrors the exact store decision before the controller closes it to a category"
    )]
    NeedsApproval(RoutedProposalSummary),
    /// Automatic runs are cooling down; nothing was published.
    CoolingDown { remaining: Duration },
    /// Another reservation is active; nothing was published.
    Busy,
    /// The store published the reservation but could not confirm it, so no
    /// permit exists and the lease must expire on its own.
    #[allow(
        dead_code,
        reason = "the runner result mirrors the exact store decision before the controller closes it to a category"
    )]
    PublishedWithoutPermit { durability: CommitDurability },
}

/// The settled result of one scan.
#[derive(Debug)]
pub(crate) struct CompletedRoutedRun {
    /// The outcome the durable store was told.
    #[allow(
        dead_code,
        reason = "the controller consumes the report while this preserves the settled outcome"
    )]
    pub(crate) outcome: RoutedScanOutcome,
    /// The scan's own report or the transport-level reason it stopped.
    pub(crate) result: Result<DiscoveryReport, DiscoveryError>,
    /// How the store settled the reservation.
    #[allow(
        dead_code,
        reason = "the controller consumes the report while this preserves the settlement decision"
    )]
    pub(crate) completion: StoredCompletionDecision,
}

struct LivePair<Pair> {
    pair: Pair,
    epoch: HealthyRoutedEpoch,
    snapshot: RouteSnapshot,
}

/// The sole owner of monitored routed discovery for one approval store.
pub(crate) struct MonitoredRoutedDiscovery<F: ObserverPairFactory, P: RoutedTargetProber> {
    coordinator: Arc<RoutedObserverCoordinator>,
    store: Arc<ApprovalStore>,
    clock: Arc<dyn RoutedAdmissionClock>,
    pairs: Arc<F>,
    prober: Arc<P>,
    live: Option<LivePair<F::Pair>>,
    observation_error: Option<ObserverPairFailure>,
}

impl<F: ObserverPairFactory, P: RoutedTargetProber> MonitoredRoutedDiscovery<F, P> {
    /// Establish the first observer pair; the store is only read.
    pub(crate) async fn start(
        coordinator: Arc<RoutedObserverCoordinator>,
        store: Arc<ApprovalStore>,
        clock: Arc<dyn RoutedAdmissionClock>,
        pairs: Arc<F>,
        prober: Arc<P>,
    ) -> Result<Self, MonitoredRoutedError> {
        let live = Self::establish(&coordinator, &store, &pairs)
            .await
            .map_err(MonitoredRoutedError::Observers)?;
        Ok(Self {
            coordinator,
            store,
            clock,
            pairs,
            prober,
            live: Some(live),
            observation_error: None,
        })
    }

    /// Whether a healthy observer pair currently backs this runner.
    #[must_use]
    pub(crate) fn is_observing(&self) -> bool {
        self.live.is_some()
    }

    /// Why the last observer replacement failed, if it did.
    #[must_use]
    #[allow(
        dead_code,
        reason = "this private inspection API is retained beside route_snapshot"
    )]
    pub(crate) fn observation_error(&self) -> Option<ObserverPairFailure> {
        self.observation_error
    }

    /// The route snapshot the live pair baselined.
    #[must_use]
    #[allow(
        dead_code,
        reason = "this private inspection API is retained beside observation_error"
    )]
    pub(crate) fn route_snapshot(&self) -> Option<&RouteSnapshot> {
        self.live.as_ref().map(|live| &live.snapshot)
    }

    /// Build the exact proposal for the live snapshot's tunnel candidates.
    ///
    /// The store may create its key on first use, which is a publication, so
    /// the observer pair is rebaselined afterwards.
    pub(crate) async fn propose(
        &mut self,
        probe_config: ProbeConfig,
        scan_config: RoutedScanConfig,
    ) -> Result<StoredRoutedProposal, MonitoredRoutedError> {
        let result = self.propose_now(probe_config, scan_config);
        self.restore_observation().await;
        result
    }

    fn propose_now(
        &self,
        probe_config: ProbeConfig,
        scan_config: RoutedScanConfig,
    ) -> Result<StoredRoutedProposal, MonitoredRoutedError> {
        let live = self
            .live
            .as_ref()
            .ok_or(MonitoredRoutedError::NotObserving)?;
        let candidates = select_route_candidates(&live.snapshot, &[])
            .map_err(MonitoredRoutedError::Candidates)?;
        self.store
            .build_proposal(&live.snapshot, &candidates, probe_config, scan_config)
            .map_err(MonitoredRoutedError::Store)
    }

    /// Remember the user's approval of one exact proposal, then rebaseline.
    pub(crate) async fn approve(
        &mut self,
        proposal: &StoredRoutedProposal,
    ) -> Result<StoreCommit, MonitoredRoutedError> {
        let result = policy_now(&*self.clock).and_then(|now| {
            self.store
                .approve(proposal, now)
                .map_err(MonitoredRoutedError::Store)
        });
        self.restore_observation().await;
        result
    }

    /// Run one approved proposal to completion under monitored authority.
    ///
    /// Whatever the outcome, the observer pair is replaced afterwards so the
    /// runner never keeps a pair that observed its own publications.
    pub(crate) async fn run(
        &mut self,
        proposal: StoredRoutedProposal,
        trigger: RoutedScanTrigger,
        cancellation: &CancellationToken,
    ) -> Result<MonitoredRoutedRun, MonitoredRoutedError> {
        let result = self.run_now(proposal, trigger, cancellation).await;
        self.restore_observation().await;
        result
    }

    async fn run_now(
        &mut self,
        proposal: StoredRoutedProposal,
        trigger: RoutedScanTrigger,
        cancellation: &CancellationToken,
    ) -> Result<MonitoredRoutedRun, MonitoredRoutedError> {
        let live = self.live.take().ok_or(MonitoredRoutedError::NotObserving)?;

        // A registration in the current epoch guards the reserve write: a
        // route or store change before publication cancels it first.
        let guard = self
            .coordinator
            .register(&live.epoch)
            .map_err(MonitoredRoutedError::Coordinator)?;
        let first = sample_live(&*self.clock, &guard, cancellation)
            .map_err(MonitoredRoutedError::Admission)?;
        let reserve_now = first
            .conservative_policy_time()
            .map_err(MonitoredRoutedError::Admission)?;
        let expected_expiry_seconds = reserve_now
            .as_seconds()
            .checked_add(RESERVATION_LEASE.as_secs())
            .ok_or(MonitoredRoutedError::Admission(
                RoutedAdmissionError::ClockOverflow,
            ))?;
        let expected_expiry = RoutedPolicyTime::from_seconds(expected_expiry_seconds);
        let mut timeline = AdmissionTimeline::new(first, expected_expiry)
            .map_err(MonitoredRoutedError::Admission)?;
        ensure_live(&guard, cancellation).map_err(MonitoredRoutedError::Admission)?;

        let decision = self
            .store
            .reserve(proposal, trigger, reserve_now)
            .map_err(MonitoredRoutedError::Store)?;
        let published = match decision {
            StoredBeginDecision::Published(published) => published,
            StoredBeginDecision::NeedsApproval(summary) => {
                self.live = Some(live);
                return Ok(MonitoredRoutedRun::NeedsApproval(summary));
            }
            StoredBeginDecision::CoolingDown { remaining } => {
                self.live = Some(live);
                return Ok(MonitoredRoutedRun::CoolingDown { remaining });
            }
            StoredBeginDecision::Busy => {
                self.live = Some(live);
                return Ok(MonitoredRoutedRun::Busy);
            }
            StoredBeginDecision::PublishedWithoutPermit { durability } => {
                drop(guard);
                live.pair.shutdown().await;
                return Ok(MonitoredRoutedRun::PublishedWithoutPermit { durability });
            }
        };
        drop(guard);

        // From here the reservation is durable; the guard settles it on every
        // path that does not reach an explicit completion.
        let completion = CompletionGuard::new(
            Arc::clone(&self.store),
            Arc::clone(&self.clock),
            published.permit().fingerprint(),
            published.permit().run_id(),
        );
        if published.permit().expires_at() != expected_expiry {
            return Err(MonitoredRoutedError::Admission(
                RoutedAdmissionError::StoreTimeClamped,
            ));
        }

        // Replace and rebaseline the pair that observed the publication. The
        // exact reread and route revalidation run inside the new store
        // observer's subscribe/drain/read/drain sandwich.
        live.pair.shutdown().await;
        let clock = Arc::clone(&self.clock);
        let (fingerprint, run_id) = (completion.fingerprint, completion.run_id);
        let (snapshot, admission, epoch, pair) = self
            .pairs
            .prepare_and_activate(
                Arc::clone(&self.coordinator),
                Arc::clone(&self.store),
                move |store, snapshot| -> Result<_, Infallible> {
                    Ok(admit_published(
                        store,
                        snapshot,
                        published,
                        &*clock,
                        fingerprint,
                        run_id,
                    ))
                },
            )
            .await
            .map_err(MonitoredRoutedError::Observers)?;
        let registration = self.coordinator.register(&epoch);
        self.live = Some(LivePair {
            pair,
            epoch,
            snapshot,
        });
        let registration = registration.map_err(MonitoredRoutedError::Coordinator)?;
        let (scan, revalidation_sample) = admission.map_err(MonitoredRoutedError::Admission)?;

        // Anchor the scan budget at the revalidation sample, then take the
        // final pre-send clock boundary under the fresh registration.
        timeline
            .observe(revalidation_sample)
            .map_err(MonitoredRoutedError::Admission)?;
        let scan_deadline = revalidation_sample
            .monotonic_time()
            .checked_add(scan.scan_config().overall_deadline())
            .ok_or(MonitoredRoutedError::Admission(
                RoutedAdmissionError::ClockOverflow,
            ))?;
        let mut absolute_deadline = timeline.lease_deadline().min(scan_deadline);
        let final_sample = checkpoint(&*self.clock, &registration, cancellation, &mut timeline)
            .map_err(MonitoredRoutedError::Admission)?;
        absolute_deadline = absolute_deadline.min(timeline.lease_deadline());
        if final_sample.monotonic_time() >= absolute_deadline {
            return Err(MonitoredRoutedError::Admission(
                RoutedAdmissionError::ReservationExpired,
            ));
        }
        let request_cancellation = cancellation.clone();
        ensure_live(&registration, &request_cancellation)
            .map_err(MonitoredRoutedError::Admission)?;
        let admitted =
            AdmittedRoutedScan::new(scan, absolute_deadline, request_cancellation, registration);

        let (outcome, result) = execute(admitted, Arc::clone(&self.prober)).await?;
        let completion = completion.settle(outcome)?;
        Ok(MonitoredRoutedRun::Completed(CompletedRoutedRun {
            outcome,
            result,
            completion,
        }))
    }

    /// Forget every remembered approval, then rebaseline.
    pub(crate) async fn revoke_all(&mut self) -> Result<(), MonitoredRoutedError> {
        let result = self
            .store
            .revoke_all()
            .map(|_| ())
            .map_err(MonitoredRoutedError::Store);
        self.restore_observation().await;
        result
    }

    /// Wait until either observed source requires replacement, then
    /// rebaseline. Returns whether observation is healthy afterwards.
    pub(crate) async fn await_replacement(&mut self) -> bool {
        let Some(live) = self.live.as_mut() else {
            return false;
        };
        let ObserverPairEvent::ReplacementRequired = live.pair.next_event().await;
        self.restore_observation().await;
        self.is_observing()
    }

    /// Retire the live pair and await both actors.
    pub(crate) async fn shutdown(mut self) {
        if let Some(live) = self.live.take() {
            live.pair.shutdown().await;
        }
    }

    async fn restore_observation(&mut self) {
        if let Some(live) = self.live.take() {
            live.pair.shutdown().await;
        }
        match Self::establish(&self.coordinator, &self.store, &self.pairs).await {
            Ok(live) => {
                self.live = Some(live);
                self.observation_error = None;
            }
            Err(error) => self.observation_error = Some(error),
        }
    }

    async fn establish(
        coordinator: &Arc<RoutedObserverCoordinator>,
        store: &Arc<ApprovalStore>,
        pairs: &F,
    ) -> Result<LivePair<F::Pair>, ObserverPairFailure> {
        let (snapshot, _status, epoch, pair) = pairs
            .prepare_and_activate(Arc::clone(coordinator), Arc::clone(store), |store, _| {
                store.load()
            })
            .await?;
        Ok(LivePair {
            pair,
            epoch,
            snapshot,
        })
    }
}

impl<F: ObserverPairFactory, P: RoutedTargetProber> fmt::Debug for MonitoredRoutedDiscovery<F, P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MonitoredRoutedDiscovery")
            .field("observing", &self.is_observing())
            .finish_non_exhaustive()
    }
}

/// The exact post-publication reread: match the published reservation, take
/// one clock sample, and revalidate the permit against the fresh snapshot.
fn admit_published<C: RoutedAdmissionClock + ?Sized>(
    store: &ApprovalStore,
    snapshot: &RouteSnapshot,
    published: StoredPublishedReservation,
    clock: &C,
    fingerprint: RouteFingerprint,
    run_id: RoutedRunId,
) -> Result<(RevalidatedRoutedScan, RoutedClockSample), RoutedAdmissionError> {
    let reservation_expires_at = published.permit().expires_at();
    let permit = store
        .match_published_reservation(published)
        .map_err(RoutedAdmissionError::Revalidation)?;
    let sample = clock
        .sample()
        .map_err(|_| RoutedAdmissionError::ClockUnavailable)?;
    let revalidation_now = sample.conservative_policy_time()?;
    let scan = store
        .revalidate_permit(permit, snapshot, revalidation_now)
        .map_err(RoutedAdmissionError::Revalidation)?;
    if scan.validated_at() != revalidation_now {
        return Err(RoutedAdmissionError::StoreTimeClamped);
    }
    if scan.fingerprint() != fingerprint
        || scan.run_id() != run_id
        || scan.reservation_expires_at() != reservation_expires_at
    {
        return Err(RoutedAdmissionError::AuthorityChanged);
    }
    Ok((scan, sample))
}

/// Probe every admitted target, stopping at the first lost authority.
async fn execute<P: RoutedTargetProber>(
    admitted: AdmittedRoutedScan<RoutedAuthorityRegistration>,
    prober: Arc<P>,
) -> Result<(RoutedScanOutcome, Result<DiscoveryReport, DiscoveryError>), MonitoredRoutedError> {
    let scan = admitted.scan();
    let targets =
        ApprovedIpv4Targets::new(scan.targets().iter().map(RevalidatedRoutedTarget::address))
            .map_err(MonitoredRoutedError::Targets)?;
    let by_address = Arc::new(
        scan.targets()
            .iter()
            .map(|target| {
                (
                    target.address(),
                    RoutedProbeTarget::from_revalidated(target),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    );
    let scan_config = scan.scan_config();
    let attempts = scan.probe_config().attempts();
    let absolute_deadline = admitted.absolute_deadline();
    let admitted = Arc::new(admitted);

    // One run token folds the request and invalidation signals so the paced
    // loop and every in-flight probe stop on either. The guard cancels it on
    // every exit from this function, including a dropped future, so the
    // watcher never outlives the scan it serves.
    let run_token = CancellationToken::new();
    let run_guard = run_token.clone().drop_guard();
    let watcher = tokio::spawn({
        let request = admitted.request_cancellation().clone();
        let invalidation = admitted.invalidation_cancellation().clone();
        let observed = run_token.clone();
        async move {
            tokio::select! {
                () = request.cancelled() => observed.cancel(),
                () = invalidation.cancelled() => observed.cancel(),
                () = observed.cancelled() => {}
            }
        }
    });
    let authority: PreSendAuthority = {
        let admitted = Arc::clone(&admitted);
        Arc::new(move || !admitted.is_cancelled() && Instant::now() < admitted.absolute_deadline())
    };

    let probe = {
        let by_address = Arc::clone(&by_address);
        let authority = Arc::clone(&authority);
        move |candidate: Ipv4Addr, task_cancellation: CancellationToken| {
            let target = by_address.get(&candidate).cloned();
            let prober = Arc::clone(&prober);
            let authority = Arc::clone(&authority);
            async move {
                let Some(target) = target else {
                    return Err(DiscoveryError::Cancelled);
                };
                if !authority() {
                    return Err(DiscoveryError::Cancelled);
                }
                prober.probe(target, authority, task_cancellation).await
            }
        }
    };
    let result = scan_approved_targets_until(
        &targets,
        scan_config,
        attempts,
        &run_token,
        tokio::time::Instant::from_std(absolute_deadline),
        Arc::new(probe),
    )
    .await;
    drop(run_guard);
    let _joined = watcher.await;
    let outcome = classify(&result);
    Ok((outcome, result))
}

fn classify(result: &Result<DiscoveryReport, DiscoveryError>) -> RoutedScanOutcome {
    match result {
        Ok(report) if !report.observations.is_empty() => RoutedScanOutcome::Found,
        Ok(report) if report.issues.is_empty() => RoutedScanOutcome::CompleteEmpty,
        Ok(_) | Err(_) => RoutedScanOutcome::Indeterminate,
    }
}

fn policy_now<C: RoutedAdmissionClock + ?Sized>(
    clock: &C,
) -> Result<RoutedPolicyTime, MonitoredRoutedError> {
    let sample = clock
        .sample()
        .map_err(|_| MonitoredRoutedError::Admission(RoutedAdmissionError::ClockUnavailable))?;
    sample
        .conservative_policy_time()
        .map_err(MonitoredRoutedError::Admission)
}

/// Owns exact completion of one published reservation. Dropping it without
/// settling records an indeterminate outcome, so a panic or early return can
/// never leave the durable reservation to expire on its own.
struct CompletionGuard {
    store: Arc<ApprovalStore>,
    clock: Arc<dyn RoutedAdmissionClock>,
    fingerprint: RouteFingerprint,
    run_id: RoutedRunId,
    settled: bool,
}

impl CompletionGuard {
    const fn new(
        store: Arc<ApprovalStore>,
        clock: Arc<dyn RoutedAdmissionClock>,
        fingerprint: RouteFingerprint,
        run_id: RoutedRunId,
    ) -> Self {
        Self {
            store,
            clock,
            fingerprint,
            run_id,
            settled: false,
        }
    }

    fn settle(
        mut self,
        outcome: RoutedScanOutcome,
    ) -> Result<StoredCompletionDecision, MonitoredRoutedError> {
        self.settled = true;
        let now = policy_now(&*self.clock)?;
        self.store
            .complete(self.fingerprint, self.run_id, outcome, now)
            .map_err(MonitoredRoutedError::Store)
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Ok(now) = policy_now(&*self.clock) {
            let _ = self.store.complete(
                self.fingerprint,
                self.run_id,
                RoutedScanOutcome::Indeterminate,
                now,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ipnet::IpNet;
    use tokio::sync::Notify;

    use super::super::store::LinuxStoreObserverBridgeError;
    use super::super::{RouteObserverSink, RoutedObserverIncarnation, StoreObserverSink};
    use super::*;
    use crate::discovery::approval::store::{ApprovalStoreStatus, StorePaths};
    use crate::discovery::client::DiscoveryObservation;
    use crate::discovery::routes::{
        InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteScope,
    };
    use crate::discovery::types::DiscoveryMethod;
    use crate::domain::DeviceId;

    fn ipnet(value: &str) -> IpNet {
        value.parse().expect("valid synthetic test network")
    }

    fn tunnel_snapshot() -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![NetworkInterface::new(
                InterfaceId::new(7),
                "synthetic-runner-tunnel",
                InterfaceKind::Tunnel,
                true,
                [ipnet("10.250.0.2/32")],
            )],
            vec![NetworkRoute::effective(
                ipnet("172.31.90.8/30"),
                Some(InterfaceId::new(7)),
                RouteKind::Unicast,
                RouteScope::OnLink,
            )],
        )
    }

    /// Mints genuine coordinator epochs for a fixed snapshot without the
    /// kernel observers, counting every preparation and shutdown.
    struct FakeObservers {
        snapshot: RouteSnapshot,
        prepared: AtomicUsize,
        shutdowns: AtomicUsize,
        fail_prepare: Mutex<Option<usize>>,
        replacement: Notify,
    }

    struct FakePairFactory(Arc<FakeObservers>);

    struct FakePair {
        observers: Arc<FakeObservers>,
        _incarnation: RoutedObserverIncarnation,
        _route_sink: RouteObserverSink,
        _store_sink: StoreObserverSink,
    }

    fn coordinator_failure(error: ObserverCoordinatorError) -> ObserverPairFailure {
        ObserverPairFailure(LinuxObserverPairError::Coordinator(error))
    }

    impl ObserverPairFactory for FakePairFactory {
        type Pair = FakePair;

        async fn prepare_and_activate<R, E, Read>(
            &self,
            coordinator: Arc<RoutedObserverCoordinator>,
            store: Arc<ApprovalStore>,
            exact_reread: Read,
        ) -> Result<(RouteSnapshot, R, HealthyRoutedEpoch, Self::Pair), ObserverPairFailure>
        where
            R: Send + 'static,
            E: Send + 'static,
            Read: FnOnce(&ApprovalStore, &RouteSnapshot) -> Result<R, E> + Send + 'static,
        {
            let sequence = self.0.prepared.fetch_add(1, Ordering::SeqCst) + 1;
            if *self.0.fail_prepare.lock().expect("fake lock") == Some(sequence) {
                return Err(coordinator_failure(ObserverCoordinatorError::Unhealthy));
            }
            let mut incarnation = coordinator
                .start_incarnation()
                .map_err(coordinator_failure)?;
            let route_sink = incarnation.take_route_sink().map_err(coordinator_failure)?;
            let store_sink = incarnation.take_store_sink().map_err(coordinator_failure)?;
            let route_token = incarnation
                .begin_route_baseline()
                .map_err(coordinator_failure)?;
            let store_token = incarnation
                .begin_store_baseline()
                .map_err(coordinator_failure)?;
            let snapshot = self.0.snapshot.clone();
            let value = exact_reread(&store, &snapshot).map_err(|_| {
                ObserverPairFailure(LinuxObserverPairError::Store(
                    LinuxStoreObserverBridgeError::ExactRereadRejected,
                ))
            })?;
            let epoch = incarnation
                .activate(route_token, store_token)
                .map_err(coordinator_failure)?;
            Ok((
                snapshot,
                value,
                epoch,
                FakePair {
                    observers: Arc::clone(&self.0),
                    _incarnation: incarnation,
                    _route_sink: route_sink,
                    _store_sink: store_sink,
                },
            ))
        }
    }

    impl ObserverPair for FakePair {
        async fn next_event(&mut self) -> ObserverPairEvent {
            self.observers.replacement.notified().await;
            ObserverPairEvent::ReplacementRequired
        }

        async fn shutdown(self) {
            self.observers.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
    }

    type ProbeHook = Box<dyn FnOnce() + Send>;

    /// Records every probe with the authority it observed and answers with a
    /// canned report.
    struct FakeProber {
        calls: Mutex<Vec<(Ipv4Addr, bool)>>,
        found: Option<Ipv4Addr>,
        block_until_cancelled: bool,
        on_first_probe: Mutex<Option<ProbeHook>>,
    }

    impl FakeProber {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                found: None,
                block_until_cancelled: false,
                on_first_probe: Mutex::new(None),
            }
        }

        fn calls(&self) -> Vec<(Ipv4Addr, bool)> {
            self.calls.lock().expect("fake prober lock").clone()
        }
    }

    impl RoutedTargetProber for FakeProber {
        async fn probe(
            &self,
            target: RoutedProbeTarget,
            authority: PreSendAuthority,
            cancellation: CancellationToken,
        ) -> Result<DiscoveryReport, DiscoveryError> {
            self.calls
                .lock()
                .expect("fake prober lock")
                .push((target.address(), authority()));
            let hook = self.on_first_probe.lock().expect("fake prober lock").take();
            if let Some(hook) = hook {
                hook();
            }
            if self.block_until_cancelled {
                cancellation.cancelled().await;
                return Err(DiscoveryError::Cancelled);
            }
            let mut report = DiscoveryReport::default();
            report.stats.probes_started = 1;
            report.stats.datagrams_sent = 1;
            if self.found == Some(target.address()) {
                report.stats.datagrams_received = 1;
                report.stats.datagrams_accepted = 1;
                report.observations.push(DiscoveryObservation {
                    device_id: DeviceId::new(0x105A_1232).expect("valid test id"),
                    source: SocketAddr::V4(SocketAddrV4::new(target.address(), DISCOVERY_UDP_PORT)),
                    method: DiscoveryMethod::RoutedTargeted,
                    interface: None,
                    device_types: vec![1],
                    tuner_count: Some(2),
                    advertised_base_url: None,
                    advertised_lineup_url: None,
                });
            }
            Ok(report)
        }
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        coordinator: Arc<RoutedObserverCoordinator>,
        store: Arc<ApprovalStore>,
        observers: Arc<FakeObservers>,
        prober: Arc<FakeProber>,
    }

    type Runner = MonitoredRoutedDiscovery<FakePairFactory, FakeProber>;

    async fn start(prober: FakeProber, fail_prepare: Option<usize>) -> (Fixture, Runner) {
        let temporary = tempfile::tempdir().expect("private store parent");
        let store = Arc::new(ApprovalStore::new(StorePaths::new(
            temporary.path().join("private"),
        )));
        let coordinator = Arc::new(RoutedObserverCoordinator::new());
        let observers = Arc::new(FakeObservers {
            snapshot: tunnel_snapshot(),
            prepared: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            fail_prepare: Mutex::new(fail_prepare),
            replacement: Notify::new(),
        });
        let prober = Arc::new(prober);
        let runner = MonitoredRoutedDiscovery::start(
            Arc::clone(&coordinator),
            Arc::clone(&store),
            Arc::new(SystemRoutedClock),
            Arc::new(FakePairFactory(Arc::clone(&observers))),
            Arc::clone(&prober),
        )
        .await
        .expect("establish the first observer pair");
        (
            Fixture {
                _temporary: temporary,
                coordinator,
                store,
                observers,
                prober,
            },
            runner,
        )
    }

    fn assert_no_active_reservation(store: &ApprovalStore) {
        match store.load().expect("load the store") {
            ApprovalStoreStatus::Ready {
                has_active_reservation,
                ..
            } => assert!(!has_active_reservation),
            ApprovalStoreStatus::Missing { .. } => {}
            status => panic!("unexpected store status {status:?}"),
        }
    }

    #[tokio::test]
    async fn unapproved_proposal_reserves_nothing_and_needs_approval() {
        let (fixture, mut runner) = start(FakeProber::new(), None).await;
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose the tunnel candidates");
        assert!(proposal.summary().candidate_count() > 0);

        let run = runner
            .run(
                proposal,
                RoutedScanTrigger::Automatic,
                &CancellationToken::new(),
            )
            .await
            .expect("an unapproved proposal is a decision, not an error");
        assert!(
            matches!(run, MonitoredRoutedRun::NeedsApproval(_)),
            "{run:?}"
        );
        assert!(fixture.prober.calls().is_empty());
        assert_no_active_reservation(&fixture.store);
        assert!(runner.is_observing());
        assert_eq!(fixture.observers.prepared.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.observers.shutdowns.load(Ordering::SeqCst), 2);
        runner.shutdown().await;
        assert_eq!(fixture.observers.shutdowns.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn approved_run_probes_every_target_under_fresh_authority_and_settles() {
        let mut prober = FakeProber::new();
        prober.found = Some("172.31.90.9".parse().expect("target"));
        let (fixture, mut runner) = start(prober, None).await;
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose");
        let candidate_count = proposal.summary().candidate_count();
        let commit = runner.approve(&proposal).await.expect("approve");
        assert!(commit.is_confirmed());

        let run = runner
            .run(
                proposal,
                RoutedScanTrigger::Automatic,
                &CancellationToken::new(),
            )
            .await
            .expect("run the approved proposal");
        let MonitoredRoutedRun::Completed(completed) = run else {
            panic!("expected a completed run, got {run:?}");
        };
        assert_eq!(completed.outcome, RoutedScanOutcome::Found);
        assert!(matches!(
            completed.completion,
            StoredCompletionDecision::Confirmed(_)
        ));
        let report = completed.result.expect("the scan finished");
        assert_eq!(report.observations.len(), 1);
        assert_eq!(
            report.observations[0].method,
            DiscoveryMethod::RoutedTargeted
        );

        let calls = fixture.prober.calls();
        assert_eq!(calls.len(), candidate_count);
        assert!(calls.iter().all(|(_, live)| *live), "{calls:?}");
        assert_no_active_reservation(&fixture.store);
        assert!(runner.is_observing());
        // start, propose, approve, the post-publication replacement, and the
        // post-completion rebaseline.
        assert_eq!(fixture.observers.prepared.load(Ordering::SeqCst), 5);
        assert_eq!(fixture.observers.shutdowns.load(Ordering::SeqCst), 4);

        // Automatic runs now cool down; an explicit refresh may run again.
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose again");
        let cooling = runner
            .run(
                proposal,
                RoutedScanTrigger::Automatic,
                &CancellationToken::new(),
            )
            .await
            .expect("cooldown is a decision");
        assert!(
            matches!(cooling, MonitoredRoutedRun::CoolingDown { .. }),
            "{cooling:?}"
        );
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose once more");
        let refreshed = runner
            .run(
                proposal,
                RoutedScanTrigger::ExplicitRefresh,
                &CancellationToken::new(),
            )
            .await
            .expect("explicit refresh runs");
        assert!(
            matches!(refreshed, MonitoredRoutedRun::Completed(_)),
            "{refreshed:?}"
        );
        assert_eq!(fixture.prober.calls().len(), candidate_count * 2);
        runner.shutdown().await;
    }

    #[tokio::test]
    async fn cancellation_during_the_scan_settles_indeterminate() {
        let mut prober = FakeProber::new();
        prober.block_until_cancelled = true;
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        prober.on_first_probe = Mutex::new(Some(Box::new(move || cancel.cancel())));
        let (fixture, mut runner) = start(prober, None).await;
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose");
        runner.approve(&proposal).await.expect("approve");

        let run = runner
            .run(proposal, RoutedScanTrigger::Automatic, &cancellation)
            .await
            .expect("a cancelled scan still completes its reservation");
        let MonitoredRoutedRun::Completed(completed) = run else {
            panic!("expected a completed run, got {run:?}");
        };
        assert_eq!(completed.outcome, RoutedScanOutcome::Indeterminate);
        assert!(matches!(completed.result, Err(DiscoveryError::Cancelled)));
        assert!(matches!(
            completed.completion,
            StoredCompletionDecision::Confirmed(_)
        ));
        assert_no_active_reservation(&fixture.store);
        assert!(runner.is_observing());
        runner.shutdown().await;
    }

    #[tokio::test]
    async fn invalidation_during_the_scan_refuses_every_later_send() {
        let mut prober = FakeProber::new();
        let (fixture, mut runner) = {
            // The hook needs the coordinator, which the fixture creates, so
            // install it after start.
            let (fixture, runner) = start(FakeProber::new(), None).await;
            drop(runner);
            drop(fixture);
            let coordinator_slot: Arc<Mutex<Option<Arc<RoutedObserverCoordinator>>>> =
                Arc::new(Mutex::new(None));
            let slot = Arc::clone(&coordinator_slot);
            prober.on_first_probe = Mutex::new(Some(Box::new(move || {
                if let Some(coordinator) = slot.lock().expect("slot lock").as_ref() {
                    coordinator.invalidate();
                }
            })));
            let (fixture, runner) = start(prober, None).await;
            *coordinator_slot.lock().expect("slot lock") = Some(Arc::clone(&fixture.coordinator));
            (fixture, runner)
        };
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose");
        assert!(proposal.summary().candidate_count() > 1);
        runner.approve(&proposal).await.expect("approve");

        let run = runner
            .run(
                proposal,
                RoutedScanTrigger::Automatic,
                &CancellationToken::new(),
            )
            .await
            .expect("an invalidated scan still completes its reservation");
        let MonitoredRoutedRun::Completed(completed) = run else {
            panic!("expected a completed run, got {run:?}");
        };
        assert_eq!(completed.outcome, RoutedScanOutcome::Indeterminate);
        assert!(matches!(completed.result, Err(DiscoveryError::Cancelled)));
        let calls = fixture.prober.calls();
        assert_eq!(
            calls.len(),
            1,
            "no target may be probed after invalidation: {calls:?}"
        );
        assert!(calls[0].1, "the first probe still held authority");
        assert_no_active_reservation(&fixture.store);
        assert!(runner.is_observing());
        runner.shutdown().await;
    }

    #[tokio::test]
    async fn observer_replacement_failure_after_publication_settles_and_recovers() {
        // The fourth preparation is the post-publication replacement.
        let (fixture, mut runner) = start(FakeProber::new(), Some(4)).await;
        let proposal = runner
            .propose(ProbeConfig::default(), RoutedScanConfig::default())
            .await
            .expect("propose");
        runner.approve(&proposal).await.expect("approve");

        let error = runner
            .run(
                proposal,
                RoutedScanTrigger::Automatic,
                &CancellationToken::new(),
            )
            .await
            .expect_err("a failed replacement cannot admit the scan");
        assert!(
            matches!(error, MonitoredRoutedError::Observers(_)),
            "{error}"
        );
        assert!(fixture.prober.calls().is_empty());
        assert_no_active_reservation(&fixture.store);
        assert!(runner.is_observing(), "{:?}", runner.observation_error());
        assert_eq!(fixture.observers.prepared.load(Ordering::SeqCst), 5);
        runner.shutdown().await;
    }
}
