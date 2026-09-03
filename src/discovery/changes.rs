//! Debounced network-change observation.
//!
//! A network change is any adapter, address, or route event the platform can
//! report. Bursts are coalesced so one reconciliation runs per burst, and the
//! only thing a coalesced change carries is the set of interface names that
//! disappeared, went down, or lost an address since the previous observation.
//! Nothing here sends a packet, and no value defined here enters a snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

/// Quiet period after the last raw notification before a burst is delivered.
pub const NETWORK_CHANGE_QUIET_PERIOD: Duration = Duration::from_millis(500);
/// Longest a continuing burst may be held before it is delivered anyway.
pub const NETWORK_CHANGE_MAX_DELAY: Duration = Duration::from_secs(2);

/// One coalesced network change, as the controller reconciles it.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct NetworkChange {
    lost_interfaces: BTreeSet<String>,
    coalesced: usize,
}

impl NetworkChange {
    /// A change that lost exactly these interfaces.
    #[must_use]
    pub fn new(lost_interfaces: impl IntoIterator<Item = String>) -> Self {
        Self {
            lost_interfaces: lost_interfaces.into_iter().collect(),
            coalesced: 1,
        }
    }

    /// A change that stands for `coalesced` raw notifications.
    #[must_use]
    pub fn coalesced(lost_interfaces: BTreeSet<String>, coalesced: usize) -> Self {
        Self {
            lost_interfaces,
            coalesced: coalesced.max(1),
        }
    }

    /// Interfaces that disappeared, went down, or lost an address. Evidence
    /// observed through them is stale; these names never enter a snapshot.
    #[must_use]
    pub const fn lost_interfaces(&self) -> &BTreeSet<String> {
        &self.lost_interfaces
    }

    /// How many raw notifications this change stands for.
    #[must_use]
    pub const fn coalesced_count(&self) -> usize {
        self.coalesced
    }

    /// Fold a later change into this one.
    pub fn merge(&mut self, later: Self) {
        self.lost_interfaces.extend(later.lost_interfaces);
        self.coalesced = self.coalesced.saturating_add(later.coalesced);
    }
}

impl fmt::Debug for NetworkChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkChange")
            .field("lost_interface_count", &self.lost_interfaces.len())
            .field("coalesced", &self.coalesced)
            .finish()
    }
}

/// One delivered burst of raw notifications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Coalesced<T> {
    /// Every notification of the burst folded into one value.
    pub value: T,
    /// How many notifications the burst contained.
    pub count: usize,
}

/// Wait for the next burst on `receiver` and deliver it once.
///
/// The burst opens with the first notification and closes after
/// [`NETWORK_CHANGE_QUIET_PERIOD`] without another one, or at
/// [`NETWORK_CHANGE_MAX_DELAY`] after it opened, whichever comes first.
/// Returns `None` once the channel is closed and drained.
pub async fn coalesce_burst<T>(
    receiver: &mut mpsc::Receiver<T>,
    mut merge: impl FnMut(&mut T, T),
) -> Option<Coalesced<T>> {
    let mut value = receiver.recv().await?;
    let mut count = 1;
    let deadline = Instant::now() + NETWORK_CHANGE_MAX_DELAY;
    loop {
        let quiet = tokio::time::sleep(NETWORK_CHANGE_QUIET_PERIOD);
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => break,
            () = quiet => break,
            next = receiver.recv() => match next {
                Some(next) => {
                    merge(&mut value, next);
                    count += 1;
                }
                None => break,
            },
        }
    }
    Some(Coalesced { value, count })
}

/// The up, non-loopback interfaces and their addresses at one moment.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct InterfaceInventory {
    interfaces: BTreeMap<String, BTreeSet<IpAddr>>,
}

impl InterfaceInventory {
    /// Build an inventory from `(interface name, address)` pairs.
    #[must_use]
    pub fn from_addresses(addresses: impl IntoIterator<Item = (String, IpAddr)>) -> Self {
        let mut interfaces: BTreeMap<String, BTreeSet<IpAddr>> = BTreeMap::new();
        for (name, address) in addresses {
            interfaces.entry(name).or_default().insert(address);
        }
        Self { interfaces }
    }

    /// Enumerate the system's up, non-loopback interfaces. Tunnels are
    /// included so a lost tunnel can expire routed evidence.
    pub fn current() -> io::Result<Self> {
        let interfaces = if_addrs::get_if_addrs()?;
        Ok(Self::from_addresses(
            interfaces
                .into_iter()
                .filter(|interface| interface.is_oper_up() && !interface.is_loopback())
                .map(|interface| {
                    let address = interface.ip();
                    (interface.name, address)
                }),
        ))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.interfaces.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty()
    }

    /// Interfaces present here that are gone, down, or missing one of their
    /// addresses in `later`. An interface that only gained addresses is kept.
    #[must_use]
    pub fn lost_since(&self, later: &Self) -> BTreeSet<String> {
        self.interfaces
            .iter()
            .filter(|(name, addresses)| {
                later
                    .interfaces
                    .get(*name)
                    .is_none_or(|current| !addresses.is_subset(current))
            })
            .map(|(name, _)| name.clone())
            .collect()
    }
}

impl fmt::Debug for InterfaceInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterfaceInventory")
            .field("interface_count", &self.interfaces.len())
            .finish()
    }
}

#[cfg(target_os = "linux")]
pub use linux::{LinuxNetworkChangeWatcher, NetworkChangeWatchError};

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use thiserror::Error;
    use tokio::runtime::Handle;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::task::AbortOnDropHandle;

    use super::{Coalesced, InterfaceInventory, NetworkChange, coalesce_burst};
    use crate::discovery::routes::{
        LinuxRouteEventMonitor, LinuxRouteMonitorError, RouteMonitorObserver,
    };

    /// A topology-redacted reason one observation attempt ended.
    #[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
    pub enum NetworkChangeWatchError {
        #[error("the network-change watcher needs a Tokio runtime with I/O")]
        RuntimeUnavailable,
        #[error("the Linux route-event subscription could not be established")]
        MonitorUnavailable,
        #[error("the network changed while its baseline was being taken")]
        BaselineChanged,
        #[error("the interface inventory could not be read")]
        InventoryUnavailable,
        #[error("the Linux route-event monitor stopped")]
        MonitorStopped,
    }

    /// The watcher holds no discovery authority, so route events have nothing
    /// to invalidate or poison; they only feed the coalesced change stream.
    struct InertObserver;

    impl RouteMonitorObserver for InertObserver {
        fn invalidate(&self) {}

        fn poison(&self) {}
    }

    /// Debounced Linux network-change observation over rtnetlink.
    ///
    /// The rtnetlink subscription is taken first and the interface baseline is
    /// read inside the monitor's synchronous activation callback, after its
    /// final drain to `EAGAIN`, so no change can fall between the two. Every
    /// later burst is coalesced, the inventory is diffed, and one
    /// [`NetworkChange`] naming the lost interfaces is sent.
    pub struct LinuxNetworkChangeWatcher;

    impl LinuxNetworkChangeWatcher {
        /// Observe until `changes` closes (`Ok`) or the observation ends.
        ///
        /// `inventory` carries the last known interfaces across attempts: when
        /// it is `Some`, the previous attempt ended and events may have been
        /// missed, so one change is sent as soon as the new baseline exists.
        /// On return it holds the latest baseline this attempt established.
        pub async fn observe(
            changes: &mpsc::Sender<NetworkChange>,
            inventory: &mut Option<InterfaceInventory>,
        ) -> Result<(), NetworkChangeWatchError> {
            let runtime =
                Handle::try_current().map_err(|_| NetworkChangeWatchError::RuntimeUnavailable)?;
            let (monitor, mut reconciliation) =
                LinuxRouteEventMonitor::subscribe(Arc::new(InertObserver))
                    .map_err(|_| NetworkChangeWatchError::MonitorUnavailable)?;
            let (baseline, baseline_receiver) = oneshot::channel();
            let monitor_task =
                AbortOnDropHandle::new(runtime.spawn(monitor.run_continuously(move || {
                    let inventory = InterfaceInventory::current().map_err(|_| ())?;
                    baseline.send(inventory).map_err(|_| ())
                })));

            let mut current = match baseline_receiver.await {
                Ok(inventory) => inventory,
                Err(_) => {
                    return Err(match monitor_task.await {
                        Ok(Err(error)) if is_inventory_failure(error) => {
                            NetworkChangeWatchError::InventoryUnavailable
                        }
                        _ => NetworkChangeWatchError::BaselineChanged,
                    });
                }
            };
            if let Some(previous) = inventory.replace(current.clone()) {
                let lost = previous.lost_since(&current);
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
                    burst = coalesce_burst(&mut reconciliation, |_, _| {}) => burst,
                };
                let Some(Coalesced { count, .. }) = burst else {
                    break;
                };
                let lost = match tokio::task::spawn_blocking(InterfaceInventory::current).await {
                    Ok(Ok(latest)) => {
                        let lost = current.lost_since(&latest);
                        current = latest;
                        *inventory = Some(current.clone());
                        lost
                    }
                    // The change is still real; authority is cancelled even
                    // when nothing can be attributed.
                    Ok(Err(_)) | Err(_) => BTreeSet::new(),
                };
                if changes
                    .send(NetworkChange::coalesced(lost, count))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            let _ = monitor_task.await;
            Err(NetworkChangeWatchError::MonitorStopped)
        }
    }

    /// The activation callback runs only after a clean barrier and fails only
    /// when the inventory cannot be read (its receiver is awaited above), so a
    /// rejected activation is an inventory failure; every other early end is
    /// a change during the baseline.
    const fn is_inventory_failure(error: LinuxRouteMonitorError) -> bool {
        matches!(error, LinuxRouteMonitorError::ActivationRejected)
    }

    #[cfg(test)]
    mod tests {
        use tokio::runtime::Builder;

        use super::*;

        #[test]
        fn observation_without_io_fails_closed_before_any_baseline() {
            let runtime = Builder::new_current_thread().build().unwrap();
            let (changes, _receiver) = mpsc::channel(1);
            let mut inventory = None;
            let outcome =
                runtime.block_on(LinuxNetworkChangeWatcher::observe(&changes, &mut inventory));
            assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorUnavailable));
            assert!(inventory.is_none());
        }

        #[tokio::test(flavor = "current_thread")]
        async fn observation_establishes_a_baseline_then_stops_with_its_receiver() {
            let (changes, receiver) = mpsc::channel(1);
            drop(receiver);
            let mut inventory = None;
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                LinuxNetworkChangeWatcher::observe(&changes, &mut inventory),
            )
            .await
            .expect("observation must notice its closed receiver promptly");
            match outcome {
                Ok(()) => assert!(
                    inventory.is_some(),
                    "a clean observation leaves its baseline behind"
                ),
                // A busy host or a sandbox without rtnetlink fails closed.
                Err(
                    NetworkChangeWatchError::MonitorUnavailable
                    | NetworkChangeWatchError::BaselineChanged,
                ) => {}
                Err(error) => panic!("unexpected observation failure {error:?}"),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn inventory(entries: &[(&str, &str)]) -> InterfaceInventory {
        InterfaceInventory::from_addresses(
            entries
                .iter()
                .map(|(name, ip)| ((*name).to_owned(), address(ip))),
        )
    }

    fn change(interfaces: &[&str]) -> NetworkChange {
        NetworkChange::new(interfaces.iter().map(|name| (*name).to_owned()))
    }

    #[test]
    fn debounce_constants_form_a_quiet_period_inside_a_hard_cap() {
        assert!(NETWORK_CHANGE_QUIET_PERIOD < NETWORK_CHANGE_MAX_DELAY);
        assert_eq!(NETWORK_CHANGE_QUIET_PERIOD, Duration::from_millis(500));
        assert_eq!(NETWORK_CHANGE_MAX_DELAY, Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_inside_the_quiet_period_is_delivered_once() {
        let (sender, mut receiver) = mpsc::channel(16);
        for index in 0..5 {
            sender.send(change(&[&format!("if{index}")])).await.unwrap();
        }
        let started = Instant::now();

        let burst = coalesce_burst(&mut receiver, NetworkChange::merge)
            .await
            .expect("an open channel with events yields a burst");

        assert_eq!(burst.count, 5);
        assert_eq!(burst.value.coalesced_count(), 5);
        assert_eq!(burst.value.lost_interfaces().len(), 5);
        assert_eq!(started.elapsed(), NETWORK_CHANGE_QUIET_PERIOD);
    }

    #[tokio::test(start_paused = true)]
    async fn an_isolated_event_waits_exactly_one_quiet_period() {
        let (sender, mut receiver) = mpsc::channel(4);
        sender.send(change(&["wlan0"])).await.unwrap();
        let started = Instant::now();

        let burst = coalesce_burst(&mut receiver, NetworkChange::merge)
            .await
            .unwrap();

        assert_eq!(burst.count, 1);
        assert_eq!(started.elapsed(), NETWORK_CHANGE_QUIET_PERIOD);
    }

    #[tokio::test(start_paused = true)]
    async fn a_continuing_burst_is_cut_at_the_maximum_delay() {
        let (sender, mut receiver) = mpsc::channel(64);
        let spacing = Duration::from_millis(300);
        tokio::spawn(async move {
            for index in 0..12 {
                if sender.send(change(&[&format!("if{index}")])).await.is_err() {
                    return;
                }
                tokio::time::sleep(spacing).await;
            }
        });
        let started = Instant::now();

        let first = coalesce_burst(&mut receiver, NetworkChange::merge)
            .await
            .unwrap();
        let first_elapsed = started.elapsed();
        let second = coalesce_burst(&mut receiver, NetworkChange::merge)
            .await
            .unwrap();

        // Events at 0, 300, ..., 1800 ms fall inside the cap; the quiet
        // period never elapses between them.
        assert_eq!(first.count, 7);
        assert_eq!(first_elapsed, NETWORK_CHANGE_MAX_DELAY);
        assert!(second.count >= 1);
        assert!(
            first
                .value
                .lost_interfaces()
                .is_disjoint(second.value.lost_interfaces())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_ends_the_burst_and_then_the_stream() {
        let (sender, mut receiver) = mpsc::channel(4);
        sender.send(change(&["eth0"])).await.unwrap();
        sender.send(change(&["eth1"])).await.unwrap();
        drop(sender);

        let burst = coalesce_burst(&mut receiver, NetworkChange::merge)
            .await
            .unwrap();
        assert_eq!(burst.count, 2);
        assert!(
            coalesce_burst(&mut receiver, NetworkChange::merge)
                .await
                .is_none()
        );
    }

    #[test]
    fn lost_interfaces_are_those_gone_down_or_missing_an_address() {
        let before = inventory(&[
            ("eth0", "192.0.2.10"),
            ("eth0", "fd12:3456::10"),
            ("wlan0", "198.51.100.20"),
            ("wg0", "10.250.0.2"),
            ("eth1", "203.0.113.5"),
        ]);
        let after = inventory(&[
            // eth0 lost its IPv6 address but kept IPv4.
            ("eth0", "192.0.2.10"),
            // wlan0 moved to another network.
            ("wlan0", "198.51.100.99"),
            // wg0 is unchanged; eth1 is gone; eth2 is new.
            ("wg0", "10.250.0.2"),
            ("eth2", "192.0.2.77"),
        ]);

        let lost = before.lost_since(&after);

        assert_eq!(
            lost,
            ["eth0", "eth1", "wlan0"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(after.lost_since(&after).is_empty());
        assert_eq!(after.len(), 4);
        assert!(!after.is_empty());
    }

    #[test]
    fn an_interface_that_only_gained_addresses_is_not_lost() {
        let before = inventory(&[("eth0", "192.0.2.10")]);
        let after = inventory(&[("eth0", "192.0.2.10"), ("eth0", "192.0.2.11")]);
        assert!(before.lost_since(&after).is_empty());
    }

    #[test]
    fn merging_changes_unions_interfaces_and_sums_counts() {
        let mut merged = change(&["eth0"]);
        merged.merge(change(&["eth0", "wg0"]));
        merged.merge(NetworkChange::coalesced(BTreeSet::new(), 3));

        assert_eq!(merged.lost_interfaces().len(), 2);
        assert_eq!(merged.coalesced_count(), 5);
        assert_eq!(
            NetworkChange::coalesced(BTreeSet::new(), 0).coalesced_count(),
            1
        );
    }

    #[test]
    fn debug_output_never_names_an_interface_or_address() {
        let change = change(&["wg0", "eth0"]);
        let inventory = inventory(&[("wg0", "10.250.0.2")]);
        let rendered = format!("{change:?} {inventory:?}");
        assert!(!rendered.contains("wg0"));
        assert!(!rendered.contains("eth0"));
        assert!(!rendered.contains("10.250"));
        assert!(rendered.contains("lost_interface_count: 2"));
        assert!(rendered.contains("interface_count: 1"));
    }

    #[test]
    fn current_inventory_excludes_loopback() {
        let Ok(inventory) = InterfaceInventory::current() else {
            return;
        };
        let lost = inventory.lost_since(&inventory);
        assert!(lost.is_empty());
        assert!(!format!("{inventory:?}").contains("127.0.0.1"));
    }
}
