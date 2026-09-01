use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use if_addrs::{IfAddr, Ifv4Addr, Ifv6Addr, Interface, get_if_addrs};
use ipnet::{IpNet, Ipv4Net, Ipv6Net};

use super::{DiscoveryMethod, ProbeEndpoint};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;

pub(crate) const IPV6_LINK_LOCAL_DISCOVERY_GROUP: Ipv6Addr =
    Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 0x0176);
pub(crate) const IPV6_SITE_LOCAL_DISCOVERY_GROUP: Ipv6Addr =
    Ipv6Addr::new(0xFF05, 0, 0, 0, 0, 0, 0, 0x0176);

// SiliconDust's Windows implementation sends the limited broadcast from each
// interface-bound socket. Other supported hosts use the narrower directed
// subnet broadcast. Both paths retain the same per-interface packet count and
// accepted-source prefix check.
#[cfg(target_os = "windows")]
const USE_LIMITED_IPV4_BROADCAST: bool = true;
#[cfg(not(target_os = "windows"))]
const USE_LIMITED_IPV4_BROADCAST: bool = false;

// Windows supplies OnLinkPrefixLength directly but if-addrs derives its
// compatibility netmask separately. POSIX supplies a native netmask, so keep
// rejecting non-contiguous or internally inconsistent masks there.
#[cfg(target_os = "windows")]
const TRUST_REPORTED_PREFIX_LENGTH: bool = true;
#[cfg(not(target_os = "windows"))]
const TRUST_REPORTED_PREFIX_LENGTH: bool = false;

/// Enumerate standard discovery probes for active, non-tunnel interfaces.
///
/// Point-to-point interfaces are intentionally excluded. Routed discovery
/// has a separate approval and packet-budget path. IPv6 link-local interfaces
/// are also excluded until the selected-device HTTP path supports scope IDs.
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
            IfAddr::V4(address) => ipv4_endpoint(
                &interface.name,
                &address,
                USE_LIMITED_IPV4_BROADCAST,
                TRUST_REPORTED_PREFIX_LENGTH,
            ),
            IfAddr::V6(address) => ipv6_endpoint(
                &interface.name,
                interface.index,
                &address,
                TRUST_REPORTED_PREFIX_LENGTH,
            ),
        };

        if let Some(endpoint) = endpoint {
            endpoints.insert(endpoint);
        }
    }

    endpoints.into_iter().collect()
}

fn ipv4_endpoint(
    interface: &str,
    address: &Ifv4Addr,
    use_limited_broadcast: bool,
    trust_reported_prefix_length: bool,
) -> Option<ProbeEndpoint> {
    if address.ip.is_unspecified()
        || address.ip.is_loopback()
        || address.ip.is_multicast()
        || address.ip == Ipv4Addr::BROADCAST
        || !(1..=30).contains(&address.prefixlen)
    {
        return None;
    }

    // Use the OS-reported prefix length directly. On Windows, if-addrs can
    // expose a valid OnLinkPrefixLength even when its separately derived
    // netmask/broadcast fields are unavailable.
    let network = ipv4_network(address, trust_reported_prefix_length)?;
    let broadcast = if use_limited_broadcast {
        Ipv4Addr::BROADCAST
    } else {
        network.broadcast()
    };

    Some(ProbeEndpoint {
        bind: SocketAddr::V4(SocketAddrV4::new(address.ip, 0)),
        destination: SocketAddr::V4(SocketAddrV4::new(broadcast, DISCOVERY_UDP_PORT)),
        method: DiscoveryMethod::Ipv4Broadcast,
        interface: Some(interface.to_owned()),
        accepted_source_network: Some(IpNet::V4(network)),
    })
}

fn ipv4_network(address: &Ifv4Addr, trust_reported_prefix_length: bool) -> Option<Ipv4Net> {
    let reported = Ipv4Net::new(address.ip, address.prefixlen).ok()?.trunc();
    if trust_reported_prefix_length {
        return Some(reported);
    }

    let masked = Ipv4Net::with_netmask(address.ip, address.netmask)
        .ok()?
        .trunc();
    (masked == reported).then_some(reported)
}

fn ipv6_endpoint(
    interface: &str,
    interface_index: Option<u32>,
    address: &Ifv6Addr,
    trust_reported_prefix_length: bool,
) -> Option<ProbeEndpoint> {
    if address.ip.is_unspecified()
        || address.ip.is_loopback()
        || address.ip.is_multicast()
        || address.ip.is_unicast_link_local()
    {
        return None;
    }

    // Link-local discovery is deliberately omitted until the HTTP layer can
    // retain and use an IPv6 scope identifier. Advertising an unusable
    // link-local-only row would otherwise make device selection fail before
    // any lineup request is attempted.
    let scope_id = interface_index?;
    let network = ipv6_network(address, trust_reported_prefix_length)?;

    Some(ProbeEndpoint {
        bind: SocketAddr::V6(SocketAddrV6::new(address.ip, 0, 0, scope_id)),
        destination: SocketAddr::V6(SocketAddrV6::new(
            IPV6_SITE_LOCAL_DISCOVERY_GROUP,
            DISCOVERY_UDP_PORT,
            0,
            scope_id,
        )),
        method: DiscoveryMethod::Ipv6SiteLocalMulticast,
        interface: Some(interface.to_owned()),
        accepted_source_network: Some(IpNet::V6(network)),
    })
}

fn ipv6_network(address: &Ifv6Addr, trust_reported_prefix_length: bool) -> Option<Ipv6Net> {
    let reported = Ipv6Net::new(address.ip, address.prefixlen).ok()?.trunc();
    if trust_reported_prefix_length {
        return Some(reported);
    }

    let masked = Ipv6Net::with_netmask(address.ip, address.netmask)
        .ok()?
        .trunc();
    (masked == reported).then_some(reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn windows_shaped_ipv4() -> Ifv4Addr {
        Ifv4Addr {
            ip: "192.0.2.20".parse().unwrap(),
            // if-addrs can retain the direct Windows prefix even when its
            // separately derived compatibility fields are absent.
            netmask: Ipv4Addr::UNSPECIFIED,
            prefixlen: 24,
            broadcast: None,
        }
    }

    fn posix_shaped_ipv4() -> Ifv4Addr {
        Ifv4Addr {
            ip: "192.0.2.20".parse().unwrap(),
            netmask: "255.255.255.0".parse().unwrap(),
            prefixlen: 24,
            broadcast: Some("192.0.2.255".parse().unwrap()),
        }
    }

    fn ula_ipv6() -> Ifv6Addr {
        Ifv6Addr {
            ip: "fd12:3456::1".parse().unwrap(),
            netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
            prefixlen: 64,
            broadcast: None,
        }
    }

    #[test]
    fn builds_directed_ipv4_probe_from_reported_prefix() {
        let endpoint =
            ipv4_endpoint("ethernet", &posix_shaped_ipv4(), false, false).expect("IPv4 endpoint");

        assert_eq!(endpoint.bind, "192.0.2.20:0".parse().unwrap());
        assert_eq!(endpoint.destination, "192.0.2.255:65001".parse().unwrap());
        assert_eq!(
            endpoint.accepted_source_network,
            Some("192.0.2.0/24".parse().unwrap())
        );
    }

    #[test]
    fn builds_windows_style_limited_broadcast_with_strict_source_prefix() {
        let endpoint =
            ipv4_endpoint("ethernet", &windows_shaped_ipv4(), true, true).expect("IPv4 endpoint");

        assert_eq!(
            endpoint.destination,
            "255.255.255.255:65001".parse().unwrap()
        );
        assert_eq!(
            endpoint.accepted_source_network,
            Some("192.0.2.0/24".parse().unwrap())
        );
    }

    #[test]
    fn rejects_prefixes_without_broadcast_neighbor_semantics() {
        for prefixlen in [0, 31, 32, 33] {
            let mut address = windows_shaped_ipv4();
            address.prefixlen = prefixlen;
            assert_eq!(ipv4_endpoint("ethernet", &address, true, true), None);
        }
    }

    #[test]
    fn posix_projection_rejects_noncontiguous_or_inconsistent_netmasks() {
        let mut address = posix_shaped_ipv4();
        address.netmask = "255.0.255.0".parse().unwrap();
        assert_eq!(ipv4_endpoint("ethernet", &address, false, false), None);

        address.netmask = "255.255.0.0".parse().unwrap();
        assert_eq!(ipv4_endpoint("ethernet", &address, false, false), None);
    }

    #[test]
    fn omits_link_local_ipv6_until_scoped_http_is_supported() {
        let address = Ifv6Addr {
            ip: "fe80::1234".parse().unwrap(),
            netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
            prefixlen: 64,
            broadcast: None,
        };
        assert_eq!(ipv6_endpoint("ethernet", Some(7), &address, false), None);
    }

    #[test]
    fn builds_scoped_site_local_ipv6_probe() {
        let endpoint =
            ipv6_endpoint("enp1s0", Some(9), &ula_ipv6(), false).expect("site-local endpoint");

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
        assert_eq!(ipv6_endpoint("enp1s0", None, &ula_ipv6(), false), None);
    }

    #[test]
    fn constants_are_multicast_addresses() {
        assert!(IPV6_LINK_LOCAL_DISCOVERY_GROUP.is_multicast());
        assert!(IPV6_SITE_LOCAL_DISCOVERY_GROUP.is_multicast());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_interface_projection_selects_limited_broadcast_from_prefix_length() {
        use if_addrs::IfOperStatus;

        let active = Interface {
            name: "Ethernet".to_owned(),
            addr: IfAddr::V4(windows_shaped_ipv4()),
            index: Some(7),
            oper_status: IfOperStatus::Up,
            is_p2p: false,
            adapter_name: "synthetic-adapter".to_owned(),
        };
        let endpoints = endpoints_from_interfaces(vec![active.clone()]);

        assert_eq!(endpoints.len(), 1);
        assert_eq!(
            endpoints[0].destination,
            "255.255.255.255:65001".parse().unwrap()
        );
        assert_eq!(
            endpoints[0].accepted_source_network,
            Some("192.0.2.0/24".parse().unwrap())
        );

        let mut down = active.clone();
        down.oper_status = IfOperStatus::Down;
        assert!(endpoints_from_interfaces(vec![down]).is_empty());

        let mut point_to_point = active;
        point_to_point.is_p2p = true;
        assert!(endpoints_from_interfaces(vec![point_to_point]).is_empty());
    }
}
