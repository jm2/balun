//! Packet-free admission for route-derived discovery runs.
//!
//! This boundary registers invalidation before durable reservation, maps the
//! coarse persisted lease onto one absolute monotonic deadline, obtains a
//! fresh route snapshot, and consumes authority through the store-owned gate.
//! It deliberately opens no socket and remains unwired from production entry
//! points.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::gate::RevalidatedRoutedScan;
use super::store::{
    ApprovalStore, CommitDurability, StoreError, StoredBeginDecision, StoredRevalidationError,
    StoredRevokeDecision, StoredRoutedProposal,
};
use super::{RESERVATION_LEASE, RoutedPolicyTime, RoutedProposalSummary, RoutedScanTrigger};
use crate::discovery::RouteProvider;

/// A paired wall-policy and monotonic clock observation.
///
/// `policy_subsecond_nanos` preserves the precision which the persisted
/// whole-second clock deliberately omits. Admission rounds that policy clock
/// upward before every store call and never rounds the monotonic deadline
/// upward.
#[derive(Clone, Copy)]
pub(crate) struct RoutedClockSample {
    policy_time: RoutedPolicyTime,
    policy_subsecond_nanos: u32,
    monotonic_time: Instant,
}

impl RoutedClockSample {
    pub(crate) const fn new(
        policy_time: RoutedPolicyTime,
        policy_subsecond_nanos: u32,
        monotonic_time: Instant,
    ) -> Self {
        Self {
            policy_time,
            policy_subsecond_nanos,
            monotonic_time,
        }
    }

    fn exact_policy_time(self) -> Result<Duration, RoutedAdmissionError> {
        if self.policy_subsecond_nanos >= 1_000_000_000 {
            return Err(RoutedAdmissionError::InvalidClockSample);
        }
        Ok(Duration::new(
            self.policy_time.as_seconds(),
            self.policy_subsecond_nanos,
        ))
    }

    fn conservative_policy_time(self) -> Result<RoutedPolicyTime, RoutedAdmissionError> {
        if self.policy_subsecond_nanos >= 1_000_000_000 {
            return Err(RoutedAdmissionError::InvalidClockSample);
        }
        let seconds = self
            .policy_time
            .as_seconds()
            .checked_add(u64::from(self.policy_subsecond_nanos != 0))
            .ok_or(RoutedAdmissionError::ClockOverflow)?;
        Ok(RoutedPolicyTime::from_seconds(seconds))
    }
}

impl fmt::Debug for RoutedClockSample {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedClockSample")
            .field("policy_time", &"<redacted>")
            .field("monotonic_time", &"<redacted>")
            .finish()
    }
}

/// Opaque failure to obtain one internally paired clock sample.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the admission clock could not provide a paired sample")]
pub(crate) struct RoutedClockReadError;

/// Supplies clock readings from one stable wall-policy and monotonic epoch.
///
/// A production implementation must obtain the two values as one conservative
/// pair. It is intentionally not provided until the controller owns that
/// clock lifecycle.
pub(crate) trait RoutedAdmissionClock: Send + Sync {
    fn sample(&self) -> Result<RoutedClockSample, RoutedClockReadError>;
}

/// Proof that a route observer installed its subscription and baseline for
/// one exact invalidation generation.
///
/// The token contains no topology and is harmless to retain: registration
/// rejects it as soon as its observer incarnation, generation, or healthy
/// epoch changes.
pub(crate) struct HealthyRouteEpoch {
    source: Weak<InvalidationInner>,
    identity: HealthyEpochIdentity,
}

impl PartialEq for HealthyRouteEpoch {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity && self.source.ptr_eq(&other.source)
    }
}

impl Eq for HealthyRouteEpoch {}

#[derive(Clone, Copy, Eq, PartialEq)]
struct HealthyEpochIdentity {
    observer_incarnation: u64,
    generation: u64,
    healthy_epoch: u64,
}

impl fmt::Debug for HealthyRouteEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HealthyRouteEpoch(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum InvalidationHubError {
    #[error("route invalidation is not in a healthy observed epoch")]
    Unhealthy,
    #[error("the route observer session is stale")]
    StaleObserver,
    #[error("the supplied healthy route epoch is stale")]
    StaleEpoch,
    #[error("route invalidation state failed closed")]
    FailedClosed,
}

struct RegisteredInvalidation {
    epoch: HealthyEpochIdentity,
    signal: CancellationToken,
}

struct InvalidationState {
    generation: u64,
    next_observer_incarnation: u64,
    current_observer_incarnation: Option<u64>,
    next_healthy_epoch: u64,
    current_healthy_epoch: Option<HealthyEpochIdentity>,
    next_registration: u64,
    registrations: BTreeMap<u64, RegisteredInvalidation>,
    failed_closed: bool,
}

impl Default for InvalidationState {
    fn default() -> Self {
        Self {
            generation: 0,
            next_observer_incarnation: 0,
            current_observer_incarnation: None,
            next_healthy_epoch: 0,
            current_healthy_epoch: None,
            next_registration: 1,
            registrations: BTreeMap::new(),
            failed_closed: false,
        }
    }
}

struct InvalidationInner {
    state: Mutex<InvalidationState>,
}

impl InvalidationInner {
    fn lock(&self) -> Result<MutexGuard<'_, InvalidationState>, InvalidationHubError> {
        match self.state.lock() {
            Ok(state) if state.failed_closed => Err(InvalidationHubError::FailedClosed),
            Ok(state) => Ok(state),
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                Err(InvalidationHubError::FailedClosed)
            }
        }
    }

    fn invalidate(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        invalidate_state(&mut state);
    }
}

impl Drop for InvalidationInner {
    fn drop(&mut self) {
        let state = match self.state.get_mut() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        fail_closed(state);
    }
}

/// Shared invalidation authority for admitted routed scans.
///
/// The hub deliberately does not implement `Clone`; owners which must share
/// it do so explicitly with `Arc`. It starts without an observer and
/// unhealthy. Only the current non-cloneable [`RouteObserverSession`] can
/// install a post-subscription/post-baseline healthy epoch. An ordinary
/// invalidation cancels every registration and leaves the session alive but
/// unhealthy until that observer establishes another coherent baseline.
///
/// One hub represents exactly one observed invalidation source. A future
/// cross-process approval-store observer must own a separate hub/healthy epoch
/// and a coordinator must require both it and the route source to be healthy;
/// a route monitor cannot attest store observation. That store observer must
/// reread and match Balun's own reserve writes rather than blindly treating
/// every file notification as an external revocation.
///
/// Starting a replacement observer invalidates all outstanding registrations;
/// the superseded incarnation can neither restore health nor mint an epoch,
/// and dropping it cannot disturb its replacement. Dropping the current
/// session invalidates the hub before relinquishing observer ownership.
pub(crate) struct RoutedInvalidationHub {
    inner: Arc<InvalidationInner>,
}

impl RoutedInvalidationHub {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(InvalidationInner {
                state: Mutex::new(InvalidationState::default()),
            }),
        }
    }

    /// Begin a new, initially unhealthy observer incarnation.
    ///
    /// The controller must retain the returned session for exactly as long as
    /// its event subscription remains live. Replacing a session invalidates
    /// all authority from the previous observer before returning.
    pub(crate) fn start_observer_session(
        &self,
    ) -> Result<RouteObserverSession, InvalidationHubError> {
        let mut state = self.inner.lock()?;
        let incarnation = state
            .next_observer_incarnation
            .checked_add(1)
            .ok_or_else(|| {
                fail_closed(&mut state);
                InvalidationHubError::FailedClosed
            })?;
        invalidate_state(&mut state);
        if state.failed_closed {
            return Err(InvalidationHubError::FailedClosed);
        }
        state.next_observer_incarnation = incarnation;
        state.current_observer_incarnation = Some(incarnation);
        Ok(RouteObserverSession {
            hub: Arc::downgrade(&self.inner),
            incarnation,
        })
    }

    /// Cancel all registrations and require a new explicit healthy baseline.
    pub(crate) fn invalidate(&self) {
        self.inner.invalidate();
    }

    fn register(
        &self,
        epoch: &HealthyRouteEpoch,
    ) -> Result<RoutedInvalidationRegistration, InvalidationHubError> {
        let mut state = self.inner.lock()?;
        if state.current_healthy_epoch.is_none() {
            return Err(InvalidationHubError::Unhealthy);
        }
        if !epoch.source.ptr_eq(&Arc::downgrade(&self.inner)) {
            return Err(InvalidationHubError::StaleEpoch);
        }
        match state.current_healthy_epoch {
            None => unreachable!("checked above"),
            Some(current) if current != epoch.identity => {
                return Err(InvalidationHubError::StaleEpoch);
            }
            Some(_) => {}
        }

        let registration_id = state.next_registration;
        state.next_registration = state.next_registration.checked_add(1).ok_or_else(|| {
            fail_closed(&mut state);
            InvalidationHubError::FailedClosed
        })?;
        let signal = CancellationToken::new();
        state.registrations.insert(
            registration_id,
            RegisteredInvalidation {
                epoch: epoch.identity,
                signal: signal.clone(),
            },
        );
        Ok(RoutedInvalidationRegistration {
            hub: Arc::downgrade(&self.inner),
            registration_id,
            epoch: epoch.identity,
            signal,
        })
    }
}

impl fmt::Debug for RoutedInvalidationHub {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedInvalidationHub(<redacted>)")
    }
}

/// Exclusive lifetime proof for one route-observer incarnation.
///
/// This value is deliberately non-cloneable. It must be created before the
/// observer subscribes, retained while the subscription is live, and used to
/// establish health only after a coherent baseline and notification barrier.
/// Dropping the current session invalidates all registrations. A superseded
/// session is inert and cannot affect its replacement.
pub(crate) struct RouteObserverSession {
    hub: Weak<InvalidationInner>,
    incarnation: u64,
}

impl RouteObserverSession {
    /// Install the next explicitly observed healthy epoch.
    ///
    /// Calling this again without an intervening invalidation returns the
    /// existing proof; it does not rotate authority beneath live scans.
    pub(crate) fn establish_healthy_epoch(
        &self,
    ) -> Result<HealthyRouteEpoch, InvalidationHubError> {
        let hub = self
            .hub
            .upgrade()
            .ok_or(InvalidationHubError::FailedClosed)?;
        let mut state = hub.lock()?;
        if state.current_observer_incarnation != Some(self.incarnation) {
            return Err(InvalidationHubError::StaleObserver);
        }

        let identity = if let Some(epoch) = state.current_healthy_epoch {
            if epoch.observer_incarnation != self.incarnation {
                fail_closed(&mut state);
                return Err(InvalidationHubError::FailedClosed);
            }
            epoch
        } else {
            let healthy_epoch = state.next_healthy_epoch.checked_add(1).ok_or_else(|| {
                fail_closed(&mut state);
                InvalidationHubError::FailedClosed
            })?;
            state.next_healthy_epoch = healthy_epoch;
            let identity = HealthyEpochIdentity {
                observer_incarnation: self.incarnation,
                generation: state.generation,
                healthy_epoch,
            };
            state.current_healthy_epoch = Some(identity);
            identity
        };
        Ok(HealthyRouteEpoch {
            source: Arc::downgrade(&hub),
            identity,
        })
    }

    /// Invalidate this observer's current baseline after any event or fault.
    pub(crate) fn invalidate(&self) -> Result<(), InvalidationHubError> {
        let hub = self
            .hub
            .upgrade()
            .ok_or(InvalidationHubError::FailedClosed)?;
        let mut state = hub.lock()?;
        if state.current_observer_incarnation != Some(self.incarnation) {
            return Err(InvalidationHubError::StaleObserver);
        }
        invalidate_state(&mut state);
        if state.failed_closed {
            return Err(InvalidationHubError::FailedClosed);
        }
        Ok(())
    }
}

impl Drop for RouteObserverSession {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let mut state = match hub.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                return;
            }
        };
        if state.current_observer_incarnation == Some(self.incarnation) {
            state.current_observer_incarnation = None;
            invalidate_state(&mut state);
        }
    }
}

impl fmt::Debug for RouteObserverSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteObserverSession(<redacted>)")
    }
}

fn invalidate_state(state: &mut InvalidationState) {
    state.current_healthy_epoch = None;
    for registration in state.registrations.values() {
        registration.signal.cancel();
    }
    state.registrations.clear();
    match state.generation.checked_add(1) {
        Some(generation) => state.generation = generation,
        None => fail_closed(state),
    }
}

fn fail_closed(state: &mut InvalidationState) {
    state.failed_closed = true;
    state.current_observer_incarnation = None;
    state.current_healthy_epoch = None;
    for registration in state.registrations.values() {
        registration.signal.cancel();
    }
    state.registrations.clear();
}

/// Non-cloneable membership in one exact healthy invalidation epoch.
pub(crate) struct RoutedInvalidationRegistration {
    hub: Weak<InvalidationInner>,
    registration_id: u64,
    epoch: HealthyEpochIdentity,
    signal: CancellationToken,
}

impl RoutedInvalidationRegistration {
    fn is_current(&self) -> bool {
        if self.signal.is_cancelled() {
            return false;
        }
        let Some(hub) = self.hub.upgrade() else {
            return false;
        };
        let Ok(state) = hub.lock() else {
            self.signal.cancel();
            return false;
        };
        state.current_healthy_epoch == Some(self.epoch)
            && state
                .registrations
                .get(&self.registration_id)
                .is_some_and(|registration| registration.epoch == self.epoch)
            && !self.signal.is_cancelled()
    }

    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.signal
    }
}

impl Drop for RoutedInvalidationRegistration {
    fn drop(&mut self) {
        // Any child clone retained by a future runner also becomes cancelled
        // when the authority-owning registration is dropped.
        self.signal.cancel();
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let mut state = match hub.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                return;
            }
        };
        if state
            .registrations
            .get(&self.registration_id)
            .is_some_and(|registration| registration.epoch == self.epoch)
        {
            state.registrations.remove(&self.registration_id);
        }
    }
}

impl fmt::Debug for RoutedInvalidationRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedInvalidationRegistration(<redacted>)")
    }
}

/// Fully admitted, still packet-free routed scan parts.
///
/// This value is non-cloneable. `absolute_deadline` is fixed before the final
/// admission check and is never reconstructed from a relative duration, so
/// store, snapshot, revalidation, and later queue delay all consume the same
/// budget. A future runner must retain this value and observe both cancellation
/// signals while it owns any derived socket work.
///
/// Dropping this value cancels its in-memory invalidation registration but
/// deliberately does not weaken the durable crash-conservative reservation.
/// The future consuming runner must own exact completion (including its own
/// drop/error paths) before production wiring may use admitted authority.
pub(crate) struct AdmittedRoutedScan {
    scan: RevalidatedRoutedScan,
    absolute_deadline: Instant,
    request_cancellation: CancellationToken,
    invalidation: RoutedInvalidationRegistration,
}

impl AdmittedRoutedScan {
    #[must_use]
    pub(crate) fn scan(&self) -> &RevalidatedRoutedScan {
        &self.scan
    }

    #[must_use]
    pub(crate) const fn absolute_deadline(&self) -> Instant {
        self.absolute_deadline
    }

    #[must_use]
    pub(crate) fn request_cancellation(&self) -> &CancellationToken {
        &self.request_cancellation
    }

    #[must_use]
    pub(crate) fn invalidation_cancellation(&self) -> &CancellationToken {
        self.invalidation.cancellation()
    }

    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.request_cancellation.is_cancelled() || !self.invalidation.is_current()
    }
}

impl fmt::Debug for AdmittedRoutedScan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedRoutedScan")
            .field("scan", &self.scan)
            .field("absolute_deadline", &"<redacted>")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

pub(crate) enum RoutedAdmissionDecision {
    Admitted(AdmittedRoutedScan),
    NeedsApproval(RoutedProposalSummary),
    CoolingDown { remaining: Duration },
    Busy,
    PublishedWithoutPermit { durability: CommitDurability },
}

impl fmt::Debug for RoutedAdmissionDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admitted(admitted) => formatter.debug_tuple("Admitted").field(admitted).finish(),
            Self::NeedsApproval(summary) => formatter
                .debug_tuple("NeedsApproval")
                .field(summary)
                .finish(),
            Self::CoolingDown { remaining } => formatter
                .debug_struct("CoolingDown")
                .field("remaining", remaining)
                .finish(),
            Self::Busy => formatter.write_str("Busy"),
            Self::PublishedWithoutPermit { durability } => formatter
                .debug_struct("PublishedWithoutPermit")
                .field("durability", durability)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum RoutedAdmissionError {
    #[error("routed admission was cancelled")]
    Cancelled,
    #[error("the healthy route epoch was rejected: {0}")]
    Invalidation(InvalidationHubError),
    #[error("the registered route epoch was invalidated")]
    Invalidated,
    #[error("the admission clock was unavailable")]
    ClockUnavailable,
    #[error("the admission clock supplied an invalid sample")]
    InvalidClockSample,
    #[error("the admission clock moved backward")]
    ClockRollback,
    #[error("the admission clock could not represent the lease safely")]
    ClockOverflow,
    #[error("approval storage rejected admission: {0}")]
    Store(StoreError),
    #[error("approval storage clamped a rolled-back policy time")]
    StoreTimeClamped,
    #[error("the routed reservation expired during admission")]
    ReservationExpired,
    #[error("the fresh route snapshot was unavailable")]
    SnapshotUnavailable,
    #[error("fresh routed authority revalidation failed: {0}")]
    Revalidation(StoredRevalidationError),
    #[error("revalidated routed authority did not match its reservation")]
    AuthorityChanged,
}

/// Packet-free owner of the reserve/snapshot/revalidation sequence.
pub(crate) struct RoutedAdmissionBoundary<'a, C: ?Sized, P: ?Sized> {
    store: &'a ApprovalStore,
    invalidations: &'a RoutedInvalidationHub,
    clock: &'a C,
    routes: &'a P,
}

impl<'a, C, P> RoutedAdmissionBoundary<'a, C, P>
where
    C: RoutedAdmissionClock + ?Sized,
    P: RouteProvider + ?Sized,
{
    pub(crate) const fn new(
        store: &'a ApprovalStore,
        invalidations: &'a RoutedInvalidationHub,
        clock: &'a C,
        routes: &'a P,
    ) -> Self {
        Self {
            store,
            invalidations,
            clock,
            routes,
        }
    }

    /// Register invalidation, reserve durably, snapshot, and revalidate.
    pub(crate) fn admit(
        &self,
        proposal: StoredRoutedProposal,
        trigger: RoutedScanTrigger,
        healthy_epoch: &HealthyRouteEpoch,
        cancellation: &CancellationToken,
    ) -> Result<RoutedAdmissionDecision, RoutedAdmissionError> {
        if cancellation.is_cancelled() {
            return Err(RoutedAdmissionError::Cancelled);
        }

        // Registration deliberately precedes the first store mutation.
        let registration = self
            .invalidations
            .register(healthy_epoch)
            .map_err(RoutedAdmissionError::Invalidation)?;
        ensure_live(&registration, cancellation)?;

        let first = sample_live(self.clock, &registration, cancellation)?;
        let reserve_now = first.conservative_policy_time()?;
        let expected_expiry_seconds = reserve_now
            .as_seconds()
            .checked_add(RESERVATION_LEASE.as_secs())
            .ok_or(RoutedAdmissionError::ClockOverflow)?;
        let expected_expiry = RoutedPolicyTime::from_seconds(expected_expiry_seconds);
        let mut timeline = AdmissionTimeline::new(first, expected_expiry)?;

        ensure_live(&registration, cancellation)?;
        let decision = self
            .store
            .reserve(proposal, trigger, reserve_now)
            .map_err(RoutedAdmissionError::Store)?;

        let permit = match decision {
            StoredBeginDecision::Permitted(permit) => permit,
            StoredBeginDecision::NeedsApproval(summary) => {
                ensure_live(&registration, cancellation)?;
                return Ok(RoutedAdmissionDecision::NeedsApproval(summary));
            }
            StoredBeginDecision::CoolingDown { remaining } => {
                ensure_live(&registration, cancellation)?;
                return Ok(RoutedAdmissionDecision::CoolingDown { remaining });
            }
            StoredBeginDecision::Busy => {
                ensure_live(&registration, cancellation)?;
                return Ok(RoutedAdmissionDecision::Busy);
            }
            StoredBeginDecision::PublishedWithoutPermit { durability } => {
                ensure_live(&registration, cancellation)?;
                return Ok(RoutedAdmissionDecision::PublishedWithoutPermit { durability });
            }
        };

        let fingerprint = permit.fingerprint();
        let run_id = permit.run_id();
        let reservation_expires_at = permit.expires_at();
        ensure_live(&registration, cancellation)?;
        if permit.expires_at() != expected_expiry {
            return Err(RoutedAdmissionError::StoreTimeClamped);
        }

        checkpoint(self.clock, &registration, cancellation, &mut timeline)?;

        ensure_live(&registration, cancellation)?;
        let snapshot = self
            .routes
            .snapshot()
            .map_err(|_| RoutedAdmissionError::SnapshotUnavailable)?;
        ensure_live(&registration, cancellation)?;

        let before_revalidation =
            checkpoint(self.clock, &registration, cancellation, &mut timeline)?;
        let revalidation_now = before_revalidation.conservative_policy_time()?;

        ensure_live(&registration, cancellation)?;
        let scan = self
            .store
            .revalidate_permit(permit, &snapshot, revalidation_now)
            .map_err(RoutedAdmissionError::Revalidation)?;
        ensure_live(&registration, cancellation)?;

        if scan.validated_at() != revalidation_now {
            return Err(RoutedAdmissionError::StoreTimeClamped);
        }
        if scan.fingerprint() != fingerprint
            || scan.run_id() != run_id
            || scan.reservation_expires_at() != reservation_expires_at
        {
            return Err(RoutedAdmissionError::AuthorityChanged);
        }

        // Anchor the approved scan work budget before the revalidation call's
        // queueing and lock time can disappear from it.
        let scan_deadline = before_revalidation
            .monotonic_time
            .checked_add(scan.scan_config().overall_deadline())
            .ok_or(RoutedAdmissionError::ClockOverflow)?;
        let mut absolute_deadline = timeline.lease_deadline.min(scan_deadline);

        let final_sample = checkpoint(self.clock, &registration, cancellation, &mut timeline)?;
        absolute_deadline = absolute_deadline.min(timeline.lease_deadline);
        if final_sample.monotonic_time >= absolute_deadline {
            return Err(RoutedAdmissionError::ReservationExpired);
        }

        // Keep a linked request-cancellation clone in the admitted value so a
        // cancellation immediately after this final check cannot be lost.
        let request_cancellation = cancellation.clone();
        ensure_live(&registration, &request_cancellation)?;

        Ok(RoutedAdmissionDecision::Admitted(AdmittedRoutedScan {
            scan,
            absolute_deadline,
            request_cancellation,
            invalidation: registration,
        }))
    }

    /// Cancel all admitted authority before mutating the exact store record.
    pub(crate) fn revoke(
        &self,
        proposal: &StoredRoutedProposal,
    ) -> Result<StoredRevokeDecision, StoreError> {
        self.invalidations.invalidate();
        self.store.revoke(proposal)
    }

    /// Cancel all admitted authority before clearing remembered approvals.
    pub(crate) fn revoke_all(&self) -> Result<StoredRevokeDecision, StoreError> {
        self.invalidations.invalidate();
        self.store.revoke_all()
    }
}

struct AdmissionTimeline {
    last_sample: RoutedClockSample,
    lease_expires_at: RoutedPolicyTime,
    lease_deadline: Instant,
}

impl AdmissionTimeline {
    fn new(
        first: RoutedClockSample,
        lease_expires_at: RoutedPolicyTime,
    ) -> Result<Self, RoutedAdmissionError> {
        first.exact_policy_time()?;
        let lease_deadline = first
            .monotonic_time
            .checked_add(RESERVATION_LEASE)
            .ok_or(RoutedAdmissionError::ClockOverflow)?;
        let mut timeline = Self {
            last_sample: first,
            lease_expires_at,
            lease_deadline,
        };
        timeline.observe(first)?;
        Ok(timeline)
    }

    fn observe(
        &mut self,
        sample: RoutedClockSample,
    ) -> Result<RoutedClockSample, RoutedAdmissionError> {
        let last_policy = self.last_sample.exact_policy_time()?;
        let policy = sample.exact_policy_time()?;
        if policy < last_policy || sample.monotonic_time < self.last_sample.monotonic_time {
            return Err(RoutedAdmissionError::ClockRollback);
        }

        let expiry = Duration::from_secs(self.lease_expires_at.as_seconds());
        let remaining = expiry
            .checked_sub(policy)
            .ok_or(RoutedAdmissionError::ReservationExpired)?;
        if remaining.is_zero() {
            return Err(RoutedAdmissionError::ReservationExpired);
        }
        let mapped_deadline = sample
            .monotonic_time
            .checked_add(remaining)
            .ok_or(RoutedAdmissionError::ClockOverflow)?;
        self.lease_deadline = self.lease_deadline.min(mapped_deadline);
        if sample.monotonic_time >= self.lease_deadline {
            return Err(RoutedAdmissionError::ReservationExpired);
        }
        self.last_sample = sample;
        Ok(sample)
    }
}

fn sample_live<C: RoutedAdmissionClock + ?Sized>(
    clock: &C,
    registration: &RoutedInvalidationRegistration,
    cancellation: &CancellationToken,
) -> Result<RoutedClockSample, RoutedAdmissionError> {
    ensure_live(registration, cancellation)?;
    let sample = clock
        .sample()
        .map_err(|_| RoutedAdmissionError::ClockUnavailable)?;
    ensure_live(registration, cancellation)?;
    sample.exact_policy_time()?;
    Ok(sample)
}

fn checkpoint<C: RoutedAdmissionClock + ?Sized>(
    clock: &C,
    registration: &RoutedInvalidationRegistration,
    cancellation: &CancellationToken,
    timeline: &mut AdmissionTimeline,
) -> Result<RoutedClockSample, RoutedAdmissionError> {
    let sample = sample_live(clock, registration, cancellation)?;
    timeline.observe(sample)
}

fn ensure_live(
    registration: &RoutedInvalidationRegistration,
    cancellation: &CancellationToken,
) -> Result<(), RoutedAdmissionError> {
    if cancellation.is_cancelled() {
        return Err(RoutedAdmissionError::Cancelled);
    }
    if !registration.is_current() {
        return Err(RoutedAdmissionError::Invalidated);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ipnet::IpNet;

    use super::super::store::{ApprovalStoreStatus, StorePaths};
    use super::*;
    use crate::discovery::{
        InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, ProbeConfig, RouteKind,
        RouteScope, RouteSnapshot, RoutedScanConfig, select_route_candidates,
    };

    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }

    impl<T: ?Sized> AmbiguousIfClone<()> for T {}

    struct ImplementsClone;

    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}

    #[test]
    fn admission_authority_types_do_not_implement_clone() {
        let _ = <RoutedInvalidationHub as AmbiguousIfClone<_>>::marker;
        let _ = <RouteObserverSession as AmbiguousIfClone<_>>::marker;
        let _ = <RoutedInvalidationRegistration as AmbiguousIfClone<_>>::marker;
        let _ = <AdmittedRoutedScan as AmbiguousIfClone<_>>::marker;
    }

    struct ClockStep {
        result: Result<RoutedClockSample, RoutedClockReadError>,
        action: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl ClockStep {
        fn sample(sample: RoutedClockSample) -> Self {
            Self {
                result: Ok(sample),
                action: None,
            }
        }

        fn sample_then(
            sample: RoutedClockSample,
            action: impl Fn() + Send + Sync + 'static,
        ) -> Self {
            Self {
                result: Ok(sample),
                action: Some(Box::new(action)),
            }
        }

        fn unavailable() -> Self {
            Self {
                result: Err(RoutedClockReadError),
                action: None,
            }
        }
    }

    struct FakeClock {
        steps: Mutex<VecDeque<ClockStep>>,
        calls: AtomicUsize,
    }

    impl FakeClock {
        fn new(steps: impl IntoIterator<Item = ClockStep>) -> Self {
            Self {
                steps: Mutex::new(steps.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl RoutedAdmissionClock for FakeClock {
        fn sample(&self) -> Result<RoutedClockSample, RoutedClockReadError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let step = self
                .steps
                .lock()
                .expect("fake clock lock")
                .pop_front()
                .expect("a clock step for every admission boundary");
            if let Some(action) = step.action {
                action();
            }
            step.result
        }
    }

    struct FakeRouteProvider {
        snapshot: Option<RouteSnapshot>,
        action: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        calls: AtomicUsize,
    }

    impl FakeRouteProvider {
        fn ready(snapshot: RouteSnapshot) -> Self {
            Self {
                snapshot: Some(snapshot),
                action: Mutex::new(None),
                calls: AtomicUsize::new(0),
            }
        }

        fn ready_then(snapshot: RouteSnapshot, action: impl Fn() + Send + Sync + 'static) -> Self {
            Self {
                snapshot: Some(snapshot),
                action: Mutex::new(Some(Box::new(action))),
                calls: AtomicUsize::new(0),
            }
        }

        fn unavailable() -> Self {
            Self {
                snapshot: None,
                action: Mutex::new(None),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl RouteProvider for FakeRouteProvider {
        fn snapshot(&self) -> io::Result<RouteSnapshot> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(action) = self.action.lock().expect("fake provider lock").take() {
                action();
            }
            self.snapshot
                .clone()
                .ok_or_else(|| io::Error::other("synthetic route snapshot failure"))
        }
    }

    fn clock_sample(
        base: Instant,
        policy_seconds: u64,
        policy_nanos: u32,
        monotonic_offset: Duration,
    ) -> RoutedClockSample {
        RoutedClockSample::new(
            RoutedPolicyTime::from_seconds(policy_seconds),
            policy_nanos,
            base.checked_add(monotonic_offset)
                .expect("small fake monotonic offset"),
        )
    }

    fn ordinary_clock(base: Instant, policy_seconds: u64) -> FakeClock {
        FakeClock::new([
            ClockStep::sample(clock_sample(base, policy_seconds, 0, Duration::ZERO)),
            ClockStep::sample(clock_sample(
                base,
                policy_seconds,
                100_000_000,
                Duration::from_secs(1),
            )),
            ClockStep::sample(clock_sample(
                base,
                policy_seconds,
                200_000_000,
                Duration::from_secs(2),
            )),
            ClockStep::sample(clock_sample(
                base,
                policy_seconds,
                300_000_000,
                Duration::from_secs(3),
            )),
        ])
    }

    fn ipnet(value: &str) -> IpNet {
        value.parse().expect("valid synthetic test network")
    }

    fn snapshot(interface_id: u64) -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![NetworkInterface::new(
                InterfaceId::new(interface_id),
                "synthetic-admission-tunnel",
                InterfaceKind::Tunnel,
                true,
                [ipnet("10.250.0.2/32")],
            )],
            vec![NetworkRoute::effective(
                ipnet("172.31.90.8/30"),
                Some(InterfaceId::new(interface_id)),
                RouteKind::Unicast,
                RouteScope::OnLink,
            )],
        )
    }

    #[cfg(unix)]
    struct ApprovedFixture {
        _temporary: tempfile::TempDir,
        store: ApprovalStore,
        snapshot: RouteSnapshot,
    }

    #[cfg(unix)]
    impl ApprovedFixture {
        fn new(approval_time: u64) -> Self {
            let temporary = tempfile::tempdir().expect("private store parent");
            let store = ApprovalStore::new(StorePaths::new(temporary.path().join("private")));
            let snapshot = snapshot(7);
            let proposal = proposal(&store, &snapshot);
            let commit = store
                .approve(&proposal, RoutedPolicyTime::from_seconds(approval_time))
                .expect("approve synthetic proposal");
            assert!(commit.is_confirmed());
            Self {
                _temporary: temporary,
                store,
                snapshot,
            }
        }

        fn proposal(&self) -> StoredRoutedProposal {
            proposal(&self.store, &self.snapshot)
        }
    }

    #[cfg(unix)]
    fn proposal(store: &ApprovalStore, snapshot: &RouteSnapshot) -> StoredRoutedProposal {
        let candidates =
            select_route_candidates(snapshot, &[]).expect("synthetic route candidates");
        store
            .build_proposal(
                snapshot,
                &candidates,
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .expect("synthetic stored proposal")
    }

    #[test]
    fn observer_session_exclusively_mints_epochs_and_drop_invalidates() {
        let hub = RoutedInvalidationHub::new();
        let foreign_hub = RoutedInvalidationHub::new();
        let foreign_observer = foreign_hub.start_observer_session().unwrap();
        let foreign_epoch = foreign_observer.establish_healthy_epoch().unwrap();
        assert_eq!(
            hub.register(&foreign_epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );

        let observer = hub.start_observer_session().unwrap();
        let first = observer.establish_healthy_epoch().unwrap();
        assert_eq!(observer.establish_healthy_epoch().unwrap(), first);
        assert_eq!(
            hub.register(&foreign_epoch).unwrap_err(),
            InvalidationHubError::StaleEpoch
        );
        let registration = hub.register(&first).unwrap();
        assert!(registration.is_current());

        observer.invalidate().unwrap();
        assert!(!registration.is_current());
        assert!(registration.cancellation().is_cancelled());
        assert_eq!(
            hub.register(&first).unwrap_err(),
            InvalidationHubError::Unhealthy
        );

        let second = observer.establish_healthy_epoch().unwrap();
        assert_ne!(second, first);
        assert_eq!(
            hub.register(&first).unwrap_err(),
            InvalidationHubError::StaleEpoch
        );
        let registration = hub.register(&second).unwrap();
        assert!(registration.is_current());
        let signal = registration.cancellation().clone();
        drop(observer);
        assert!(!registration.is_current());
        assert!(signal.is_cancelled());
        assert_eq!(
            hub.register(&second).unwrap_err(),
            InvalidationHubError::Unhealthy
        );
    }

    #[test]
    fn superseded_observer_is_inert_and_cannot_restore_health() {
        let hub = RoutedInvalidationHub::new();
        let stale_observer = hub.start_observer_session().unwrap();
        let stale_epoch = stale_observer.establish_healthy_epoch().unwrap();
        let stale_registration = hub.register(&stale_epoch).unwrap();

        let current_observer = hub.start_observer_session().unwrap();
        assert!(!stale_registration.is_current());
        assert_eq!(
            stale_observer.establish_healthy_epoch().unwrap_err(),
            InvalidationHubError::StaleObserver
        );
        assert_eq!(
            stale_observer.invalidate().unwrap_err(),
            InvalidationHubError::StaleObserver
        );
        assert_eq!(
            hub.register(&stale_epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );

        let current_epoch = current_observer.establish_healthy_epoch().unwrap();
        let current_registration = hub.register(&current_epoch).unwrap();
        drop(stale_observer);
        assert!(current_registration.is_current());
        assert_eq!(
            hub.register(&stale_epoch).unwrap_err(),
            InvalidationHubError::StaleEpoch
        );
    }

    #[test]
    fn dropping_a_registration_cancels_any_retained_signal_clone() {
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let registration = hub.register(&epoch).unwrap();
        let escaped_signal = registration.cancellation().clone();
        drop(registration);
        assert!(escaped_signal.is_cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_fresh_snapshot_is_admitted_with_one_absolute_budget() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let base = Instant::now();
        let clock = ordinary_clock(base, 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let cancellation = CancellationToken::new();
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        let decision = boundary
            .admit(
                fixture.proposal(),
                RoutedScanTrigger::ExplicitRefresh,
                &epoch,
                &cancellation,
            )
            .unwrap();
        let admitted = match decision {
            RoutedAdmissionDecision::Admitted(admitted) => admitted,
            other => panic!("expected admitted routed scan, got {other:?}"),
        };

        assert_eq!(clock.calls(), 4);
        assert_eq!(routes.calls(), 1);
        assert_eq!(
            admitted.scan().validated_at(),
            RoutedPolicyTime::from_seconds(101)
        );
        assert_eq!(admitted.absolute_deadline(), base + Duration::from_secs(17));
        assert!(!admitted.is_cancelled());
        assert!(!admitted.request_cancellation().is_cancelled());
        assert!(!admitted.invalidation_cancellation().is_cancelled());

        let escaped_invalidation = admitted.invalidation_cancellation().clone();
        drop(admitted);
        assert!(escaped_invalidation.is_cancelled());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_admitted_scan_cancels_memory_but_keeps_store_busy_until_expiry() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);
        let admitted = match boundary
            .admit(
                fixture.proposal(),
                RoutedScanTrigger::ExplicitRefresh,
                &epoch,
                &CancellationToken::new(),
            )
            .unwrap()
        {
            RoutedAdmissionDecision::Admitted(admitted) => admitted,
            other => panic!("expected admitted routed scan, got {other:?}"),
        };
        let invalidation = admitted.invalidation_cancellation().clone();

        drop(admitted);
        assert!(invalidation.is_cancelled());
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: true,
                ..
            }
        ));
        assert!(matches!(
            fixture
                .store
                .reserve(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    RoutedPolicyTime::from_seconds(159),
                )
                .unwrap(),
            StoredBeginDecision::Busy
        ));
        assert!(matches!(
            fixture
                .store
                .reserve(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    RoutedPolicyTime::from_seconds(160),
                )
                .unwrap(),
            StoredBeginDecision::Permitted(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pre_cancel_and_unhealthy_epoch_cannot_create_a_reservation() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let foreign_hub = RoutedInvalidationHub::new();
        let _foreign_observer = foreign_hub.start_observer_session().unwrap();
        let foreign_epoch = _foreign_observer.establish_healthy_epoch().unwrap();
        let base = Instant::now();
        let clock = ordinary_clock(base, 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        assert_eq!(
            boundary
                .admit(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &foreign_epoch,
                    &cancellation,
                )
                .unwrap_err(),
            RoutedAdmissionError::Cancelled
        );
        assert_eq!(clock.calls(), 0);
        assert_eq!(routes.calls(), 0);
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: false,
                ..
            }
        ));

        let not_cancelled = CancellationToken::new();
        assert_eq!(
            boundary
                .admit(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &foreign_epoch,
                    &not_cancelled,
                )
                .unwrap_err(),
            RoutedAdmissionError::Invalidation(InvalidationHubError::Unhealthy)
        );
        assert_eq!(clock.calls(), 0);
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: false,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_or_invalidation_during_snapshot_never_admits() {
        let cancelled_fixture = ApprovedFixture::new(100);
        let cancelled_hub = RoutedInvalidationHub::new();
        let _cancelled_observer = cancelled_hub.start_observer_session().unwrap();
        let cancelled_epoch = _cancelled_observer.establish_healthy_epoch().unwrap();
        let cancelled_token = CancellationToken::new();
        let cancellation_action = cancelled_token.clone();
        let cancelled_routes =
            FakeRouteProvider::ready_then(cancelled_fixture.snapshot.clone(), move || {
                cancellation_action.cancel()
            });
        let cancelled_clock = ordinary_clock(Instant::now(), 100);
        let cancelled_boundary = RoutedAdmissionBoundary::new(
            &cancelled_fixture.store,
            &cancelled_hub,
            &cancelled_clock,
            &cancelled_routes,
        );
        assert_eq!(
            cancelled_boundary
                .admit(
                    cancelled_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &cancelled_epoch,
                    &cancelled_token,
                )
                .unwrap_err(),
            RoutedAdmissionError::Cancelled
        );
        assert_eq!(cancelled_routes.calls(), 1);
        assert_eq!(cancelled_clock.calls(), 2);
        assert!(matches!(
            cancelled_fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: true,
                ..
            }
        ));

        let invalidated_fixture = ApprovedFixture::new(100);
        let invalidated_hub = Arc::new(RoutedInvalidationHub::new());
        let _invalidated_observer = invalidated_hub.start_observer_session().unwrap();
        let invalidated_epoch = _invalidated_observer.establish_healthy_epoch().unwrap();
        let invalidation_action = Arc::clone(&invalidated_hub);
        let invalidated_routes =
            FakeRouteProvider::ready_then(invalidated_fixture.snapshot.clone(), move || {
                invalidation_action.invalidate()
            });
        let invalidated_clock = ordinary_clock(Instant::now(), 100);
        let invalidated_boundary = RoutedAdmissionBoundary::new(
            &invalidated_fixture.store,
            invalidated_hub.as_ref(),
            &invalidated_clock,
            &invalidated_routes,
        );
        assert_eq!(
            invalidated_boundary
                .admit(
                    invalidated_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &invalidated_epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::Invalidated
        );
        assert_eq!(invalidated_routes.calls(), 1);
        assert_eq!(invalidated_clock.calls(), 2);
        assert!(matches!(
            invalidated_fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: true,
                ..
            }
        ));
        assert_eq!(
            invalidated_hub.register(&invalidated_epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );
    }

    #[cfg(unix)]
    #[test]
    fn final_clock_boundary_rejects_cancellation_and_invalidation() {
        let cancelled_fixture = ApprovedFixture::new(100);
        let cancelled_hub = RoutedInvalidationHub::new();
        let _cancelled_observer = cancelled_hub.start_observer_session().unwrap();
        let cancelled_epoch = _cancelled_observer.establish_healthy_epoch().unwrap();
        let cancelled_token = CancellationToken::new();
        let final_cancellation = cancelled_token.clone();
        let base = Instant::now();
        let cancelled_clock = FakeClock::new([
            ClockStep::sample(clock_sample(base, 100, 0, Duration::ZERO)),
            ClockStep::sample(clock_sample(base, 100, 100, Duration::from_secs(1))),
            ClockStep::sample(clock_sample(base, 100, 200, Duration::from_secs(2))),
            ClockStep::sample_then(
                clock_sample(base, 100, 300, Duration::from_secs(3)),
                move || final_cancellation.cancel(),
            ),
        ]);
        let routes = FakeRouteProvider::ready(cancelled_fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(
            &cancelled_fixture.store,
            &cancelled_hub,
            &cancelled_clock,
            &routes,
        );
        assert_eq!(
            boundary
                .admit(
                    cancelled_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &cancelled_epoch,
                    &cancelled_token,
                )
                .unwrap_err(),
            RoutedAdmissionError::Cancelled
        );

        let invalidated_fixture = ApprovedFixture::new(100);
        let invalidated_hub = Arc::new(RoutedInvalidationHub::new());
        let _invalidated_observer = invalidated_hub.start_observer_session().unwrap();
        let invalidated_epoch = _invalidated_observer.establish_healthy_epoch().unwrap();
        let final_invalidation = Arc::clone(&invalidated_hub);
        let base = Instant::now();
        let invalidated_clock = FakeClock::new([
            ClockStep::sample(clock_sample(base, 100, 0, Duration::ZERO)),
            ClockStep::sample(clock_sample(base, 100, 100, Duration::from_secs(1))),
            ClockStep::sample(clock_sample(base, 100, 200, Duration::from_secs(2))),
            ClockStep::sample_then(
                clock_sample(base, 100, 300, Duration::from_secs(3)),
                move || final_invalidation.invalidate(),
            ),
        ]);
        let routes = FakeRouteProvider::ready(invalidated_fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(
            &invalidated_fixture.store,
            invalidated_hub.as_ref(),
            &invalidated_clock,
            &routes,
        );
        assert_eq!(
            boundary
                .admit(
                    invalidated_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &invalidated_epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::Invalidated
        );
    }

    #[cfg(unix)]
    #[test]
    fn subsecond_policy_or_monotonic_rollback_fails_closed() {
        let policy_fixture = ApprovedFixture::new(100);
        let policy_hub = RoutedInvalidationHub::new();
        let _policy_observer = policy_hub.start_observer_session().unwrap();
        let policy_epoch = _policy_observer.establish_healthy_epoch().unwrap();
        let base = Instant::now();
        let policy_clock = FakeClock::new([
            ClockStep::sample(clock_sample(base, 100, 900_000_000, Duration::ZERO)),
            ClockStep::sample(clock_sample(base, 100, 800_000_000, Duration::from_secs(1))),
        ]);
        let routes = FakeRouteProvider::ready(policy_fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(
            &policy_fixture.store,
            &policy_hub,
            &policy_clock,
            &routes,
        );
        assert_eq!(
            boundary
                .admit(
                    policy_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &policy_epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::ClockRollback
        );

        let monotonic_fixture = ApprovedFixture::new(100);
        let monotonic_hub = RoutedInvalidationHub::new();
        let _monotonic_observer = monotonic_hub.start_observer_session().unwrap();
        let monotonic_epoch = _monotonic_observer.establish_healthy_epoch().unwrap();
        let base = Instant::now();
        let monotonic_clock = FakeClock::new([
            ClockStep::sample(clock_sample(base, 100, 0, Duration::from_secs(2))),
            ClockStep::sample(clock_sample(base, 100, 100, Duration::from_secs(1))),
        ]);
        let routes = FakeRouteProvider::ready(monotonic_fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(
            &monotonic_fixture.store,
            &monotonic_hub,
            &monotonic_clock,
            &routes,
        );
        assert_eq!(
            boundary
                .admit(
                    monotonic_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &monotonic_epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::ClockRollback
        );
    }

    #[cfg(unix)]
    #[test]
    fn prior_store_time_high_water_is_rejected_instead_of_clamped() {
        let fixture = ApprovedFixture::new(200);
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        assert_eq!(
            boundary
                .admit(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::StoreTimeClamped
        );
        assert_eq!(clock.calls(), 1);
        assert_eq!(routes.calls(), 0);
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: true,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn escaped_permit_failure_retains_crash_conservative_authority() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::unavailable();
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        assert_eq!(
            boundary
                .admit(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::SnapshotUnavailable
        );
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                has_active_reservation: true,
                ..
            }
        ));

        // No stale clock observation may shorten the confirmed reservation or
        // its crash-conservative cooldown after admission fails.
        assert!(matches!(
            fixture
                .store
                .reserve(
                    fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    RoutedPolicyTime::from_seconds(159),
                )
                .unwrap(),
            StoredBeginDecision::Busy
        ));
        assert!(matches!(
            fixture
                .store
                .reserve(
                    fixture.proposal(),
                    RoutedScanTrigger::Automatic,
                    RoutedPolicyTime::from_seconds(160),
                )
                .unwrap(),
            StoredBeginDecision::CoolingDown { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn elapsed_snapshot_and_revalidation_queue_time_never_extend_the_lease() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let _observer = hub.start_observer_session().unwrap();
        let epoch = _observer.establish_healthy_epoch().unwrap();
        let base = Instant::now();
        let clock = FakeClock::new([
            ClockStep::sample(clock_sample(base, 100, 100_000_000, Duration::ZERO)),
            ClockStep::sample(clock_sample(base, 100, 200_000_000, Duration::from_secs(5))),
            ClockStep::sample(clock_sample(
                base,
                130,
                200_000_000,
                Duration::from_secs(40),
            )),
            ClockStep::sample(clock_sample(
                base,
                135,
                200_000_000,
                Duration::from_secs(45),
            )),
        ]);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        let admitted = match boundary
            .admit(
                fixture.proposal(),
                RoutedScanTrigger::ExplicitRefresh,
                &epoch,
                &CancellationToken::new(),
            )
            .unwrap()
        {
            RoutedAdmissionDecision::Admitted(admitted) => admitted,
            other => panic!("expected admitted routed scan, got {other:?}"),
        };

        // The scan's 15-second budget starts before store revalidation. The
        // final admission check has therefore already consumed five seconds.
        assert_eq!(admitted.absolute_deadline(), base + Duration::from_secs(55));
        // The reservation itself remains capped at start + 60 even though
        // upward wall rounding could otherwise map it later.
        assert!(admitted.absolute_deadline() <= base + RESERVATION_LEASE);
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_clock_or_snapshot_is_topology_redacted_and_fails_closed() {
        let clock_fixture = ApprovedFixture::new(100);
        let clock_hub = RoutedInvalidationHub::new();
        let _clock_observer = clock_hub.start_observer_session().unwrap();
        let clock_epoch = _clock_observer.establish_healthy_epoch().unwrap();
        let clock = FakeClock::new([ClockStep::unavailable()]);
        let routes = FakeRouteProvider::ready(clock_fixture.snapshot.clone());
        let boundary =
            RoutedAdmissionBoundary::new(&clock_fixture.store, &clock_hub, &clock, &routes);
        assert_eq!(
            boundary
                .admit(
                    clock_fixture.proposal(),
                    RoutedScanTrigger::ExplicitRefresh,
                    &clock_epoch,
                    &CancellationToken::new(),
                )
                .unwrap_err(),
            RoutedAdmissionError::ClockUnavailable
        );

        let snapshot_fixture = ApprovedFixture::new(100);
        let snapshot_hub = RoutedInvalidationHub::new();
        let _snapshot_observer = snapshot_hub.start_observer_session().unwrap();
        let snapshot_epoch = _snapshot_observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::unavailable();
        let boundary =
            RoutedAdmissionBoundary::new(&snapshot_fixture.store, &snapshot_hub, &clock, &routes);
        let error = boundary
            .admit(
                snapshot_fixture.proposal(),
                RoutedScanTrigger::ExplicitRefresh,
                &snapshot_epoch,
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert_eq!(error, RoutedAdmissionError::SnapshotUnavailable);
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("172.31"));
        assert!(!rendered.contains("synthetic-admission-tunnel"));
    }

    #[cfg(unix)]
    #[test]
    fn revoke_invalidates_before_mutation_and_never_restores_health() {
        let fixture = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let observer = hub.start_observer_session().unwrap();
        let epoch = observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);
        let admitted = match boundary
            .admit(
                fixture.proposal(),
                RoutedScanTrigger::ExplicitRefresh,
                &epoch,
                &CancellationToken::new(),
            )
            .unwrap()
        {
            RoutedAdmissionDecision::Admitted(admitted) => admitted,
            other => panic!("expected admitted routed scan, got {other:?}"),
        };
        let revoke_proposal = fixture.proposal();

        assert!(matches!(
            boundary.revoke(&revoke_proposal).unwrap(),
            StoredRevokeDecision::Published(commit) if commit.is_confirmed()
        ));
        assert!(admitted.is_cancelled());
        assert_eq!(
            hub.register(&epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );
        assert!(matches!(
            fixture.store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 0,
                has_active_reservation: false,
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn failed_revoke_and_revoke_all_both_remain_invalidated() {
        let fixture = ApprovedFixture::new(100);
        let other = ApprovedFixture::new(100);
        let hub = RoutedInvalidationHub::new();
        let observer = hub.start_observer_session().unwrap();
        let epoch = observer.establish_healthy_epoch().unwrap();
        let clock = ordinary_clock(Instant::now(), 100);
        let routes = FakeRouteProvider::ready(fixture.snapshot.clone());
        let boundary = RoutedAdmissionBoundary::new(&fixture.store, &hub, &clock, &routes);

        assert_eq!(
            boundary.revoke(&other.proposal()).unwrap_err(),
            StoreError::StaleProposal
        );
        assert_eq!(
            hub.register(&epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );

        let next_epoch = observer.establish_healthy_epoch().unwrap();
        assert!(matches!(
            boundary.revoke_all().unwrap(),
            StoredRevokeDecision::Published(commit) if commit.is_confirmed()
        ));
        assert_eq!(
            hub.register(&next_epoch).unwrap_err(),
            InvalidationHubError::Unhealthy
        );
    }
}
