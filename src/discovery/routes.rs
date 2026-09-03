use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::Ipv4Addr;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use thiserror::Error;

use super::routed::MAX_ROUTED_CANDIDATES;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxRouteProvider;

#[cfg(target_os = "linux")]
pub(in crate::discovery) use linux::{
    LinuxRouteEventMonitor, LinuxRouteMonitorError, RouteMonitorObserver,
    RouteReconciliationRequired,
};

/// An operating-system interface identifier, normalized to fit Linux and
/// macOS interface indexes as well as Windows interface LUIDs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceId(u64);

impl InterfaceId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The policy-relevant kind of a network interface.
///
/// Platform providers, rather than this platform-neutral module, are
/// responsible for recognizing tunnel devices such as Linux WireGuard links,
/// macOS utun interfaces, and Windows tunnel adapters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InterfaceKind {
    Loopback,
    Tunnel,
    Other,
}

/// A normalized interface record supplied by a platform route provider.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NetworkInterface {
    id: InterfaceId,
    name: String,
    kind: InterfaceKind,
    is_up: bool,
    addresses: Vec<IpNet>,
}

impl NetworkInterface {
    pub fn new(
        id: InterfaceId,
        name: impl Into<String>,
        kind: InterfaceKind,
        is_up: bool,
        addresses: impl IntoIterator<Item = IpNet>,
    ) -> Self {
        let addresses = addresses
            .into_iter()
            .map(|network| network.trunc())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Self {
            id,
            name: name.into(),
            kind,
            is_up,
            addresses,
        }
    }

    #[must_use]
    pub const fn id(&self) -> InterfaceId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> InterfaceKind {
        self.kind
    }

    #[must_use]
    pub const fn is_up(&self) -> bool {
        self.is_up
    }

    #[must_use]
    pub fn addresses(&self) -> &[IpNet] {
        &self.addresses
    }
}

/// Whether a route can carry ordinary unicast traffic.
///
/// Blackhole, reject, multicast, and other special platform routes should be
/// normalized to `Other` so they can never produce discovery candidates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RouteKind {
    Unicast,
    Other,
}

/// How the operating system reaches a route's next hop.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RouteScope {
    OnLink,
    ViaGateway,
    Other,
}

/// A normalized route selected by the operating system for its destination
/// prefix.
///
/// This is deliberately not a raw route-table row. Constructing a value with
/// [`NetworkRoute::effective`] asserts that the platform provider has already
/// applied the operating system's routing-domain, policy, metric, and
/// next-hop selection rules. A provider must omit losing alternatives with the
/// same prefix; equal-cost alternatives that the operating system may select
/// may all be returned. A more-specific selected route may shadow part of a
/// broader route, so candidate selection also applies longest-prefix matching.
///
/// A provider must never omit an uncertain route when doing so could promote a
/// broader candidate-producing route. It must either fail the complete
/// snapshot or retain that destination prefix as a non-candidate blocker with
/// [`RouteKind::Other`] and [`RouteScope::Other`]. Omission is safe only when
/// the provider can prove that candidate selection cannot change. Users can
/// still approve an exact address or bounded explicit range when a provider
/// fails closed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NetworkRoute {
    destination: IpNet,
    interface: Option<InterfaceId>,
    kind: RouteKind,
    scope: RouteScope,
}

impl NetworkRoute {
    /// Assert that this is an operating-system-selected effective route.
    #[must_use]
    pub fn effective(
        destination: IpNet,
        interface: Option<InterfaceId>,
        kind: RouteKind,
        scope: RouteScope,
    ) -> Self {
        Self {
            destination: destination.trunc(),
            interface,
            kind,
            scope,
        }
    }

    #[must_use]
    pub const fn destination(self) -> IpNet {
        self.destination
    }

    #[must_use]
    pub const fn interface(self) -> Option<InterfaceId> {
        self.interface
    }

    #[must_use]
    pub const fn kind(self) -> RouteKind {
        self.kind
    }

    #[must_use]
    pub const fn scope(self) -> RouteScope {
        self.scope
    }
}

/// One internally consistent view of interfaces and effective routes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteSnapshot {
    interfaces: Vec<NetworkInterface>,
    effective_routes: Vec<NetworkRoute>,
}

impl RouteSnapshot {
    /// Build a snapshot from routes already resolved by the platform provider.
    ///
    /// Every route must satisfy the contract of [`NetworkRoute::effective`].
    #[must_use]
    pub fn from_effective_routes(
        mut interfaces: Vec<NetworkInterface>,
        mut effective_routes: Vec<NetworkRoute>,
    ) -> Self {
        interfaces.sort();
        interfaces.dedup();
        effective_routes.sort();
        effective_routes.dedup();

        Self {
            interfaces,
            effective_routes,
        }
    }

    #[must_use]
    pub fn interfaces(&self) -> &[NetworkInterface] {
        &self.interfaces
    }

    #[must_use]
    pub fn effective_routes(&self) -> &[NetworkRoute] {
        &self.effective_routes
    }

    /// Interfaces the candidate policy treats as tunnels: up, and classified
    /// as [`InterfaceKind::Tunnel`] by every record that shares the id.
    ///
    /// This is the complete set of recognized tunnels, whether or not any of
    /// their routes is eligible to produce a candidate.
    #[must_use]
    pub fn tunnel_interfaces(&self) -> BTreeSet<InterfaceId> {
        let mut kinds_by_id = BTreeMap::<InterfaceId, BTreeSet<InterfaceKind>>::new();
        for interface in self.interfaces.iter().filter(|item| item.is_up()) {
            kinds_by_id
                .entry(interface.id())
                .or_default()
                .insert(interface.kind());
        }
        kinds_by_id
            .into_iter()
            .filter_map(|(id, kinds)| {
                (kinds.len() == 1 && kinds.contains(&InterfaceKind::Tunnel)).then_some(id)
            })
            .collect()
    }
}

/// Supplies a normalized, platform-specific interface and effective-route
/// snapshot.
///
/// Implementations are expected to use netlink on Linux, routing APIs plus
/// `getifaddrs` on macOS, and IP Helper APIs on Windows. Collection may block,
/// so callers running an async controller should invoke providers on an
/// appropriate blocking worker.
///
/// Implementations must resolve platform routing tables, policy rules,
/// compartments, and metrics as the operating system would for the discovery
/// socket. Snapshots must contain only routes satisfying
/// [`NetworkRoute::effective`], never an unfiltered dump of installed routes.
/// An uncertain potentially winning prefix must be represented as a
/// non-candidate blocker or make the whole snapshot fail; silently dropping it
/// can expose a broader route that the operating system would not select.
pub trait RouteProvider: Send + Sync {
    fn snapshot(&self) -> io::Result<RouteSnapshot>;
}

/// Why an address was selected as a routed-discovery candidate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RouteCandidateOrigin {
    Explicit(Ipv4Net),
    TunnelRoute {
        interface: InterfaceId,
        network: Ipv4Net,
    },
}

/// One unique IPv4 target and every eligible source that suggested it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCandidate {
    address: Ipv4Addr,
    origins: BTreeSet<RouteCandidateOrigin>,
}

impl RouteCandidate {
    #[must_use]
    pub const fn address(&self) -> Ipv4Addr {
        self.address
    }

    #[must_use]
    pub fn origins(&self) -> &BTreeSet<RouteCandidateOrigin> {
        &self.origins
    }
}

/// A failure to obtain or safely turn routes into targeted probe candidates.
#[derive(Debug, Error)]
pub enum RouteCandidateError {
    #[error("failed to read network routes: {0}")]
    Provider(#[source] io::Error),

    #[error("IPv6 range {0} cannot be enumerated for routed discovery")]
    ExplicitIpv6(Ipv6Net),

    #[error("the default route cannot be used for routed discovery")]
    ExplicitDefaultRoute,

    #[error("explicit routed range {network} is wider than /{maximum_prefix}")]
    ExplicitRangeTooWide {
        network: Ipv4Net,
        maximum_prefix: u8,
    },

    #[error("explicit routed range {0} is not wholly within RFC 1918 space")]
    ExplicitRangeNotPrivate(Ipv4Net),

    #[error("routed discovery would exceed the {maximum}-candidate safety limit")]
    TooManyCandidates { maximum: usize },
}

/// Read a provider snapshot and select bounded routed-discovery candidates.
pub fn route_candidates<P: RouteProvider + ?Sized>(
    provider: &P,
    explicit_ranges: &[IpNet],
) -> Result<Vec<RouteCandidate>, RouteCandidateError> {
    let snapshot = provider.snapshot().map_err(RouteCandidateError::Provider)?;
    select_route_candidates(&snapshot, explicit_ranges)
}

/// Select deterministic IPv4 targets from explicit ranges and eligible
/// effective tunnel routes.
///
/// Explicit ranges fail closed when they are IPv6, non-private, default, or
/// wider than `/24`. Ineligible entries in the operating-system route table
/// are ignored because normal snapshots contain default, public, local, and
/// IPv6 routes. An automatic origin is retained only while its route is a
/// longest-prefix match and every equally specific effective route uses an
/// active, unambiguously classified tunnel. Addresses covered by an active
/// interface's assigned network or an active non-tunnel on-link route are
/// removed so directly connected LANs and tunnel transit networks are never
/// enumerated.
pub fn select_route_candidates(
    snapshot: &RouteSnapshot,
    explicit_ranges: &[IpNet],
) -> Result<Vec<RouteCandidate>, RouteCandidateError> {
    let explicit_ranges = explicit_ranges
        .iter()
        .map(|network| network.trunc())
        .collect::<BTreeSet<_>>();

    let mut eligible_ranges = BTreeMap::<Ipv4Net, BTreeSet<RouteCandidateOrigin>>::new();
    for network in explicit_ranges {
        let network = validate_explicit_range(network)?;
        eligible_ranges
            .entry(network)
            .or_default()
            .insert(RouteCandidateOrigin::Explicit(network));
    }

    let interface_policy = InterfacePolicy::from_snapshot(snapshot);
    for route in snapshot.effective_routes() {
        if route.kind() != RouteKind::Unicast || route.scope() == RouteScope::Other {
            continue;
        }
        let Some(interface) = route.interface() else {
            continue;
        };
        if !interface_policy.tunnels.contains(&interface) {
            continue;
        }
        let IpNet::V4(network) = route.destination() else {
            continue;
        };
        if !eligible_ipv4_range(network) {
            continue;
        }

        let network = network.trunc();
        eligible_ranges
            .entry(network)
            .or_default()
            .insert(RouteCandidateOrigin::TunnelRoute { interface, network });
    }

    let mut candidates = BTreeMap::<Ipv4Addr, BTreeSet<RouteCandidateOrigin>>::new();
    for (range, origins) in eligible_ranges {
        for candidate in range.hosts() {
            if interface_policy
                .direct_networks
                .iter()
                .any(|network| network.contains(&candidate))
            {
                continue;
            }

            let effective_origins = origins
                .iter()
                .copied()
                .filter(|origin| {
                    origin_is_effective_for_candidate(
                        *origin,
                        candidate,
                        snapshot,
                        &interface_policy,
                    )
                })
                .collect::<BTreeSet<_>>();
            if effective_origins.is_empty() {
                continue;
            }

            candidates
                .entry(candidate)
                .or_default()
                .extend(effective_origins);
            if candidates.len() > MAX_ROUTED_CANDIDATES {
                return Err(RouteCandidateError::TooManyCandidates {
                    maximum: MAX_ROUTED_CANDIDATES,
                });
            }
        }
    }

    Ok(candidates
        .into_iter()
        .map(|(address, origins)| RouteCandidate { address, origins })
        .collect())
}

fn origin_is_effective_for_candidate(
    origin: RouteCandidateOrigin,
    candidate: Ipv4Addr,
    snapshot: &RouteSnapshot,
    interface_policy: &InterfacePolicy,
) -> bool {
    let RouteCandidateOrigin::TunnelRoute { interface, network } = origin else {
        return true;
    };

    let best_prefix = snapshot
        .effective_routes()
        .iter()
        .filter_map(|route| match route.destination() {
            IpNet::V4(route_network) if route_network.contains(&candidate) => {
                Some(route_network.prefix_len())
            }
            _ => None,
        })
        .max();
    let Some(best_prefix) = best_prefix else {
        return false;
    };
    if network.prefix_len() != best_prefix {
        return false;
    }

    let mut origin_is_a_winner = false;
    for route in snapshot.effective_routes() {
        let IpNet::V4(route_network) = route.destination() else {
            continue;
        };
        if route_network.prefix_len() != best_prefix || !route_network.contains(&candidate) {
            continue;
        }

        if route.kind() != RouteKind::Unicast || route.scope() == RouteScope::Other {
            return false;
        }
        let Some(route_interface) = route.interface() else {
            return false;
        };
        if !interface_policy.tunnels.contains(&route_interface) {
            return false;
        }

        origin_is_a_winner |= route_interface == interface && route_network == network;
    }

    origin_is_a_winner
}

struct InterfacePolicy {
    tunnels: BTreeSet<InterfaceId>,
    direct_networks: BTreeSet<Ipv4Net>,
}

impl InterfacePolicy {
    fn from_snapshot(snapshot: &RouteSnapshot) -> Self {
        let tunnels = snapshot.tunnel_interfaces();
        let mut active = BTreeSet::new();
        let mut direct_networks = BTreeSet::new();

        for interface in snapshot.interfaces().iter().filter(|item| item.is_up()) {
            active.insert(interface.id());

            for address in interface.addresses() {
                let IpNet::V4(network) = address else {
                    continue;
                };
                direct_networks.insert(network.trunc());
            }
        }

        for route in snapshot.effective_routes() {
            if route.kind() != RouteKind::Unicast || route.scope() != RouteScope::OnLink {
                continue;
            }
            let Some(interface) = route.interface() else {
                continue;
            };
            if !active.contains(&interface) {
                continue;
            }
            // A tunnel's own OnLink destination is a remote routed candidate,
            // not a directly connected LAN. Linux commonly represents
            // WireGuard routes as `dev wg0 scope link`, so suppressing these
            // would disable automatic tunnel discovery. Assigned tunnel
            // prefixes were already added above and remain suppressed.
            if tunnels.contains(&interface) {
                continue;
            }
            let IpNet::V4(network) = route.destination() else {
                continue;
            };
            direct_networks.insert(network.trunc());
        }

        Self {
            tunnels,
            direct_networks,
        }
    }
}

fn validate_explicit_range(network: IpNet) -> Result<Ipv4Net, RouteCandidateError> {
    match network {
        IpNet::V4(network) if network.prefix_len() == 0 => {
            Err(RouteCandidateError::ExplicitDefaultRoute)
        }
        IpNet::V4(network) if network.prefix_len() < 24 => {
            Err(RouteCandidateError::ExplicitRangeTooWide {
                network,
                maximum_prefix: 24,
            })
        }
        IpNet::V4(network) if !wholly_rfc1918(network) => {
            Err(RouteCandidateError::ExplicitRangeNotPrivate(network))
        }
        IpNet::V4(network) => Ok(network.trunc()),
        IpNet::V6(network) => Err(RouteCandidateError::ExplicitIpv6(network)),
    }
}

fn eligible_ipv4_range(network: Ipv4Net) -> bool {
    network.prefix_len() >= 24 && wholly_rfc1918(network)
}

fn wholly_rfc1918(network: Ipv4Net) -> bool {
    network.network().is_private() && network.broadcast().is_private()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct FakeRouteProvider {
        snapshot: RouteSnapshot,
        error: Option<io::ErrorKind>,
    }

    impl FakeRouteProvider {
        fn with_snapshot(snapshot: RouteSnapshot) -> Self {
            Self {
                snapshot,
                error: None,
            }
        }

        fn failing(kind: io::ErrorKind) -> Self {
            Self {
                snapshot: RouteSnapshot::default(),
                error: Some(kind),
            }
        }
    }

    impl RouteProvider for FakeRouteProvider {
        fn snapshot(&self) -> io::Result<RouteSnapshot> {
            match self.error {
                Some(kind) => Err(io::Error::new(kind, "synthetic route-provider error")),
                None => Ok(self.snapshot.clone()),
            }
        }
    }

    fn net(value: &str) -> IpNet {
        value.parse().expect("valid test network")
    }

    fn id(value: u64) -> InterfaceId {
        InterfaceId::new(value)
    }

    fn interface(
        value: u64,
        kind: InterfaceKind,
        is_up: bool,
        addresses: &[&str],
    ) -> NetworkInterface {
        NetworkInterface::new(
            id(value),
            format!("if{value}"),
            kind,
            is_up,
            addresses.iter().map(|value| net(value)),
        )
    }

    fn route(value: &str, interface: Option<u64>, kind: RouteKind) -> NetworkRoute {
        NetworkRoute::effective(net(value), interface.map(id), kind, RouteScope::OnLink)
    }

    fn route_via_gateway(value: &str, interface: Option<u64>, kind: RouteKind) -> NetworkRoute {
        NetworkRoute::effective(net(value), interface.map(id), kind, RouteScope::ViaGateway)
    }

    fn candidate_addresses(candidates: &[RouteCandidate]) -> Vec<Ipv4Addr> {
        candidates.iter().map(RouteCandidate::address).collect()
    }

    fn addresses(first: [u8; 4], last: [u8; 4]) -> Vec<Ipv4Addr> {
        let first = u32::from_be_bytes(first);
        let last = u32::from_be_bytes(last);
        (first..=last).map(Ipv4Addr::from).collect()
    }

    #[test]
    fn tunnel_interfaces_are_the_active_unambiguous_tunnels() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &[]),
                interface(2, InterfaceKind::Tunnel, false, &[]),
                interface(3, InterfaceKind::Tunnel, true, &[]),
                interface(3, InterfaceKind::Other, true, &[]),
                interface(4, InterfaceKind::Other, true, &[]),
            ],
            vec![],
        );

        assert_eq!(snapshot.tunnel_interfaces(), BTreeSet::from([id(1)]));
    }

    #[test]
    fn provider_snapshot_drives_tunnel_and_explicit_candidates() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(
                7,
                InterfaceKind::Tunnel,
                true,
                &["10.255.0.2/32"],
            )],
            vec![route("192.168.40.8/30", Some(7), RouteKind::Unicast)],
        );
        let provider = FakeRouteProvider::with_snapshot(snapshot);

        let candidates = route_candidates(&provider, &[net("172.20.5.9/32")]).unwrap();

        assert_eq!(
            candidate_addresses(&candidates),
            vec![
                Ipv4Addr::new(172, 20, 5, 9),
                Ipv4Addr::new(192, 168, 40, 9),
                Ipv4Addr::new(192, 168, 40, 10),
            ]
        );
    }

    #[test]
    fn provider_errors_are_preserved() {
        let provider = FakeRouteProvider::failing(io::ErrorKind::PermissionDenied);

        let error = route_candidates(&provider, &[]).expect_err("provider must fail");

        match error {
            RouteCandidateError::Provider(error) => {
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn only_active_unambiguous_tunnels_supply_automatic_ranges() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &[]),
                interface(2, InterfaceKind::Tunnel, false, &[]),
                interface(3, InterfaceKind::Other, true, &[]),
                interface(4, InterfaceKind::Loopback, true, &[]),
                interface(5, InterfaceKind::Tunnel, true, &[]),
                interface(5, InterfaceKind::Other, true, &[]),
            ],
            vec![
                route("10.0.1.9/32", Some(1), RouteKind::Unicast),
                route("10.0.2.9/32", Some(2), RouteKind::Unicast),
                route("10.0.3.9/32", Some(3), RouteKind::Unicast),
                route("10.0.4.9/32", Some(4), RouteKind::Unicast),
                route("10.0.5.9/32", Some(5), RouteKind::Unicast),
                route("10.0.6.9/32", Some(99), RouteKind::Unicast),
                route("10.0.7.9/32", None, RouteKind::Unicast),
                route("10.0.8.9/32", Some(1), RouteKind::Other),
            ],
        );

        assert_eq!(
            candidate_addresses(&select_route_candidates(&snapshot, &[]).unwrap()),
            vec![Ipv4Addr::new(10, 0, 1, 9)]
        );
    }

    #[test]
    fn ineligible_tunnel_routes_are_ignored() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(1, InterfaceKind::Tunnel, true, &[])],
            vec![
                route("0.0.0.0/0", Some(1), RouteKind::Unicast),
                route("10.50.0.0/23", Some(1), RouteKind::Unicast),
                route("203.0.113.0/24", Some(1), RouteKind::Unicast),
                route("100.64.0.0/24", Some(1), RouteKind::Unicast),
                route("0.0.0.0/32", Some(1), RouteKind::Unicast),
                route("127.0.0.0/24", Some(1), RouteKind::Unicast),
                route("169.254.8.0/24", Some(1), RouteKind::Unicast),
                route("224.0.0.0/24", Some(1), RouteKind::Unicast),
                route("fd00::/120", Some(1), RouteKind::Unicast),
                route("192.168.90.6/32", Some(1), RouteKind::Unicast),
            ],
        );

        assert_eq!(
            candidate_addresses(&select_route_candidates(&snapshot, &[]).unwrap()),
            vec![Ipv4Addr::new(192, 168, 90, 6)]
        );
    }

    #[test]
    fn explicit_ipv6_is_rejected_without_enumeration() {
        let IpNet::V6(network) = net("fd12:3456::/120") else {
            panic!("expected IPv6 test range");
        };

        let error = select_route_candidates(&RouteSnapshot::default(), &[IpNet::V6(network)])
            .expect_err("IPv6 enumeration is forbidden");

        assert!(matches!(error, RouteCandidateError::ExplicitIpv6(value) if value == network));
    }

    #[test]
    fn explicit_default_route_is_rejected() {
        assert!(matches!(
            select_route_candidates(&RouteSnapshot::default(), &[net("0.0.0.0/0")]),
            Err(RouteCandidateError::ExplicitDefaultRoute)
        ));
    }

    #[test]
    fn explicit_ranges_wider_than_slash_24_are_rejected() {
        let network = "10.2.0.0/23".parse::<Ipv4Net>().unwrap();

        assert!(matches!(
            select_route_candidates(&RouteSnapshot::default(), &[IpNet::V4(network)]),
            Err(RouteCandidateError::ExplicitRangeTooWide {
                network: value,
                maximum_prefix: 24,
            }) if value == network
        ));
    }

    #[test]
    fn explicit_public_and_special_ranges_are_rejected() {
        for value in [
            "0.0.0.0/32",
            "100.64.0.0/24",
            "192.0.2.0/24",
            "127.0.0.0/24",
            "169.254.12.0/24",
            "224.0.0.0/24",
        ] {
            let network = value.parse::<Ipv4Net>().unwrap();
            assert!(matches!(
                select_route_candidates(&RouteSnapshot::default(), &[IpNet::V4(network)]),
                Err(RouteCandidateError::ExplicitRangeNotPrivate(candidate))
                    if candidate == network
            ));
        }
    }

    #[test]
    fn active_assigned_networks_are_removed_from_all_sources() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &["10.99.0.2/24"]),
                interface(2, InterfaceKind::Other, true, &["192.168.20.99/24"]),
            ],
            vec![
                route("10.99.0.0/24", Some(1), RouteKind::Unicast),
                route("192.168.20.0/24", Some(1), RouteKind::Unicast),
            ],
        );

        let candidates =
            select_route_candidates(&snapshot, &[net("10.99.0.9/32"), net("192.168.20.200/32")])
                .unwrap();

        assert!(candidates.is_empty());
    }

    #[test]
    fn active_loopback_networks_are_removed_from_all_sources() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(
                1,
                InterfaceKind::Loopback,
                true,
                &["10.70.0.9/32"],
            )],
            vec![route("10.71.0.0/24", Some(1), RouteKind::Unicast)],
        );

        let candidates =
            select_route_candidates(&snapshot, &[net("10.70.0.9/32"), net("10.71.0.9/32")])
                .unwrap();

        assert!(candidates.is_empty());
    }

    #[test]
    fn broad_assigned_and_on_link_networks_suppress_private_subranges() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Other, true, &["10.20.30.40/7"]),
                interface(2, InterfaceKind::Other, true, &[]),
            ],
            vec![route("172.20.0.0/11", Some(2), RouteKind::Unicast)],
        );

        let candidates =
            select_route_candidates(&snapshot, &[net("10.20.30.9/32"), net("172.20.30.9/32")])
                .unwrap();

        assert!(candidates.is_empty());
    }

    #[test]
    fn only_the_overlapping_part_of_a_direct_network_is_removed() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(2, InterfaceKind::Other, true, &["10.30.8.42/25"])],
            vec![],
        );

        let candidates = select_route_candidates(&snapshot, &[net("10.30.8.0/24")]).unwrap();

        assert_eq!(
            candidate_addresses(&candidates),
            addresses([10, 30, 8, 128], [10, 30, 8, 254])
        );
    }

    #[test]
    fn down_interface_networks_do_not_suppress_explicit_remote_targets() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(1, InterfaceKind::Other, false, &["10.1.1.1/24"])],
            vec![],
        );

        let candidates = select_route_candidates(&snapshot, &[net("10.1.1.9/32")]).unwrap();

        assert_eq!(
            candidate_addresses(&candidates),
            vec![Ipv4Addr::new(10, 1, 1, 9)]
        );
    }

    #[test]
    fn physical_route_is_not_automatic_but_same_range_can_be_explicit() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(
                3,
                InterfaceKind::Other,
                true,
                &["192.168.1.5/24"],
            )],
            vec![route_via_gateway(
                "10.80.9.0/30",
                Some(3),
                RouteKind::Unicast,
            )],
        );

        assert!(select_route_candidates(&snapshot, &[]).unwrap().is_empty());
        assert_eq!(
            candidate_addresses(
                &select_route_candidates(&snapshot, &[net("10.80.9.0/30")]).unwrap()
            ),
            vec![Ipv4Addr::new(10, 80, 9, 1), Ipv4Addr::new(10, 80, 9, 2),]
        );
    }

    #[test]
    fn on_link_physical_routes_are_treated_as_directly_covered() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &[]),
                interface(2, InterfaceKind::Other, true, &["192.168.1.5/24"]),
            ],
            vec![
                route("10.81.4.0/30", Some(1), RouteKind::Unicast),
                route("10.81.4.0/30", Some(2), RouteKind::Unicast),
            ],
        );

        assert!(
            select_route_candidates(&snapshot, &[net("10.81.4.0/30")])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn more_specific_non_tunnel_route_shadows_an_automatic_tunnel_route() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &[]),
                interface(2, InterfaceKind::Other, true, &[]),
            ],
            vec![
                route("10.83.4.0/30", Some(1), RouteKind::Unicast),
                route_via_gateway("10.83.4.1/32", Some(2), RouteKind::Unicast),
            ],
        );

        assert_eq!(
            candidate_addresses(&select_route_candidates(&snapshot, &[]).unwrap()),
            vec![Ipv4Addr::new(10, 83, 4, 2)]
        );
    }

    #[test]
    fn more_specific_uncertain_blocker_prevents_broader_route_promotion() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(1, InterfaceKind::Tunnel, true, &[])],
            vec![
                route("10.85.4.0/30", Some(1), RouteKind::Unicast),
                NetworkRoute::effective(
                    net("10.85.4.1/32"),
                    None,
                    RouteKind::Other,
                    RouteScope::Other,
                ),
            ],
        );

        assert_eq!(
            candidate_addresses(&select_route_candidates(&snapshot, &[]).unwrap()),
            vec![Ipv4Addr::new(10, 85, 4, 2)]
        );
    }

    #[test]
    fn equally_specific_non_tunnel_route_makes_automatic_selection_ambiguous() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(1, InterfaceKind::Tunnel, true, &[]),
                interface(2, InterfaceKind::Other, true, &[]),
            ],
            vec![
                route("10.84.4.0/30", Some(1), RouteKind::Unicast),
                route_via_gateway("10.84.4.0/30", Some(2), RouteKind::Unicast),
            ],
        );

        assert!(select_route_candidates(&snapshot, &[]).unwrap().is_empty());
    }

    #[test]
    fn routes_with_unknown_scope_never_supply_candidates() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface(1, InterfaceKind::Tunnel, true, &[])],
            vec![NetworkRoute::effective(
                net("10.82.4.9/32"),
                Some(id(1)),
                RouteKind::Unicast,
                RouteScope::Other,
            )],
        );

        assert!(select_route_candidates(&snapshot, &[]).unwrap().is_empty());
    }

    #[test]
    fn duplicate_and_overlapping_sources_merge_origins_and_sort_targets() {
        let forward = RouteSnapshot::from_effective_routes(
            vec![interface(7, InterfaceKind::Tunnel, true, &[])],
            vec![
                route("172.16.9.4/30", Some(7), RouteKind::Unicast),
                route("172.16.9.5/32", Some(7), RouteKind::Unicast),
                route("172.16.9.4/30", Some(7), RouteKind::Unicast),
            ],
        );
        let reverse = RouteSnapshot::from_effective_routes(
            forward.interfaces().iter().cloned().rev().collect(),
            forward.effective_routes().iter().copied().rev().collect(),
        );
        let explicit = [net("172.16.9.6/32"), net("172.16.9.5/32")];

        let expected = vec![Ipv4Addr::new(172, 16, 9, 5), Ipv4Addr::new(172, 16, 9, 6)];
        let forward_candidates = select_route_candidates(&forward, &explicit).unwrap();
        let reversed_explicit = explicit.into_iter().rev().collect::<Vec<_>>();
        let reverse_candidates = select_route_candidates(&reverse, &reversed_explicit).unwrap();
        assert_eq!(candidate_addresses(&forward_candidates), expected);
        assert_eq!(candidate_addresses(&reverse_candidates), expected);
        assert_eq!(forward_candidates, reverse_candidates);

        assert_eq!(forward_candidates[0].origins().len(), 2);
        assert_eq!(forward_candidates[1].origins().len(), 2);
    }

    #[test]
    fn slash_31_and_slash_32_ranges_keep_point_to_point_hosts() {
        let candidates = select_route_candidates(
            &RouteSnapshot::default(),
            &[net("10.0.0.4/31"), net("10.0.0.9/32")],
        )
        .unwrap();

        assert_eq!(
            candidate_addresses(&candidates),
            vec![
                Ipv4Addr::new(10, 0, 0, 4),
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(10, 0, 0, 9),
            ]
        );
    }

    #[test]
    fn exactly_256_unique_candidates_are_allowed() {
        let candidates = select_route_candidates(
            &RouteSnapshot::default(),
            &[net("10.4.0.0/24"), net("10.4.1.0/31")],
        )
        .unwrap();

        assert_eq!(candidates.len(), MAX_ROUTED_CANDIDATES);
    }

    #[test]
    fn more_than_256_unique_candidates_are_rejected() {
        assert!(matches!(
            select_route_candidates(
                &RouteSnapshot::default(),
                &[net("10.4.0.0/24"), net("10.4.1.0/29")],
            ),
            Err(RouteCandidateError::TooManyCandidates { maximum })
                if maximum == MAX_ROUTED_CANDIDATES
        ));
    }

    #[test]
    fn constructors_canonicalize_and_deduplicate_snapshot_data() {
        let interface = NetworkInterface::new(
            id(42),
            "wg0",
            InterfaceKind::Tunnel,
            true,
            [net("10.9.8.7/24"), net("10.9.8.0/24")],
        );
        assert_eq!(interface.name(), "wg0");
        assert_eq!(interface.id().get(), 42);
        assert_eq!(interface.kind(), InterfaceKind::Tunnel);
        assert_eq!(interface.addresses(), &[net("10.9.8.0/24")]);

        let route = NetworkRoute::effective(
            net("192.168.4.99/24"),
            Some(id(42)),
            RouteKind::Unicast,
            RouteScope::OnLink,
        );
        assert_eq!(route.destination(), net("192.168.4.0/24"));
        assert_eq!(route.scope(), RouteScope::OnLink);

        let snapshot = RouteSnapshot::from_effective_routes(
            vec![interface.clone(), interface],
            vec![route, route],
        );
        assert_eq!(snapshot.interfaces().len(), 1);
        assert_eq!(snapshot.effective_routes(), &[route]);
    }
}
