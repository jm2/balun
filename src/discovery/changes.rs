//! Debounced network-change observation.
//!
//! A network change is any adapter, address, or route event the platform can
//! report. Bursts are coalesced so one reconciliation runs per burst, and the
//! only thing a coalesced change carries is what each interface lost since the
//! previous observation: the interface itself, an IPv4 address, an IPv6
//! link-local address, or its last routable IPv6 address. Nothing here sends a
//! packet, and no value defined here enters a snapshot.

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

/// What one interface lost between two observations.
///
/// Each field names the discovery evidence that became stale: IPv4 broadcast
/// and routed IPv4 replies, IPv6 link-local multicast replies, and IPv6
/// site-local multicast replies. A removed interface loses all of them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterfaceLoss {
    ipv4: bool,
    ipv6_link_local: bool,
    ipv6_routable: bool,
}

impl InterfaceLoss {
    /// The interface disappeared or is no longer up.
    pub const REMOVED: Self = Self {
        ipv4: true,
        ipv6_link_local: true,
        ipv6_routable: true,
    };

    /// Nothing observed through the interface became stale.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.ipv4 || self.ipv6_link_local || self.ipv6_routable)
    }

    /// The interface is gone or lost an IPv4 address.
    #[must_use]
    pub const fn ipv4(self) -> bool {
        self.ipv4
    }

    /// The interface is gone or lost an IPv6 link-local address.
    #[must_use]
    pub const fn ipv6_link_local(self) -> bool {
        self.ipv6_link_local
    }

    /// The interface is gone or no longer has any routable IPv6 address.
    #[must_use]
    pub const fn ipv6_routable(self) -> bool {
        self.ipv6_routable
    }

    #[must_use]
    const fn union(self, other: Self) -> Self {
        Self {
            ipv4: self.ipv4 || other.ipv4,
            ipv6_link_local: self.ipv6_link_local || other.ipv6_link_local,
            ipv6_routable: self.ipv6_routable || other.ipv6_routable,
        }
    }
}

/// One coalesced network change, as the controller reconciles it.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct NetworkChange {
    lost_interfaces: BTreeMap<String, InterfaceLoss>,
    coalesced: usize,
}

impl NetworkChange {
    /// A change in which exactly these interfaces were removed.
    #[must_use]
    pub fn new(lost_interfaces: impl IntoIterator<Item = String>) -> Self {
        Self {
            lost_interfaces: lost_interfaces
                .into_iter()
                .map(|name| (name, InterfaceLoss::REMOVED))
                .collect(),
            coalesced: 1,
        }
    }

    /// A change that stands for `coalesced` raw notifications. Interfaces
    /// that lost nothing are dropped.
    #[must_use]
    pub fn coalesced(
        mut lost_interfaces: BTreeMap<String, InterfaceLoss>,
        coalesced: usize,
    ) -> Self {
        lost_interfaces.retain(|_, loss| !loss.is_empty());
        Self {
            lost_interfaces,
            coalesced: coalesced.max(1),
        }
    }

    /// What each affected interface lost. Evidence observed through it in a
    /// lost family is stale; these names never enter a snapshot.
    #[must_use]
    pub const fn lost_interfaces(&self) -> &BTreeMap<String, InterfaceLoss> {
        &self.lost_interfaces
    }

    /// What `interface` lost; empty when it lost nothing.
    #[must_use]
    pub fn loss(&self, interface: &str) -> InterfaceLoss {
        self.lost_interfaces
            .get(interface)
            .copied()
            .unwrap_or_default()
    }

    /// How many raw notifications this change stands for.
    #[must_use]
    pub const fn coalesced_count(&self) -> usize {
        self.coalesced
    }

    /// Fold a later change into this one.
    pub fn merge(&mut self, later: Self) {
        for (name, loss) in later.lost_interfaces {
            let merged = self.loss(&name).union(loss);
            self.lost_interfaces.insert(name, merged);
        }
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

    /// What each interface present here lost in `later`: the interface
    /// itself (gone or down), any IPv4 address, any IPv6 link-local address,
    /// or its last routable IPv6 address. Temporary IPv6 addresses rotate
    /// routinely while a stable one remains, so losing one of several
    /// routable IPv6 addresses is not a loss; neither is gaining addresses.
    /// Interfaces that lost nothing are omitted.
    #[must_use]
    pub fn loss_since(&self, later: &Self) -> BTreeMap<String, InterfaceLoss> {
        self.interfaces
            .iter()
            .filter_map(|(name, before)| {
                let loss = later
                    .interfaces
                    .get(name)
                    .map_or(InterfaceLoss::REMOVED, |after| address_loss(before, after));
                (!loss.is_empty()).then(|| (name.clone(), loss))
            })
            .collect()
    }
}

fn address_loss(before: &BTreeSet<IpAddr>, after: &BTreeSet<IpAddr>) -> InterfaceLoss {
    let gone = |family: fn(&IpAddr) -> bool| {
        before
            .iter()
            .any(|address| family(address) && !after.contains(address))
    };
    let routable_v6 = |address: &IpAddr| is_ipv6(address) && !is_ipv6_link_local(address);
    InterfaceLoss {
        ipv4: gone(IpAddr::is_ipv4),
        ipv6_link_local: gone(is_ipv6_link_local),
        ipv6_routable: before.iter().any(routable_v6) && !after.iter().any(routable_v6),
    }
}

const fn is_ipv6(address: &IpAddr) -> bool {
    matches!(address, IpAddr::V6(_))
}

const fn is_ipv6_link_local(address: &IpAddr) -> bool {
    matches!(address, IpAddr::V6(address) if address.is_unicast_link_local())
}

impl fmt::Debug for InterfaceInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterfaceInventory")
            .field("interface_count", &self.interfaces.len())
            .finish()
    }
}

mod observation;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod watch;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

// The routing-socket parser is target-independent so every test lane runs it.
#[cfg(any(target_os = "macos", all(test, any(target_os = "linux", windows))))]
mod route_socket;

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::LinuxNetworkChangeWatcher;
#[cfg(target_os = "macos")]
pub use macos::MacosNetworkChangeWatcher;
pub use observation::{ObservationGate, ObservationGeneration, ObservationState, ObservationWatch};
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use watch::NetworkChangeWatchError;
#[cfg(windows)]
pub use windows::WindowsNetworkChangeWatcher;

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
                .keys()
                .all(|name| !second.value.lost_interfaces().contains_key(name))
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
    fn interfaces_lose_what_disappeared_per_address_family() {
        let before = inventory(&[
            ("eth0", "192.0.2.10"),
            ("eth0", "fd12:3456::10"),
            ("wlan0", "198.51.100.20"),
            ("wg0", "10.250.0.2"),
            ("eth1", "203.0.113.5"),
            ("eth3", "fe80::3"),
        ]);
        let after = inventory(&[
            // eth0 lost its only routable IPv6 address but kept IPv4.
            ("eth0", "192.0.2.10"),
            // wlan0 moved to another network.
            ("wlan0", "198.51.100.99"),
            // wg0 is unchanged; eth1 is gone; eth2 is new; eth3 lost its
            // link-local address and gained a routable one.
            ("wg0", "10.250.0.2"),
            ("eth2", "192.0.2.77"),
            ("eth3", "2001:db8::3"),
        ]);

        let lost = before.loss_since(&after);

        let families = |name: &str| {
            let loss = lost[name];
            [loss.ipv4(), loss.ipv6_link_local(), loss.ipv6_routable()]
        };
        assert_eq!(
            lost.keys().map(String::as_str).collect::<Vec<_>>(),
            ["eth0", "eth1", "eth3", "wlan0"]
        );
        assert_eq!(families("eth0"), [false, false, true]);
        assert_eq!(lost["eth1"], InterfaceLoss::REMOVED);
        assert_eq!(families("eth3"), [false, true, false]);
        assert_eq!(families("wlan0"), [true, false, false]);
        assert!(after.loss_since(&after).is_empty());
        assert_eq!(after.len(), 5);
        assert!(!after.is_empty());
    }

    #[test]
    fn a_rotated_temporary_ipv6_address_is_not_a_loss() {
        let before = inventory(&[
            ("eth0", "192.0.2.10"),
            ("eth0", "fe80::10"),
            ("eth0", "2001:db8::10"),
            ("eth0", "2001:db8::beef"),
        ]);
        let after = inventory(&[
            ("eth0", "192.0.2.10"),
            ("eth0", "fe80::10"),
            ("eth0", "2001:db8::10"),
            ("eth0", "2001:db8::cafe"),
        ]);
        assert!(before.loss_since(&after).is_empty());
    }

    #[test]
    fn an_interface_that_only_gained_addresses_is_not_lost() {
        let before = inventory(&[("eth0", "192.0.2.10")]);
        let after = inventory(&[("eth0", "192.0.2.10"), ("eth0", "192.0.2.11")]);
        assert!(before.loss_since(&after).is_empty());
    }

    #[test]
    fn merging_changes_unions_losses_and_sums_counts() {
        let ipv4_only =
            inventory(&[("eth0", "192.0.2.10")]).loss_since(&inventory(&[("eth0", "192.0.2.11")]));
        let mut merged = NetworkChange::coalesced(ipv4_only, 1);
        let link_local_only =
            inventory(&[("eth0", "fe80::1")]).loss_since(&inventory(&[("eth0", "192.0.2.11")]));
        merged.merge(NetworkChange::coalesced(link_local_only, 1));
        merged.merge(change(&["wg0"]));
        merged.merge(NetworkChange::coalesced(BTreeMap::new(), 3));

        assert_eq!(merged.lost_interfaces().len(), 2);
        let eth0 = merged.loss("eth0");
        assert!(eth0.ipv4() && eth0.ipv6_link_local() && !eth0.ipv6_routable());
        assert_eq!(merged.loss("wg0"), InterfaceLoss::REMOVED);
        assert!(merged.loss("eth9").is_empty());
        assert_eq!(merged.coalesced_count(), 6);
        assert_eq!(
            NetworkChange::coalesced(BTreeMap::new(), 0).coalesced_count(),
            1
        );
        // An interface that lost nothing is not recorded.
        let unchanged = [("eth0".to_owned(), InterfaceLoss::default())]
            .into_iter()
            .collect();
        assert!(
            NetworkChange::coalesced(unchanged, 1)
                .lost_interfaces()
                .is_empty()
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
        let lost = inventory.loss_since(&inventory);
        assert!(lost.is_empty());
        assert!(!format!("{inventory:?}").contains("127.0.0.1"));
    }
}
