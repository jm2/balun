use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use if_addrs::{IfAddr, Interface, get_if_addrs};
use ipnet::{IpNet, Ipv4Net, Ipv6Net};

use super::{DiscoveryMethod, ProbeEndpoint};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;

const IPV6_LINK_LOCAL_DISCOVERY_GROUP: Ipv6Addr = Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 0x0176);
const IPV6_SITE_LOCAL_DISCOVERY_GROUP: Ipv6Addr = Ipv6Addr::new(0xFF05, 0, 0, 0, 0, 0, 0, 0x0176);

/// Enumerate standard discovery probes for active, non-tunnel interfaces.
///
/// Point-to-point interfaces are intentionally excluded. Routed discovery
/// has a separate approval and packet-budget path.
pub fn local_probe_endpoints() -> io::Result<Vec<ProbeEndpoint>> {
    Ok(endpoints_from_interfaces(get_if_addrs()?))
}

fn endpoints_from_interfaces(interfaces: Vec<Interface>) -> Vec<ProbeEndpoint> {
    let mut endpoints = BTreeSet::new();

    for interface in interfaces {
        if !interface.is_oper_up() || interface.is_loopback() || interface.is_p2p() {
            continue;
        }

        let endpoint = match interface.addr {
            IfAddr::V4(address) => address.broadcast.and_then(|broadcast| {
                let network = Ipv4Net::with_netmask(address.ip, address.netmask).ok()?;
                Some(ProbeEndpoint {
                    bind: SocketAddr::V4(SocketAddrV4::new(address.ip, 0)),
                    destination: SocketAddr::V4(SocketAddrV4::new(broadcast, DISCOVERY_UDP_PORT)),
                    method: DiscoveryMethod::Ipv4Broadcast,
                    interface: Some(interface.name),
                    accepted_source_network: Some(IpNet::V4(network)),
                })
            }),
            IfAddr::V6(address) => ipv6_endpoint(
                &interface.name,
                interface.index,
                address.ip,
                address.netmask,
            ),
        };

        if let Some(endpoint) = endpoint {
            endpoints.insert(endpoint);
        }
    }

    endpoints.into_iter().collect()
}

fn ipv6_endpoint(
    interface: &str,
    interface_index: Option<u32>,
    address: Ipv6Addr,
    netmask: Ipv6Addr,
) -> Option<ProbeEndpoint> {
    if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
        return None;
    }

    let scope_id = interface_index?;
    let network = Ipv6Net::with_netmask(address, netmask).ok()?;
    let (group, method) = if address.is_unicast_link_local() {
        (
            IPV6_LINK_LOCAL_DISCOVERY_GROUP,
            DiscoveryMethod::Ipv6LinkLocalMulticast,
        )
    } else {
        (
            IPV6_SITE_LOCAL_DISCOVERY_GROUP,
            DiscoveryMethod::Ipv6SiteLocalMulticast,
        )
    };

    Some(ProbeEndpoint {
        bind: SocketAddr::V6(SocketAddrV6::new(address, 0, 0, scope_id)),
        destination: SocketAddr::V6(SocketAddrV6::new(group, DISCOVERY_UDP_PORT, 0, scope_id)),
        method,
        interface: Some(interface.to_owned()),
        accepted_source_network: Some(IpNet::V6(network)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_scoped_link_local_ipv6_probe() {
        let endpoint = ipv6_endpoint(
            "enp1s0",
            Some(7),
            "fe80::1234".parse().unwrap(),
            "ffff:ffff:ffff:ffff::".parse().unwrap(),
        )
        .expect("link-local endpoint");

        assert_eq!(endpoint.method, DiscoveryMethod::Ipv6LinkLocalMulticast);
        assert_eq!(
            endpoint.destination,
            SocketAddr::V6(SocketAddrV6::new(
                IPV6_LINK_LOCAL_DISCOVERY_GROUP,
                DISCOVERY_UDP_PORT,
                0,
                7
            ))
        );
        assert_eq!(endpoint.interface.as_deref(), Some("enp1s0"));
    }

    #[test]
    fn builds_scoped_site_local_ipv6_probe() {
        let endpoint = ipv6_endpoint(
            "enp1s0",
            Some(9),
            "fd12:3456::1".parse().unwrap(),
            "ffff:ffff:ffff:ffff::".parse().unwrap(),
        )
        .expect("site-local endpoint");

        assert_eq!(endpoint.method, DiscoveryMethod::Ipv6SiteLocalMulticast);
        assert_eq!(
            endpoint.destination,
            SocketAddr::V6(SocketAddrV6::new(
                IPV6_SITE_LOCAL_DISCOVERY_GROUP,
                DISCOVERY_UDP_PORT,
                0,
                9
            ))
        );
    }

    #[test]
    fn rejects_ipv6_probe_without_scope() {
        assert_eq!(
            ipv6_endpoint(
                "enp1s0",
                None,
                "fe80::1234".parse().unwrap(),
                "ffff:ffff:ffff:ffff::".parse().unwrap(),
            ),
            None
        );
    }

    #[test]
    fn constants_are_multicast_addresses() {
        assert!(IPV6_LINK_LOCAL_DISCOVERY_GROUP.is_multicast());
        assert!(IPV6_SITE_LOCAL_DISCOVERY_GROUP.is_multicast());
    }
}
