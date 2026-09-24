//! The platform-neutral half of native network-change observation.
//!
//! Each platform watcher subscribes to its operating-system notifications
//! first, reads the interface baseline second, and then hands both to
//! [`deliver_bursts`]. Notifications arrive as a kind recorded in
//! [`EventKinds`] followed by a wake-up on a small channel; every burst of
//! wake-ups is coalesced, the inventory is re-read, and one
//! [`NetworkChange`] naming what each interface lost is sent.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio::sync::mpsc;

use super::{Coalesced, InterfaceInventory, NetworkChange, coalesce_burst};

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
}

/// The watcher holds no discovery authority, so notifications have nothing
/// to invalidate; they only feed the coalesced change stream. It records
/// whether anything other than an address notification arrived, so a burst
/// of address-lifetime refreshes can be recognized.
#[derive(Debug, Default)]
pub(super) struct EventKinds {
    beyond_addresses: AtomicBool,
}

impl EventKinds {
    /// Record one notification. A platform records the kind before it wakes
    /// the watcher, so every notification of a delivered burst is counted.
    pub(super) fn record(&self, kind: ChangeKind) {
        if kind != ChangeKind::Address {
            self.beyond_addresses.store(true, Ordering::Release);
        }
    }

    /// Whether a link, route, or rule notification arrived since the last
    /// call.
    pub(super) fn take_beyond_addresses(&self) -> bool {
        self.beyond_addresses.swap(false, Ordering::AcqRel)
    }
}

/// Whether a delivered burst can matter to discovery evidence or routed
/// authority. Routers refresh address lifetimes every few seconds on
/// many IPv6 networks; a burst of only address notifications that left
/// every interface and address unchanged is such a refresh and is
/// dropped. Link, route, and rule notifications are always delivered.
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
) -> Result<(), NetworkChangeWatchError>
where
    R: Fn() -> io::Result<InterfaceInventory> + Clone + Send + 'static,
{
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
        let burst = tokio::select! {
            biased;
            () = changes.closed() => return Ok(()),
            burst = coalesce_burst(events, |_, _| {}) => burst,
        };
        let Some(Coalesced { count, .. }) = burst else {
            return Err(NetworkChangeWatchError::MonitorStopped);
        };
        // Take the kinds before reading the inventory: a notification
        // after this point belongs to the next burst, and the state it
        // reports is already visible to the read below.
        let beyond_addresses = kinds.take_beyond_addresses();
        let lost = match tokio::task::spawn_blocking(read_inventory.clone()).await {
            Ok(Ok(latest)) => {
                if !burst_matters(&current, &latest, beyond_addresses) {
                    continue;
                }
                let lost = current.loss_since(&latest);
                current = latest;
                *inventory = Some(current.clone());
                lost
            }
            // The change is still real; authority is cancelled even
            // when nothing can be attributed.
            Ok(Err(_)) | Err(_) => BTreeMap::new(),
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

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
    fn only_link_and_route_kinds_go_beyond_addresses() {
        let kinds = EventKinds::default();
        kinds.record(ChangeKind::Address);
        kinds.record(ChangeKind::Address);
        assert!(!kinds.take_beyond_addresses());

        for kind in [ChangeKind::Link, ChangeKind::Route] {
            kinds.record(ChangeKind::Address);
            kinds.record(kind);
            assert!(kinds.take_beyond_addresses(), "{kind:?}");
            // Taking clears the record for the next burst.
            assert!(!kinds.take_beyond_addresses());
        }
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
    async fn a_first_baseline_is_recorded_without_a_change() {
        let (changes, mut delivered) = mpsc::channel(4);
        let (signal, mut events) = mpsc::channel::<()>(1);
        drop(signal);
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        let mut known = None;

        let outcome = deliver_bursts(
            &changes,
            &mut known,
            baseline.clone(),
            &EventKinds::default(),
            &mut events,
            scripted([]),
        )
        .await;

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
        assert_eq!(known, Some(baseline));
        assert!(delivered.try_recv().is_err());
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
    async fn bursts_are_filtered_diffed_and_counted() {
        let (changes, mut delivered) = mpsc::channel(8);
        let (signal, mut events) = mpsc::channel::<()>(8);
        let kinds = Arc::new(EventKinds::default());
        let baseline = inventory(&[("eth0", "192.0.2.10"), ("eth1", "198.51.100.7")]);
        let without_eth1 = inventory(&[("eth0", "192.0.2.10")]);
        let mut known = None;
        let reads = scripted([
            // An address-only burst that changed nothing is a refresh.
            Ok(baseline.clone()),
            // A route burst is delivered even though nothing was lost.
            Ok(baseline.clone()),
            // An address burst that removed an interface is delivered.
            Ok(without_eth1.clone()),
            // A failed read still delivers an unattributed change.
            Err(io::Error::other("unreadable")),
        ]);

        let feeder = {
            let kinds = Arc::clone(&kinds);
            tokio::spawn(async move {
                let quiet = crate::discovery::NETWORK_CHANGE_QUIET_PERIOD * 2;
                for burst in [
                    &[ChangeKind::Address, ChangeKind::Address][..],
                    &[ChangeKind::Address, ChangeKind::Route, ChangeKind::Link][..],
                    &[ChangeKind::Address][..],
                    &[ChangeKind::Link][..],
                ] {
                    for kind in burst {
                        kinds.record(*kind);
                        signal.send(()).await.unwrap();
                    }
                    tokio::time::sleep(quiet).await;
                }
            })
        };

        let outcome =
            deliver_bursts(&changes, &mut known, baseline, &kinds, &mut events, reads).await;
        feeder.await.unwrap();

        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorStopped));
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
    async fn a_closed_change_stream_ends_delivery_cleanly() {
        let (changes, delivered) = mpsc::channel(1);
        let (_signal, mut events) = mpsc::channel::<()>(1);
        drop(delivered);
        let baseline = inventory(&[("eth0", "192.0.2.10")]);

        // While idle.
        let mut known = None;
        let idle = deliver_bursts(
            &changes,
            &mut known,
            baseline.clone(),
            &EventKinds::default(),
            &mut events,
            scripted([]),
        )
        .await;
        assert_eq!(idle, Ok(()));
        assert_eq!(known.as_ref(), Some(&baseline));

        // While owing a change after resubscription.
        let resumed = deliver_bursts(
            &changes,
            &mut known,
            inventory(&[]),
            &EventKinds::default(),
            &mut events,
            scripted([]),
        )
        .await;
        assert_eq!(resumed, Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_that_cannot_be_sent_ends_delivery_cleanly() {
        let (changes, delivered) = mpsc::channel(1);
        let (signal, mut events) = mpsc::channel::<()>(1);
        let kinds = EventKinds::default();
        let baseline = inventory(&[("eth0", "192.0.2.10")]);
        // A full stream holds the burst's change until the controller
        // stops listening.
        changes.try_send(NetworkChange::default()).unwrap();
        kinds.record(ChangeKind::Link);
        signal.send(()).await.unwrap();
        let closer = tokio::spawn(async move {
            tokio::time::sleep(crate::discovery::NETWORK_CHANGE_MAX_DELAY * 2).await;
            drop(delivered);
        });

        let outcome = deliver_bursts(
            &changes,
            &mut None,
            baseline.clone(),
            &kinds,
            &mut events,
            scripted([Ok(baseline)]),
        )
        .await;
        closer.await.unwrap();

        assert_eq!(outcome, Ok(()));
        drop(signal);
    }
}
