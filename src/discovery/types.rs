use std::net::SocketAddr;

use ipnet::IpNet;

/// How a discovery request reached its destination.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiscoveryMethod {
    Targeted,
    Ipv4Broadcast,
    Ipv6LinkLocalMulticast,
    Ipv6SiteLocalMulticast,
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
