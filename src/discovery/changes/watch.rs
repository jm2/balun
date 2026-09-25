//! The platform-neutral half of native network-change observation.
//!
//! Each platform watcher subscribes to its operating-system notifications
//! first, reads the interface baseline second, and then hands both to
//! [`deliver_bursts`]. Notifications arrive as a kind recorded in
//! [`EventKinds`] followed by a wake-up on a small channel; every burst of
//! wake-ups is coalesced, the inventory is re-read, and one
//! [`NetworkChange`] naming what each interface lost is sent.
//!
//! Beside the debounced changes, the watcher publishes whether observation
//! is healthy through an [`ObservationGate`]. A link or route notification
//! revokes readiness where it is recorded, and an address notification does
//! so as soon as a re-read shows the inventory changed, so the debounce
//! never delays revocation. Readiness returns, as a new generation, only
//! once the burst is reconciled and no notification is pending.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::observation::ObservationGate;
use super::{
    InterfaceInventory, NETWORK_CHANGE_MAX_DELAY, NETWORK_CHANGE_QUIET_PERIOD, NetworkChange,
};

/// A topology-redacted reason one observation attempt ended.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NetworkChangeWatchError {
    #[error("the network-change watcher needs a Tokio runtime with I/O")]
    RuntimeUnavailable,
    #[error("the network-change subscription could not be established")]
    MonitorUnavailable,
    #[error("the network changed while its baseline was being taken")]
    BaselineChanged,
    #[error("the interface inventory could not be read")]
    InventoryUnavailable,
    #[error("the network-change subscription stopped")]
    MonitorStopped,
}

/// What one raw operating-system notification reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChangeKind {
    /// An interface appeared, disappeared, or changed state.
    Link,
    /// An interface address was added, removed, or refreshed.
    Address,
    /// A route or routing rule changed.
    Route,
    /// A parameter of an existing interface or route was updated, such as a
    /// lifetime refreshed by a router advertisement. Like an address
    /// notification, it matters only when the inventory changed.
    #[cfg(any(windows, test))]
    Refresh,
}

/// Records whether anything other than an address notification arrived, so
/// a burst of address-lifetime refreshes can be recognized, and revokes
/// observation readiness at once for a link or route notification, which is
/// always a change.
#[derive(Debug, Default)]
pub(super) struct EventKinds {
    beyond_addresses: AtomicBool,
    gate: Option<ObservationGate>,
}

impl EventKinds {
    /// Kinds that revoke `gate` the moment a link or route notification is
    /// recorded.
    pub(super) fn revoking(gate: ObservationGate) -> Self {
        Self {
            beyond_addresses: AtomicBool::new(false),
            gate: Some(gate),
        }
    }

    /// Record one notification. A platform records the kind before it wakes
    /// the watcher, so every notification of a delivered burst is counted.
    pub(super) fn record(&self, kind: ChangeKind) {
        if matches!(kind, ChangeKind::Link | ChangeKind::Route) {
            self.beyond_addresses.store(true, Ordering::Release);
            self.revoke();
        }
    }

    /// Revoke readiness: a change was detected or observation failed.
    pub(super) fn revoke(&self) {
        if let Some(gate) = &self.gate {
            gate.invalidate();
        }
    }

    /// Whether a link, route, or rule notification arrived since the last
    /// call.
    pub(super) fn take_beyond_addresses(&self) -> bool {
        self.beyond_addresses.swap(false, Ordering::AcqRel)
    }
}

/// Whether a delivered burst can matter to discovery evidence. Routers
/// refresh address lifetimes every few seconds on many IPv6 networks; a
/// burst of only address notifications that left every interface and
/// address unchanged is such a refresh and is dropped. Link, route, and rule
/// notifications are always delivered.
pub(super) fn burst_matters(
    previous: &InterfaceInventory,
    latest: &InterfaceInventory,
    beyond_addresses: bool,
) -> bool {
    beyond_addresses || previous != latest
}

/// Deliver changes from an established subscription and baseline.
///
/// `inventory` carries the last known interfaces across attempts: when it is
/// `Some`, the previous attempt ended and events may have been missed, so one
/// change is sent as soon as `current`, the new baseline, is recorded. Every
/// later burst on `events` is coalesced, `read_inventory` is called off the
/// runtime thread, and one change is sent unless [`burst_matters`] drops it.
///
/// `gate` is declared ready whenever the baseline is reconciled and no
/// notification is pending, revoked as soon as a burst is known to be a
/// change, and left unavailable when this returns.
///
/// Returns `Ok` once `changes` closes and
/// [`NetworkChangeWatchError::MonitorStopped`] once `events` ends. On return
/// `inventory` holds the latest baseline.
pub(super) async fn deliver_bursts<T, R>(
    changes: &mpsc::Sender<NetworkChange>,
    inventory: &mut Option<InterfaceInventory>,
    mut current: InterfaceInventory,
    kinds: &EventKinds,
    events: &mut mpsc::Receiver<T>,
    read_inventory: R,
    gate: &ObservationGate,
) -> Result<(), NetworkChangeWatchError>
where
    R: Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static,
{
    let _unavailable = UnavailableOnReturn(gate);
    if let Some(previous) = inventory.replace(current.clone()) {
        let lost = previous.loss_since(&current);
        if changes
            .send(NetworkChange::coalesced(lost, 1))
            .await
            .is_err()
        {
            return Ok(());
        }
    }

    loop {
        // Reconcile a notification already queued before declaring the
        // baseline healthy; a queued one starts the next burst instead.
        if events.is_empty() {
            gate.establish();
        }
        let burst = tokio::select! {
            biased;
            () = changes.closed() => return Ok(()),
            burst = next_burst(events, gate, &current, &read_inventory) => burst,
        };
        let Some(count) = burst else {
            return Err(NetworkChangeWatchError::MonitorStopped);
        };
        // Take the kinds before reading the inventory: a notification
        // after this point belongs to the next burst, and the state it
        // reports is already visible to the read below.
        let beyond_addresses = kinds.take_beyond_addresses();
        let lost = match read_off_runtime(&read_inventory).await {
            Some(latest) => {
                if !burst_matters(&current, &latest, beyond_addresses) {
                    continue;
                }
                gate.invalidate();
                let lost = current.loss_since(&latest);
                current = latest;
                *inventory = Some(current.clone());
                lost
            }
            // The change is still real; authority is cancelled even
            // when nothing can be attributed.
            None => {
                gate.invalidate();
                BTreeMap::new()
            }
        };
        if changes
            .send(NetworkChange::coalesced(lost, count))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

/// Wait for the next burst and return how many wake-ups it held, or `None`
/// once `events` is closed and drained.
///
/// The burst opens with the first wake-up and closes after
/// [`NETWORK_CHANGE_QUIET_PERIOD`] without another one, or at
/// [`NETWORK_CHANGE_MAX_DELAY`] after it opened. While the gate is still
/// ready, every wake-up re-reads the inventory at once, so an address change
/// revokes readiness on detection rather than when the burst closes; a
/// refresh that changed nothing leaves it ready.
async fn next_burst<T, R>(
    events: &mut mpsc::Receiver<T>,
    gate: &ObservationGate,
    current: &InterfaceInventory,
    read_inventory: &R,
) -> Option<usize>
where
    R: Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static,
{
    events.recv().await?;
    revoke_if_changed(gate, current, read_inventory).await;
    let mut count = 1;
    let deadline = Instant::now() + NETWORK_CHANGE_MAX_DELAY;
    loop {
        let quiet = tokio::time::sleep(NETWORK_CHANGE_QUIET_PERIOD);
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => break,
            () = quiet => break,
            next = events.recv() => match next {
                Some(_) => {
                    count += 1;
                    revoke_if_changed(gate, current, read_inventory).await;
                }
                None => break,
            },
        }
    }
    Some(count)
}

/// Revoke a still-ready gate when the inventory no longer matches the
/// baseline or cannot be read.
async fn revoke_if_changed<R>(gate: &ObservationGate, current: &InterfaceInventory, read: &R)
where
    R: Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static,
{
    if gate.state().generation().is_none() {
        return;
    }
    if read_off_runtime(read).await.as_ref() != Some(current) {
        gate.invalidate();
    }
}

/// Read the inventory on the blocking pool; `None` when it cannot be read.
async fn read_off_runtime<R>(read: &R) -> Option<InterfaceInventory>
where
    R: Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static,
{
    match tokio::task::spawn_blocking(read.clone()).await {
        Ok(Ok(latest)) => Some(latest),
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Leaves the gate unavailable however an observation attempt returns.
struct UnavailableOnReturn<'a>(&'a ObservationGate);

impl Drop for UnavailableOnReturn<'_> {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::observation::{ObservationGeneration, ObservationState};
    use super::*;

    fn inventory(entries: &[(&str, &str)]) -> InterfaceInventory {
        InterfaceInventory::from_addresses(
            entries
                .iter()
                .map(|(name, ip)| ((*name).to_owned(), ip.parse().unwrap())),
        )
    }

    /// An inventory reader that replays scripted reads, then fails.
    fn scripted(
        reads: impl IntoIterator<Item = io::Result<InterfaceInventory>>,
    ) -> impl Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static {
        let reads = Arc::new(Mutex::new(reads.into_iter().collect::<VecDeque<_>>()));
        move || {
            reads
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("no scripted read")))
        }
    }

    /// The system's interfaces as a test sets them; `None` cannot be read.
    type LiveInventory = Arc<Mutex<Option<InterfaceInventory>>>;

    fn live(
        system: &LiveInventory,
    ) -> impl Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static {
        let system = Arc::clone(system);
        move || {
            system
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| io::Error::other("unreadable"))
        }
    }

    fn generation(value: u64) -> ObservationState {
        ObservationState::Ready(ObservationGeneration::new(value).unwrap())
    }

    #[test]
    fn address_lifetime_refreshes_are_not_changes() {
        let before = inventory(&[("eth0", "192.0.2.10"), ("eth0", "2001:db8::10")]);
        let same = before.clone();
        let gained = inventory(&[
            ("eth0", "192.0.2.10"),
            ("eth0", "2001:db8::10"),
            ("eth0", "2001:db8::11"),
        ]);

        // Only address notifications, and nothing changed: a refresh.
        assert!(!burst_matters(&before, &same, false));
        // Any added or removed address is delivered.
        assert!(burst_matters(&before, &gained, false));
        assert!(burst_matters(&gained, &before, false));
        // Link, route, and rule notifications are always delivered even
        // though the interface inventory cannot show what they changed.
        assert!(burst_matters(&before, &same, true));
    }

    #[test]
    fn only_link_and_route_kinds_go_beyond_addresses_and_revoke_at_once() {
        let gate = ObservationGate::new();
        let kinds = EventKinds::revoking(gate.clone());
        gate.establish();
        kinds.record(ChangeKind::Address);
        kinds.record(ChangeKind::Refresh);
        assert!(!kinds.take_beyond_addresses());
        assert_eq!(gate.state(), generation(1), "addresses need a re-read");

        for kind in [ChangeKind::Link, ChangeKind::Route] {
            gate.establish();
            kinds.record(ChangeKind::Address);
            kinds.record(kind);
            assert_eq!(gate.state(), ObservationState::Unavailable, "{kind:?}");
            assert!(kinds.take_beyond_addresses(), "{kind:?}");
            // Taking clears the record for the next burst.
            assert!(!kinds.take_beyond_addresses());
        }
        // Kinds without a gate only classify.
        EventKinds::default().record(ChangeKind::Link);
    }

    #[test]
    fn errors_are_topology_free() {
        for error in [
            NetworkChangeWatchError::RuntimeUnavailable,
            NetworkChangeWatchError::MonitorUnavailable,
            NetworkChangeWatchError::BaselineChanged,
            NetworkChangeWatchError::InventoryUnavailable,
            NetworkChangeWatchError::MonitorStopped,
        ] {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("eth"));
            assert!(!rendered.contains("wg"));
            assert!(!rendered.contains('.'));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_first_baseline_is_ready_without_a_change_and_unavailable_once_stopped() {
        let (changes, mut delivered) = mpsc::channel(4);
        let (signal, mut events) = mpsc::channel::<()>(1);
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        let gate = ObservationGate::new();
        let mut watch = gate.watch();
        let mut known = None;

        let observed = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let ready = watch.changed().await;
                // Readiness exists only while the attempt runs.
                assert_eq!(gate.state(), ready);
                drop(signal);
                watch.changed().await;
                (ready, watch.current())
            })
        };
        let outcome = deliver_bursts(
            &changes,
            &mut known,
            baseline.clone(),
            &EventKinds::default(),
            &mut events,
            scripted([]),
            &gate,
        )
        .await;

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert_eq!(known, Some(baseline));
        assert!(delivered.try_recv().is_err());
        assert_eq!(
            observed.await.unwrap(),
            (generation(1), ObservationState::Unavailable)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_resumed_observation_reports_what_was_lost_while_it_was_down() {
        let (changes, mut delivered) = mpsc::channel(4);
        let (signal, mut events) = mpsc::channel::<()>(1);
        drop(signal);
        let before = inventory(&[("eth0", "192.0.2.10"), ("wg0", "10.250.0.2")]);
        let after = inventory(&[("eth0", "192.0.2.10")]);
        let mut known = Some(before);

        let outcome = deliver_bursts(
            &changes,
            &mut known,
            after.clone(),
            &EventKinds::default(),
            &mut events,
            scripted([]),
            &ObservationGate::new(),
        )
        .await;

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert_eq!(known, Some(after));
        let change = delivered.try_recv().unwrap();
        assert_eq!(change.coalesced_count(), 1);
        assert_eq!(
            change
                .lost_interfaces()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["wg0"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_notification_queued_with_the_baseline_is_reconciled_before_readiness() {
        let (changes, mut delivered) = mpsc::channel(4);
        let (signal, mut events) = mpsc::channel::<()>(1);
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        let gate = ObservationGate::new();
        let kinds = EventKinds::revoking(gate.clone());
        kinds.record(ChangeKind::Link);
        signal.send(()).await.unwrap();
        let states = Arc::new(Mutex::new(Vec::new()));
        let recorder = {
            let states = Arc::clone(&states);
            let mut watch = gate.watch();
            tokio::spawn(async move {
                loop {
                    let state = watch.changed().await;
                    states.lock().unwrap().push(state);
                    if state == ObservationState::Unavailable && states.lock().unwrap().len() > 1 {
                        return;
                    }
                }
            })
        };
        let closer = tokio::spawn(async move {
            tokio::time::sleep(NETWORK_CHANGE_MAX_DELAY * 2).await;
            drop(signal);
        });

        let outcome = deliver_bursts(
            &changes,
            &mut None,
            baseline.clone(),
            &kinds,
            &mut events,
            scripted([Ok(baseline)]),
            &gate,
        )
        .await;
        closer.await.unwrap();
        recorder.await.unwrap();

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert!(delivered.try_recv().is_ok(), "the queued link change");
        // The first readiness follows the queued burst's reconciliation.
        assert_eq!(
            *states.lock().unwrap(),
            [generation(1), ObservationState::Unavailable]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bursts_are_filtered_diffed_counted_and_revoked_on_detection() {
        let (changes, mut delivered) = mpsc::channel(8);
        let (signal, mut events) = mpsc::channel::<()>(8);
        let gate = ObservationGate::new();
        let kinds = Arc::new(EventKinds::revoking(gate.clone()));
        let baseline = inventory(&[("eth0", "192.0.2.10"), ("eth1", "198.51.100.7")]);
        let without_eth1 = inventory(&[("eth0", "192.0.2.10")]);
        let system: LiveInventory = Arc::new(Mutex::new(Some(baseline.clone())));
        let mut known = None;

        let feeder = {
            let kinds = Arc::clone(&kinds);
            let gate = gate.clone();
            let system = Arc::clone(&system);
            let without_eth1 = without_eth1.clone();
            tokio::spawn(async move {
                let quiet = NETWORK_CHANGE_QUIET_PERIOD * 2;
                let mut seen = Vec::new();
                let bursts: [&[ChangeKind]; 4] = [
                    // An address-only burst that changed nothing is a refresh.
                    &[ChangeKind::Address, ChangeKind::Address],
                    // A route burst is delivered even though nothing was lost.
                    &[ChangeKind::Address, ChangeKind::Route, ChangeKind::Link],
                    // An address burst that removed an interface is delivered.
                    &[ChangeKind::Address],
                    // A failed read still delivers an unattributed change.
                    &[ChangeKind::Link],
                ];
                for (index, burst) in bursts.into_iter().enumerate() {
                    match index {
                        2 => *system.lock().unwrap() = Some(without_eth1.clone()),
                        3 => *system.lock().unwrap() = None,
                        _ => {}
                    }
                    for kind in burst {
                        kinds.record(*kind);
                        signal.send(()).await.unwrap();
                    }
                    // Revocation happens on detection, long before the
                    // burst's quiet period closes it.
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    seen.push(gate.state());
                    tokio::time::sleep(quiet).await;
                    seen.push(gate.state());
                }
                seen
            })
        };

        let outcome = deliver_bursts(
            &changes,
            &mut known,
            baseline,
            &kinds,
            &mut events,
            live(&system),
            &gate,
        )
        .await;
        let seen = feeder.await.unwrap();

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert_eq!(
            seen,
            [
                // The refresh leaves the first generation in place.
                generation(1),
                generation(1),
                // Each change revokes at once and returns as a new generation.
                ObservationState::Unavailable,
                generation(2),
                ObservationState::Unavailable,
                generation(3),
                ObservationState::Unavailable,
                generation(4),
            ]
        );
        assert_eq!(gate.state(), ObservationState::Unavailable);
        let route = delivered.try_recv().unwrap();
        assert_eq!(route.coalesced_count(), 3);
        assert!(route.lost_interfaces().is_empty());
        let removal = delivered.try_recv().unwrap();
        assert_eq!(removal.coalesced_count(), 1);
        assert!(removal.loss("eth1").ipv4());
        let unreadable = delivered.try_recv().unwrap();
        assert!(unreadable.lost_interfaces().is_empty());
        assert!(delivered.try_recv().is_err());
        // The failed read kept the last good baseline.
        assert_eq!(known, Some(without_eth1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_transient_address_change_inside_one_burst_still_needs_a_new_generation() {
        let (changes, mut delivered) = mpsc::channel(4);
        let (signal, mut events) = mpsc::channel::<()>(4);
        let gate = ObservationGate::new();
        let kinds = EventKinds::revoking(gate.clone());
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        let system: LiveInventory = Arc::new(Mutex::new(Some(baseline.clone())));
        let feeder = {
            let system = Arc::clone(&system);
            let gate = gate.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let first = gate.state();
                // The address disappears and returns within one burst.
                *system.lock().unwrap() = Some(inventory(&[]));
                signal.send(()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                let revoked = gate.state();
                *system.lock().unwrap() = Some(inventory(&[("eth0", "192.0.2.10")]));
                signal.send(()).await.unwrap();
                tokio::time::sleep(NETWORK_CHANGE_MAX_DELAY).await;
                (first, revoked, gate.state())
            })
        };

        let outcome = deliver_bursts(
            &changes,
            &mut None,
            baseline,
            &kinds,
            &mut events,
            live(&system),
            &gate,
        )
        .await;

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert_eq!(
            feeder.await.unwrap(),
            (generation(1), ObservationState::Unavailable, generation(2))
        );
        // The inventory ended where it began, so nothing was delivered.
        assert!(delivered.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_change_stream_ends_delivery_cleanly() {
        let (changes, delivered) = mpsc::channel(1);
        let (_signal, mut events) = mpsc::channel::<()>(1);
        drop(delivered);
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        let gate = ObservationGate::new();

        // While idle.
        let mut known = None;
        let idle = deliver_bursts(
            &changes,
            &mut known,
            baseline.clone(),
            &EventKinds::default(),
            &mut events,
            scripted([]),
            &gate,
        )
        .await;
        assert_eq!(idle, Ok(()));
        assert_eq!(known.as_ref(), Some(&baseline));
        assert_eq!(gate.state(), ObservationState::Unavailable);

        // While owing a change after resubscription.
        let resumed = deliver_bursts(
            &changes,
            &mut known,
            inventory(&[]),
            &EventKinds::default(),
            &mut events,
            scripted([]),
            &gate,
        )
        .await;
        assert_eq!(resumed, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_that_cannot_be_sent_ends_delivery_cleanly() {
        let (changes, delivered) = mpsc::channel(1);
        let (signal, mut events) = mpsc::channel::<()>(1);
        let gate = ObservationGate::new();
        let kinds = EventKinds::revoking(gate.clone());
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        // A full stream holds the burst's change until the controller
        // stops listening.
        changes.try_send(NetworkChange::default()).unwrap();
        kinds.record(ChangeKind::Link);
        signal.send(()).await.unwrap();
        let closer = tokio::spawn(async move {
            tokio::time::sleep(NETWORK_CHANGE_MAX_DELAY * 2).await;
            drop(delivered);
        });

        let outcome = deliver_bursts(
            &changes,
            &mut None,
            baseline.clone(),
            &kinds,
            &mut events,
            scripted([Ok(baseline)]),
            &gate,
        )
        .await;
        closer.await.unwrap();

        assert_eq!(outcome, Ok(()));
        assert_eq!(
            gate.state(),
            ObservationState::Unavailable,
            "readiness never returned while the change was undelivered"
        );
        drop(signal);
    }
}
