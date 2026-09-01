//! Combined route and approval-store observation authority.
//!
//! A routed scan is safe only while one exact route observer and one exact
//! approval-store observer are both live. This state machine deliberately
//! exposes no independently pairable healthy source epochs: one incarnation
//! consumes both source-bound baseline tokens and mints one combined epoch.
//! Any source event, source failure, replacement, or drop cancels every
//! registration synchronously before reconciliation work can be scheduled.
//!
//! Linux actor wiring remains separate. Its route and store activation
//! callbacks will be the only production callers of `activate`; until that
//! handoff is complete this module remains unavailable outside the crate.

#![allow(
    dead_code,
    reason = "the consuming routed-discovery controller remains intentionally unwired"
)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::watch::StoreInvalidationSink;

mod activation;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod pair;
#[cfg(target_os = "linux")]
mod store;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObserverSource {
    Route,
    Store,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct HealthyRoutedEpochIdentity {
    incarnation: u64,
    route_generation: u64,
    store_generation: u64,
    healthy_epoch: u64,
}

struct RegisteredAuthority {
    epoch: HealthyRoutedEpochIdentity,
    cancellation: CancellationToken,
}

struct CoordinatorState {
    next_incarnation: u64,
    current_incarnation: Option<u64>,
    route_generation: u64,
    store_generation: u64,
    route_poisoned: bool,
    store_poisoned: bool,
    next_healthy_epoch: u64,
    current_healthy_epoch: Option<HealthyRoutedEpochIdentity>,
    next_registration: u64,
    registrations: BTreeMap<u64, RegisteredAuthority>,
    failed_closed: bool,
}

impl Default for CoordinatorState {
    fn default() -> Self {
        Self {
            next_incarnation: 0,
            current_incarnation: None,
            route_generation: 0,
            store_generation: 0,
            route_poisoned: false,
            store_poisoned: false,
            next_healthy_epoch: 0,
            current_healthy_epoch: None,
            next_registration: 1,
            registrations: BTreeMap::new(),
            failed_closed: false,
        }
    }
}

struct CoordinatorInner {
    state: Mutex<CoordinatorState>,
}

impl CoordinatorInner {
    fn lock(&self) -> Result<MutexGuard<'_, CoordinatorState>, ObserverCoordinatorError> {
        match self.state.lock() {
            Ok(state) if state.failed_closed => Err(ObserverCoordinatorError::FailedClosed),
            Ok(state) => Ok(state),
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                Err(ObserverCoordinatorError::FailedClosed)
            }
        }
    }

    fn notify(&self, incarnation: u64, source: ObserverSource, poison: bool) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                return;
            }
        };
        if state.failed_closed || state.current_incarnation != Some(incarnation) {
            return;
        }
        invalidate_authority(&mut state);
        if increment_source_generation(&mut state, source).is_err() {
            fail_closed(&mut state);
            return;
        }
        if poison {
            match source {
                ObserverSource::Route => state.route_poisoned = true,
                ObserverSource::Store => state.store_poisoned = true,
            }
        }
    }

    fn retire(&self, incarnation: u64) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                return;
            }
        };
        if state.current_incarnation == Some(incarnation) {
            state.current_incarnation = None;
            invalidate_authority(&mut state);
        }
    }
}

impl Drop for CoordinatorInner {
    fn drop(&mut self) {
        let state = match self.state.get_mut() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        fail_closed(state);
    }
}

/// Topology-free errors from combined observer authority.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ObserverCoordinatorError {
    #[error("combined routed observation failed closed")]
    FailedClosed,
    #[error("the routed observer incarnation is stale")]
    StaleIncarnation,
    #[error("the observer sink was already issued")]
    SinkAlreadyIssued,
    #[error("an observer sink is unavailable")]
    SinkUnavailable,
    #[error("an observer source failed closed")]
    SourcePoisoned,
    #[error("the combined observer baseline is stale")]
    StaleBaseline,
    #[error("combined routed observation is not healthy")]
    Unhealthy,
    #[error("the combined healthy routed epoch is stale")]
    StaleEpoch,
}

/// The sole owner of combined observer incarnations and registrations.
///
/// This value is deliberately non-cloneable. Explicit shared ownership, when
/// the controller needs it, must use `Arc` so its lifetime remains visible.
pub(crate) struct RoutedObserverCoordinator {
    inner: Arc<CoordinatorInner>,
}

impl RoutedObserverCoordinator {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(CoordinatorInner {
                state: Mutex::new(CoordinatorState::default()),
            }),
        }
    }

    /// Replace any previous observer pair with one initially unhealthy pair.
    pub(crate) fn start_incarnation(
        &self,
    ) -> Result<RoutedObserverIncarnation, ObserverCoordinatorError> {
        let mut state = self.inner.lock()?;
        let incarnation = state.next_incarnation.checked_add(1).ok_or_else(|| {
            fail_closed(&mut state);
            ObserverCoordinatorError::FailedClosed
        })?;
        invalidate_authority(&mut state);
        state.next_incarnation = incarnation;
        state.current_incarnation = Some(incarnation);
        state.route_generation = 0;
        state.store_generation = 0;
        state.route_poisoned = false;
        state.store_poisoned = false;

        let route_identity = Arc::new(SourceSinkIdentity);
        let store_identity = Arc::new(SourceSinkIdentity);
        Ok(RoutedObserverIncarnation {
            coordinator: Arc::downgrade(&self.inner),
            incarnation,
            route_identity: Arc::downgrade(&route_identity),
            store_identity: Arc::downgrade(&store_identity),
            route_sink: Some(RouteObserverSink {
                coordinator: Arc::downgrade(&self.inner),
                incarnation,
                identity: route_identity,
            }),
            store_sink: Some(StoreObserverSink {
                coordinator: Arc::downgrade(&self.inner),
                incarnation,
                identity: store_identity,
            }),
        })
    }

    /// Register cancellation in one exact current combined epoch.
    pub(crate) fn register(
        &self,
        epoch: &HealthyRoutedEpoch,
    ) -> Result<RoutedAuthorityRegistration, ObserverCoordinatorError> {
        let mut state = self.inner.lock()?;
        let Some(current) = state.current_healthy_epoch else {
            return Err(ObserverCoordinatorError::Unhealthy);
        };
        if !epoch.coordinator.ptr_eq(&Arc::downgrade(&self.inner)) || epoch.identity != current {
            return Err(ObserverCoordinatorError::StaleEpoch);
        }

        let registration_id = state.next_registration;
        state.next_registration = state.next_registration.checked_add(1).ok_or_else(|| {
            fail_closed(&mut state);
            ObserverCoordinatorError::FailedClosed
        })?;
        let cancellation = CancellationToken::new();
        state.registrations.insert(
            registration_id,
            RegisteredAuthority {
                epoch: current,
                cancellation: cancellation.clone(),
            },
        );
        Ok(RoutedAuthorityRegistration {
            coordinator: Arc::downgrade(&self.inner),
            registration_id,
            epoch: current,
            cancellation,
        })
    }

    /// Synchronously revoke all combined in-memory authority.
    pub(crate) fn invalidate(&self) {
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                fail_closed(&mut state);
                return;
            }
        };
        let Some(route_generation) = state.route_generation.checked_add(1) else {
            fail_closed(&mut state);
            return;
        };
        let Some(store_generation) = state.store_generation.checked_add(1) else {
            fail_closed(&mut state);
            return;
        };
        invalidate_authority(&mut state);
        state.route_generation = route_generation;
        state.store_generation = store_generation;
    }
}

impl fmt::Debug for RoutedObserverCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedObserverCoordinator(<redacted>)")
    }
}

struct SourceSinkIdentity;

/// Invalidate-only route callback for one exact combined incarnation.
///
/// The route monitor may retain this value in an `Arc`, but it cannot mint an
/// epoch or access the coordinator. A stale callback is inert. Final sink drop
/// poisons only its own still-current incarnation.
pub(crate) struct RouteObserverSink {
    coordinator: Weak<CoordinatorInner>,
    incarnation: u64,
    identity: Arc<SourceSinkIdentity>,
}

impl RouteObserverSink {
    pub(crate) fn invalidate(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify(self.incarnation, ObserverSource::Route, false);
        }
    }

    pub(crate) fn poison(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify(self.incarnation, ObserverSource::Route, true);
        }
    }
}

impl Drop for RouteObserverSink {
    fn drop(&mut self) {
        self.poison();
    }
}

impl fmt::Debug for RouteObserverSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteObserverSink(<redacted>)")
    }
}

/// Invalidate-only approval-store callback for one exact incarnation.
pub(crate) struct StoreObserverSink {
    coordinator: Weak<CoordinatorInner>,
    incarnation: u64,
    identity: Arc<SourceSinkIdentity>,
}

impl StoreInvalidationSink for StoreObserverSink {
    fn invalidate(&self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify(self.incarnation, ObserverSource::Store, false);
        }
    }
}

impl Drop for StoreObserverSink {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.notify(self.incarnation, ObserverSource::Store, true);
        }
    }
}

impl fmt::Debug for StoreObserverSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreObserverSink(<redacted>)")
    }
}

/// Exclusive controller authority for one route/store observer pair.
pub(crate) struct RoutedObserverIncarnation {
    coordinator: Weak<CoordinatorInner>,
    incarnation: u64,
    route_identity: Weak<SourceSinkIdentity>,
    store_identity: Weak<SourceSinkIdentity>,
    route_sink: Option<RouteObserverSink>,
    store_sink: Option<StoreObserverSink>,
}

impl RoutedObserverIncarnation {
    /// Issue the only route callback for this incarnation.
    pub(crate) fn take_route_sink(
        &mut self,
    ) -> Result<RouteObserverSink, ObserverCoordinatorError> {
        self.ensure_current()?;
        self.route_sink
            .take()
            .ok_or(ObserverCoordinatorError::SinkAlreadyIssued)
    }

    /// Issue the only approval-store callback for this incarnation.
    pub(crate) fn take_store_sink(
        &mut self,
    ) -> Result<StoreObserverSink, ObserverCoordinatorError> {
        self.ensure_current()?;
        self.store_sink
            .take()
            .ok_or(ObserverCoordinatorError::SinkAlreadyIssued)
    }

    /// Begin a route baseline after the route callback has been handed to its
    /// subscribed monitor.
    pub(crate) fn begin_route_baseline(
        &self,
    ) -> Result<RouteBaselineToken, ObserverCoordinatorError> {
        let generation = self.begin_source_baseline(ObserverSource::Route)?;
        Ok(RouteBaselineToken {
            coordinator: self.coordinator.clone(),
            sink: self.route_identity.clone(),
            incarnation: self.incarnation,
            generation,
        })
    }

    /// Begin an approval-store baseline after its directory observer has
    /// subscribed and before its exact reread.
    pub(crate) fn begin_store_baseline(
        &self,
    ) -> Result<StoreBaselineToken, ObserverCoordinatorError> {
        let generation = self.begin_source_baseline(ObserverSource::Store)?;
        Ok(StoreBaselineToken {
            coordinator: self.coordinator.clone(),
            sink: self.store_identity.clone(),
            incarnation: self.incarnation,
            generation,
        })
    }

    /// Consume both exact current source baselines and atomically install the
    /// only epoch which routed admission may register.
    fn activate(
        &self,
        route: RouteBaselineToken,
        store: StoreBaselineToken,
    ) -> Result<HealthyRoutedEpoch, ObserverCoordinatorError> {
        self.validate_baseline_ownership(&route, &store)?;

        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or(ObserverCoordinatorError::FailedClosed)?;
        let mut state = coordinator.lock()?;
        if state.current_incarnation != Some(self.incarnation) {
            return Err(ObserverCoordinatorError::StaleIncarnation);
        }
        if state.route_poisoned || state.store_poisoned {
            return Err(ObserverCoordinatorError::SourcePoisoned);
        }
        if route.generation != state.route_generation || store.generation != state.store_generation
        {
            return Err(ObserverCoordinatorError::StaleBaseline);
        }
        if state.current_healthy_epoch.is_some() {
            fail_closed(&mut state);
            return Err(ObserverCoordinatorError::FailedClosed);
        }

        let healthy_epoch = state.next_healthy_epoch.checked_add(1).ok_or_else(|| {
            fail_closed(&mut state);
            ObserverCoordinatorError::FailedClosed
        })?;
        state.next_healthy_epoch = healthy_epoch;
        let identity = HealthyRoutedEpochIdentity {
            incarnation: self.incarnation,
            route_generation: route.generation,
            store_generation: store.generation,
            healthy_epoch,
        };
        state.current_healthy_epoch = Some(identity);
        Ok(HealthyRoutedEpoch {
            coordinator: Arc::downgrade(&coordinator),
            identity,
        })
    }

    fn validate_baseline_ownership(
        &self,
        route: &RouteBaselineToken,
        store: &StoreBaselineToken,
    ) -> Result<(), ObserverCoordinatorError> {
        self.validate_route_baseline_ownership(route)?;
        self.validate_store_baseline_ownership(store)
    }

    fn validate_route_baseline_ownership(
        &self,
        route: &RouteBaselineToken,
    ) -> Result<(), ObserverCoordinatorError> {
        if !route.coordinator.ptr_eq(&self.coordinator)
            || route.incarnation != self.incarnation
            || !route.sink.ptr_eq(&self.route_identity)
        {
            return Err(ObserverCoordinatorError::StaleBaseline);
        }
        if route.sink.upgrade().is_none() {
            return Err(ObserverCoordinatorError::SinkUnavailable);
        }
        Ok(())
    }

    fn validate_store_baseline_ownership(
        &self,
        store: &StoreBaselineToken,
    ) -> Result<(), ObserverCoordinatorError> {
        if !store.coordinator.ptr_eq(&self.coordinator)
            || store.incarnation != self.incarnation
            || !store.sink.ptr_eq(&self.store_identity)
        {
            return Err(ObserverCoordinatorError::StaleBaseline);
        }
        if store.sink.upgrade().is_none() {
            return Err(ObserverCoordinatorError::SinkUnavailable);
        }
        Ok(())
    }

    fn validate_route_baseline_current(
        &self,
        route: &RouteBaselineToken,
    ) -> Result<(), ObserverCoordinatorError> {
        self.validate_route_baseline_ownership(route)?;
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or(ObserverCoordinatorError::FailedClosed)?;
        let state = coordinator.lock()?;
        if state.current_incarnation != Some(self.incarnation) {
            return Err(ObserverCoordinatorError::StaleIncarnation);
        }
        if state.route_poisoned || state.store_poisoned {
            return Err(ObserverCoordinatorError::SourcePoisoned);
        }
        if route.generation != state.route_generation {
            return Err(ObserverCoordinatorError::StaleBaseline);
        }
        Ok(())
    }

    fn validate_store_baseline_current(
        &self,
        store: &StoreBaselineToken,
    ) -> Result<(), ObserverCoordinatorError> {
        self.validate_store_baseline_ownership(store)?;
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or(ObserverCoordinatorError::FailedClosed)?;
        let state = coordinator.lock()?;
        if state.current_incarnation != Some(self.incarnation) {
            return Err(ObserverCoordinatorError::StaleIncarnation);
        }
        if state.route_poisoned || state.store_poisoned {
            return Err(ObserverCoordinatorError::SourcePoisoned);
        }
        if store.generation != state.store_generation {
            return Err(ObserverCoordinatorError::StaleBaseline);
        }
        Ok(())
    }

    fn begin_source_baseline(
        &self,
        source: ObserverSource,
    ) -> Result<u64, ObserverCoordinatorError> {
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or(ObserverCoordinatorError::FailedClosed)?;
        let sink_is_live = match source {
            ObserverSource::Route => {
                self.route_sink.is_none() && self.route_identity.upgrade().is_some()
            }
            ObserverSource::Store => {
                self.store_sink.is_none() && self.store_identity.upgrade().is_some()
            }
        };
        if !sink_is_live {
            return Err(ObserverCoordinatorError::SinkUnavailable);
        }

        let mut state = coordinator.lock()?;
        if state.current_incarnation != Some(self.incarnation) {
            return Err(ObserverCoordinatorError::StaleIncarnation);
        }
        // A poisoned half makes the pair unusable. Do not let the surviving
        // observer mint a token which could look independently healthy.
        if state.route_poisoned || state.store_poisoned {
            return Err(ObserverCoordinatorError::SourcePoisoned);
        }

        invalidate_authority(&mut state);
        increment_source_generation(&mut state, source).map_err(|()| {
            fail_closed(&mut state);
            ObserverCoordinatorError::FailedClosed
        })?;
        Ok(match source {
            ObserverSource::Route => state.route_generation,
            ObserverSource::Store => state.store_generation,
        })
    }

    fn ensure_current(&self) -> Result<(), ObserverCoordinatorError> {
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or(ObserverCoordinatorError::FailedClosed)?;
        let state = coordinator.lock()?;
        if state.current_incarnation != Some(self.incarnation) {
            return Err(ObserverCoordinatorError::StaleIncarnation);
        }
        Ok(())
    }
}

impl Drop for RoutedObserverIncarnation {
    fn drop(&mut self) {
        if let Some(coordinator) = self.coordinator.upgrade() {
            coordinator.retire(self.incarnation);
        }
    }
}

impl fmt::Debug for RoutedObserverIncarnation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedObserverIncarnation(<redacted>)")
    }
}

/// One-use route baseline, bound to a live route callback and incarnation.
pub(crate) struct RouteBaselineToken {
    coordinator: Weak<CoordinatorInner>,
    sink: Weak<SourceSinkIdentity>,
    incarnation: u64,
    generation: u64,
}

impl fmt::Debug for RouteBaselineToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteBaselineToken(<redacted>)")
    }
}

/// One-use store baseline, bound to a live store callback and incarnation.
pub(crate) struct StoreBaselineToken {
    coordinator: Weak<CoordinatorInner>,
    sink: Weak<SourceSinkIdentity>,
    incarnation: u64,
    generation: u64,
}

impl fmt::Debug for StoreBaselineToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreBaselineToken(<redacted>)")
    }
}

/// Non-cloneable combined route/store health proof.
pub(crate) struct HealthyRoutedEpoch {
    coordinator: Weak<CoordinatorInner>,
    identity: HealthyRoutedEpochIdentity,
}

impl fmt::Debug for HealthyRoutedEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HealthyRoutedEpoch(<redacted>)")
    }
}

/// Non-cloneable registration cancelled by either observed source.
pub(crate) struct RoutedAuthorityRegistration {
    coordinator: Weak<CoordinatorInner>,
    registration_id: u64,
    epoch: HealthyRoutedEpochIdentity,
    cancellation: CancellationToken,
}

impl RoutedAuthorityRegistration {
    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub(crate) fn is_current(&self) -> bool {
        if self.cancellation.is_cancelled() {
            return false;
        }
        let Some(coordinator) = self.coordinator.upgrade() else {
            return false;
        };
        let Ok(state) = coordinator.lock() else {
            self.cancellation.cancel();
            return false;
        };
        state.current_healthy_epoch == Some(self.epoch)
            && state
                .registrations
                .get(&self.registration_id)
                .is_some_and(|registration| registration.epoch == self.epoch)
            && !self.cancellation.is_cancelled()
    }
}

impl Drop for RoutedAuthorityRegistration {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let Some(coordinator) = self.coordinator.upgrade() else {
            return;
        };
        let mut state = match coordinator.state.lock() {
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

impl fmt::Debug for RoutedAuthorityRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedAuthorityRegistration(<redacted>)")
    }
}

fn increment_source_generation(
    state: &mut CoordinatorState,
    source: ObserverSource,
) -> Result<(), ()> {
    let generation = match source {
        ObserverSource::Route => &mut state.route_generation,
        ObserverSource::Store => &mut state.store_generation,
    };
    *generation = generation.checked_add(1).ok_or(())?;
    Ok(())
}

fn invalidate_authority(state: &mut CoordinatorState) {
    state.current_healthy_epoch = None;
    for registration in state.registrations.values() {
        registration.cancellation.cancel();
    }
    state.registrations.clear();
}

fn fail_closed(state: &mut CoordinatorState) {
    state.failed_closed = true;
    state.current_incarnation = None;
    state.route_poisoned = true;
    state.store_poisoned = true;
    invalidate_authority(state);
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }

    impl<T: ?Sized> AmbiguousIfClone<()> for T {}

    struct ImplementsClone;

    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}

    struct HealthyFixture {
        coordinator: RoutedObserverCoordinator,
        incarnation: RoutedObserverIncarnation,
        route: Arc<RouteObserverSink>,
        store: StoreObserverSink,
        epoch: HealthyRoutedEpoch,
    }

    fn healthy_fixture() -> HealthyFixture {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route = Arc::new(incarnation.take_route_sink().unwrap());
        let store = incarnation.take_store_sink().unwrap();
        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        let epoch = incarnation
            .activate(route_baseline, store_baseline)
            .unwrap();
        HealthyFixture {
            coordinator,
            incarnation,
            route,
            store,
            epoch,
        }
    }

    #[test]
    fn combined_authority_types_do_not_implement_clone() {
        let _ = <RoutedObserverCoordinator as AmbiguousIfClone<_>>::marker;
        let _ = <RoutedObserverIncarnation as AmbiguousIfClone<_>>::marker;
        let _ = <RouteObserverSink as AmbiguousIfClone<_>>::marker;
        let _ = <StoreObserverSink as AmbiguousIfClone<_>>::marker;
        let _ = <RouteBaselineToken as AmbiguousIfClone<_>>::marker;
        let _ = <StoreBaselineToken as AmbiguousIfClone<_>>::marker;
        let _ = <HealthyRoutedEpoch as AmbiguousIfClone<_>>::marker;
        let _ = <RoutedAuthorityRegistration as AmbiguousIfClone<_>>::marker;
    }

    #[test]
    fn both_live_source_baselines_are_required_for_one_epoch() {
        let coordinator = RoutedObserverCoordinator::new();
        assert_eq!(
            coordinator
                .register(&HealthyRoutedEpoch {
                    coordinator: Arc::downgrade(&coordinator.inner),
                    identity: HealthyRoutedEpochIdentity {
                        incarnation: 0,
                        route_generation: 0,
                        store_generation: 0,
                        healthy_epoch: 0,
                    },
                })
                .unwrap_err(),
            ObserverCoordinatorError::Unhealthy
        );

        let fixture = healthy_fixture();
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        assert!(registration.is_current());
        fixture.route.invalidate();
        assert!(!registration.is_current());
        assert!(registration.cancellation().is_cancelled());
    }

    #[test]
    fn sinks_must_be_issued_before_their_baselines_and_only_once() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );

        let _route = incarnation.take_route_sink().unwrap();
        let _store = incarnation.take_store_sink().unwrap();
        assert_eq!(
            incarnation.take_route_sink().unwrap_err(),
            ObserverCoordinatorError::SinkAlreadyIssued
        );
        assert_eq!(
            incarnation.take_store_sink().unwrap_err(),
            ObserverCoordinatorError::SinkAlreadyIssued
        );
        incarnation.begin_route_baseline().unwrap();
        incarnation.begin_store_baseline().unwrap();
    }

    #[test]
    fn an_event_at_either_baseline_seam_prevents_activation() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route = Arc::new(incarnation.take_route_sink().unwrap());
        let store = incarnation.take_store_sink().unwrap();

        let stale_route = incarnation.begin_route_baseline().unwrap();
        route.invalidate();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        assert_eq!(
            incarnation
                .activate(stale_route, store_baseline)
                .unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );

        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let stale_store = incarnation.begin_store_baseline().unwrap();
        StoreInvalidationSink::invalidate(&store);
        assert_eq!(
            incarnation
                .activate(route_baseline, stale_store)
                .unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );
    }

    #[test]
    fn global_invalidation_stales_every_pending_source_baseline() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();
        let _store = incarnation.take_store_sink().unwrap();
        let stale_route = incarnation.begin_route_baseline().unwrap();
        let stale_store = incarnation.begin_store_baseline().unwrap();

        coordinator.invalidate();
        assert_eq!(
            incarnation.activate(stale_route, stale_store).unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );

        let fresh_route = incarnation.begin_route_baseline().unwrap();
        let fresh_store = incarnation.begin_store_baseline().unwrap();
        let epoch = incarnation.activate(fresh_route, fresh_store).unwrap();
        let registration = coordinator.register(&epoch).unwrap();
        coordinator.invalidate();
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());
    }

    #[test]
    fn source_failure_is_sticky_for_its_incarnation() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = coordinator.start_incarnation().unwrap();
        let route = Arc::new(incarnation.take_route_sink().unwrap());
        let _store = incarnation.take_store_sink().unwrap();
        route.poison();
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        assert_eq!(
            incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
    }

    #[test]
    fn active_source_poison_cancels_authority_until_replacement() {
        let fixture = healthy_fixture();
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        fixture.route.poison();
        assert!(registration.cancellation().is_cancelled());
        assert_eq!(
            fixture.incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );
        assert_eq!(
            fixture.incarnation.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SourcePoisoned
        );

        let mut replacement = fixture.coordinator.start_incarnation().unwrap();
        let _route = replacement.take_route_sink().unwrap();
        let _store = replacement.take_store_sink().unwrap();
        let route = replacement.begin_route_baseline().unwrap();
        let store = replacement.begin_store_baseline().unwrap();
        let epoch = replacement.activate(route, store).unwrap();
        assert!(fixture.coordinator.register(&epoch).unwrap().is_current());
    }

    #[test]
    fn foreign_or_cross_incarnation_tokens_cannot_be_paired() {
        let first = RoutedObserverCoordinator::new();
        let mut first_incarnation = first.start_incarnation().unwrap();
        let _first_route = first_incarnation.take_route_sink().unwrap();
        let _first_store = first_incarnation.take_store_sink().unwrap();
        let foreign_route = first_incarnation.begin_route_baseline().unwrap();

        let second = RoutedObserverCoordinator::new();
        let mut second_incarnation = second.start_incarnation().unwrap();
        let _second_route = second_incarnation.take_route_sink().unwrap();
        let _second_store = second_incarnation.take_store_sink().unwrap();
        let second_store = second_incarnation.begin_store_baseline().unwrap();
        assert_eq!(
            second_incarnation
                .activate(foreign_route, second_store)
                .unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );
    }

    #[test]
    fn same_coordinator_tokens_cannot_cross_an_incarnation_replacement() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut old = coordinator.start_incarnation().unwrap();
        let _old_route = old.take_route_sink().unwrap();
        let _old_store = old.take_store_sink().unwrap();
        let cross_route = old.begin_route_baseline().unwrap();
        let old_route = old.begin_route_baseline().unwrap();
        let old_store = old.begin_store_baseline().unwrap();

        let mut current = coordinator.start_incarnation().unwrap();
        let _current_route = current.take_route_sink().unwrap();
        let _current_store = current.take_store_sink().unwrap();
        let current_store = current.begin_store_baseline().unwrap();
        assert_eq!(
            current.activate(cross_route, current_store).unwrap_err(),
            ObserverCoordinatorError::StaleBaseline
        );
        assert_eq!(
            old.activate(old_route, old_store).unwrap_err(),
            ObserverCoordinatorError::StaleIncarnation
        );
    }

    #[test]
    fn replacement_cancels_old_authority_and_stale_callbacks_are_inert() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut old = coordinator.start_incarnation().unwrap();
        let old_route = Arc::new(old.take_route_sink().unwrap());
        let old_store = old.take_store_sink().unwrap();
        let old_epoch = old
            .activate(
                old.begin_route_baseline().unwrap(),
                old.begin_store_baseline().unwrap(),
            )
            .unwrap();
        let old_registration = coordinator.register(&old_epoch).unwrap();

        let mut current = coordinator.start_incarnation().unwrap();
        assert!(!old_registration.is_current());
        let current_route = Arc::new(current.take_route_sink().unwrap());
        let current_store = current.take_store_sink().unwrap();
        let current_epoch = current
            .activate(
                current.begin_route_baseline().unwrap(),
                current.begin_store_baseline().unwrap(),
            )
            .unwrap();
        let current_registration = coordinator.register(&current_epoch).unwrap();

        old_route.invalidate();
        old_route.poison();
        StoreInvalidationSink::invalidate(&old_store);
        drop(old_store);
        drop(old_route);
        drop(old);
        assert!(current_registration.is_current());

        current_route.invalidate();
        assert!(!current_registration.is_current());
        drop(current_store);
    }

    #[test]
    fn dropping_either_current_sink_prevents_reactivation() {
        let coordinator = RoutedObserverCoordinator::new();
        let mut route_drop = coordinator.start_incarnation().unwrap();
        let route = route_drop.take_route_sink().unwrap();
        let _store = route_drop.take_store_sink().unwrap();
        drop(route);
        assert_eq!(
            route_drop.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );

        let mut store_drop = coordinator.start_incarnation().unwrap();
        let _route = store_drop.take_route_sink().unwrap();
        let store = store_drop.take_store_sink().unwrap();
        drop(store);
        assert_eq!(
            store_drop.begin_store_baseline().unwrap_err(),
            ObserverCoordinatorError::SinkUnavailable
        );
    }

    #[test]
    fn every_owner_drop_synchronously_cancels_active_authority() {
        let route_fixture = healthy_fixture();
        let route_registration = route_fixture
            .coordinator
            .register(&route_fixture.epoch)
            .unwrap();
        let route_cancellation = route_registration.cancellation().clone();
        drop(route_fixture.route);
        assert!(route_cancellation.is_cancelled());

        let store_fixture = healthy_fixture();
        let store_registration = store_fixture
            .coordinator
            .register(&store_fixture.epoch)
            .unwrap();
        let store_cancellation = store_registration.cancellation().clone();
        drop(store_fixture.store);
        assert!(store_cancellation.is_cancelled());

        let incarnation_fixture = healthy_fixture();
        let incarnation_registration = incarnation_fixture
            .coordinator
            .register(&incarnation_fixture.epoch)
            .unwrap();
        let incarnation_cancellation = incarnation_registration.cancellation().clone();
        drop(incarnation_fixture.incarnation);
        assert!(incarnation_cancellation.is_cancelled());

        let coordinator_fixture = healthy_fixture();
        let coordinator_registration = coordinator_fixture
            .coordinator
            .register(&coordinator_fixture.epoch)
            .unwrap();
        let coordinator_cancellation = coordinator_registration.cancellation().clone();
        drop(coordinator_fixture.coordinator);
        assert!(coordinator_cancellation.is_cancelled());
    }

    #[test]
    fn dropping_registration_cancels_an_escaped_signal_and_removes_it() {
        let fixture = healthy_fixture();
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        let cancellation = registration.cancellation().clone();
        drop(registration);
        assert!(cancellation.is_cancelled());

        let replacement = fixture.coordinator.register(&fixture.epoch).unwrap();
        assert!(replacement.is_current());
        assert_eq!(
            fixture
                .coordinator
                .inner
                .state
                .lock()
                .unwrap()
                .registrations
                .len(),
            1
        );
    }

    #[test]
    fn registration_and_invalidation_linearize_without_live_authority() {
        for _ in 0..64 {
            let fixture = healthy_fixture();
            let barrier = Arc::new(Barrier::new(3));
            let register_barrier = Arc::clone(&barrier);
            let invalidate_barrier = Arc::clone(&barrier);
            let route = Arc::clone(&fixture.route);

            let registration = thread::scope(|scope| {
                let register = scope.spawn(|| {
                    register_barrier.wait();
                    fixture.coordinator.register(&fixture.epoch)
                });
                let invalidate = scope.spawn(move || {
                    invalidate_barrier.wait();
                    route.invalidate();
                });
                barrier.wait();
                invalidate.join().unwrap();
                register.join().unwrap()
            });

            match registration {
                Ok(registration) => {
                    assert!(!registration.is_current());
                    assert!(registration.cancellation().is_cancelled());
                }
                Err(error) => assert!(matches!(
                    error,
                    ObserverCoordinatorError::Unhealthy | ObserverCoordinatorError::StaleEpoch
                )),
            }
        }
    }

    #[test]
    fn activation_and_invalidation_linearize_without_restored_health() {
        for _ in 0..64 {
            let coordinator = RoutedObserverCoordinator::new();
            let mut incarnation = coordinator.start_incarnation().unwrap();
            let _route = incarnation.take_route_sink().unwrap();
            let _store = incarnation.take_store_sink().unwrap();
            let route = incarnation.begin_route_baseline().unwrap();
            let store = incarnation.begin_store_baseline().unwrap();
            let barrier = Arc::new(Barrier::new(3));

            let outcome = thread::scope(|scope| {
                let activation_barrier = Arc::clone(&barrier);
                let invalidation_barrier = Arc::clone(&barrier);
                let incarnation = &incarnation;
                let activation = scope.spawn(move || {
                    activation_barrier.wait();
                    incarnation.activate(route, store)
                });
                let coordinator = &coordinator;
                let invalidation = scope.spawn(move || {
                    invalidation_barrier.wait();
                    coordinator.invalidate();
                });
                barrier.wait();
                invalidation.join().unwrap();
                activation.join().unwrap()
            });

            match outcome {
                Ok(epoch) => assert!(matches!(
                    coordinator.register(&epoch),
                    Err(ObserverCoordinatorError::Unhealthy | ObserverCoordinatorError::StaleEpoch)
                )),
                Err(error) => assert_eq!(error, ObserverCoordinatorError::StaleBaseline),
            }
        }
    }

    #[test]
    fn counter_exhaustion_fails_closed_instead_of_reusing_authority() {
        let incarnation_coordinator = RoutedObserverCoordinator::new();
        incarnation_coordinator
            .inner
            .state
            .lock()
            .unwrap()
            .next_incarnation = u64::MAX;
        assert_eq!(
            incarnation_coordinator.start_incarnation().unwrap_err(),
            ObserverCoordinatorError::FailedClosed
        );

        let generation_coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = generation_coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();
        let _store = incarnation.take_store_sink().unwrap();
        generation_coordinator
            .inner
            .state
            .lock()
            .unwrap()
            .route_generation = u64::MAX;
        assert_eq!(
            incarnation.begin_route_baseline().unwrap_err(),
            ObserverCoordinatorError::FailedClosed
        );
        assert!(
            generation_coordinator
                .inner
                .state
                .lock()
                .unwrap()
                .failed_closed
        );

        let healthy_epoch_coordinator = RoutedObserverCoordinator::new();
        let mut incarnation = healthy_epoch_coordinator.start_incarnation().unwrap();
        let _route = incarnation.take_route_sink().unwrap();
        let _store = incarnation.take_store_sink().unwrap();
        let route_baseline = incarnation.begin_route_baseline().unwrap();
        let store_baseline = incarnation.begin_store_baseline().unwrap();
        healthy_epoch_coordinator
            .inner
            .state
            .lock()
            .unwrap()
            .next_healthy_epoch = u64::MAX;
        assert_eq!(
            incarnation
                .activate(route_baseline, store_baseline)
                .unwrap_err(),
            ObserverCoordinatorError::FailedClosed
        );

        let registration_fixture = healthy_fixture();
        registration_fixture
            .coordinator
            .inner
            .state
            .lock()
            .unwrap()
            .next_registration = u64::MAX;
        assert_eq!(
            registration_fixture
                .coordinator
                .register(&registration_fixture.epoch)
                .unwrap_err(),
            ObserverCoordinatorError::FailedClosed
        );

        for source in [ObserverSource::Route, ObserverSource::Store] {
            let invalidation_coordinator = RoutedObserverCoordinator::new();
            let _incarnation = invalidation_coordinator.start_incarnation().unwrap();
            let mut state = invalidation_coordinator.inner.state.lock().unwrap();
            match source {
                ObserverSource::Route => state.route_generation = u64::MAX,
                ObserverSource::Store => state.store_generation = u64::MAX,
            }
            drop(state);

            invalidation_coordinator.invalidate();
            assert!(
                invalidation_coordinator
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .failed_closed
            );
        }

        for source in [ObserverSource::Route, ObserverSource::Store] {
            let notification_coordinator = RoutedObserverCoordinator::new();
            let mut incarnation = notification_coordinator.start_incarnation().unwrap();
            let route = incarnation.take_route_sink().unwrap();
            let store = incarnation.take_store_sink().unwrap();
            let mut state = notification_coordinator.inner.state.lock().unwrap();
            match source {
                ObserverSource::Route => state.route_generation = u64::MAX,
                ObserverSource::Store => state.store_generation = u64::MAX,
            }
            drop(state);

            match source {
                ObserverSource::Route => route.invalidate(),
                ObserverSource::Store => StoreInvalidationSink::invalidate(&store),
            }
            assert!(
                notification_coordinator
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .failed_closed
            );
        }
    }

    #[test]
    fn poisoned_state_lock_cancels_authority_and_stays_failed_closed() {
        let fixture = healthy_fixture();
        let registration = fixture.coordinator.register(&fixture.epoch).unwrap();
        let inner = Arc::clone(&fixture.coordinator.inner);

        assert!(
            thread::spawn(move || {
                let _state = inner.state.lock().unwrap();
                panic!("poison coordinator state for the regression fixture");
            })
            .join()
            .is_err()
        );

        fixture.route.invalidate();
        assert!(registration.cancellation().is_cancelled());
        assert!(!registration.is_current());
        assert_eq!(
            fixture.coordinator.start_incarnation().unwrap_err(),
            ObserverCoordinatorError::FailedClosed
        );
    }

    #[test]
    fn debug_and_error_output_are_topology_free() {
        let fixture = healthy_fixture();
        let route_baseline = fixture.incarnation.begin_route_baseline().unwrap();
        let store_baseline = fixture.incarnation.begin_store_baseline().unwrap();
        let error = ObserverCoordinatorError::StaleBaseline;
        let rendered = format!(
            "{:?} {:?} {:?} {:?} {:?} {:?} {error:?} {error}",
            fixture.coordinator,
            fixture.incarnation,
            fixture.route,
            fixture.store,
            route_baseline,
            store_baseline,
        );
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("172.31"));
        assert!(!rendered.contains("wireguard"));
    }
}
