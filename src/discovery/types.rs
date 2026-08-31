use std::net::SocketAddr;

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
}
