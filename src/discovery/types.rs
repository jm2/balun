use std::net::SocketAddr;

use ipnet::IpNet;

/// How a discovery request reached its destination.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiscoveryMethod {
    /// Exact unicast address supplied by a manual, cached, or similarly
    /// high-confidence source.
    Targeted,
    /// Unicast address selected by the separately approved routed-scan
    /// policy.
    RoutedTargeted,
    Ipv4Broadcast,
    Ipv6LinkLocalMulticast,
    Ipv6SiteLocalMulticast,
}

impl DiscoveryMethod {
    /// Whether this method uses exact-source unicast response validation.
    #[must_use]
    pub const fn is_targeted(self) -> bool {
        matches!(self, Self::Targeted | Self::RoutedTargeted)
    }
}

/// One socket binding and destination used for a discovery probe.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProbeEndpoint {
    pub bind: SocketAddr,
    pub destination: SocketAddr,
    pub method: DiscoveryMethod,
    pub interface: Option<String>,
    /// Directly attached prefix from which a broadcast or multicast response
    /// may be accepted. Targeted probes instead require an exact source match
    /// and therefore carry no prefix.
    pub accepted_source_network: Option<IpNet>,
}
