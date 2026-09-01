//! Linux rtnetlink-backed route snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::io::Cursor;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use neli::FromBytesWithInput;
use neli::consts::{
    nl::NlmF,
    rtnl::{Arphrd, Ifa, Iff, Ifla, IflaInfo, RtAddrFamily, RtScope, RtTable, Rta, Rtm, Rtn},
    socket::NlFamily,
};
use neli::nl::NlPayload;
use neli::router::asynchronous::NlRouter;
use neli::rtnl::{
    Ifaddrmsg, IfaddrmsgBuilder, Ifinfomsg, IfinfomsgBuilder, Rtattr, Rtmsg, RtmsgBuilder,
};
use neli::types::{Buffer, RtBuffer};
use neli::utils::Groups;

use super::{
    InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteProvider,
    RouteScope, RouteSnapshot,
};

mod monitor;

const MAIN_TABLE: u32 = 254;
const LOCAL_TABLE: u32 = 255;
const DEFAULT_TABLE: u32 = 253;
const COMPAT_TABLE: u32 = 252;

// These retained-row limits bound model memory and parsing work even on
// container-heavy desktops. neli's async router also bounds each in-flight
// response queue; the whole-snapshot deadline bounds time spent draining data
// the kernel returns outside these retained sets. The limits are intentionally
// far above ordinary workstation snapshots while still rejecting
// routing-daemon-scale state that this viewer should not model.
const MAX_LINK_ROWS: usize = 8_192;
const MAX_ADDRESS_ROWS: usize = 32_768;
const MAX_ROUTE_DUMP_ROWS: usize = 131_072;
const MAX_RELEVANT_ROUTE_ROWS: usize = 65_536;
const MAX_RULE_ROWS: usize = 4_096;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

// Linux UAPI values from <linux/if_arp.h>. neli intentionally exposes only a
// subset of ARPHRD constants, so keep the small set used for classification
// local rather than adding another public dependency.
const ARPHRD_TUNNEL: u16 = 768;
const ARPHRD_TUNNEL6: u16 = 769;
const ARPHRD_SIT: u16 = 776;
const ARPHRD_IPGRE: u16 = 778;

// Linux UAPI values from <linux/if_link.h> and <linux/if_tun.h>.
const IFLA_TUN_TYPE: u16 = 3;
const IFF_TUN: u8 = 1;

// Linux UAPI values from <linux/fib_rules.h>. neli 0.7 does not expose the
// fib-rule header or its attribute namespace, so that one response type is
// decoded from a bounds-checked byte buffer below.
const FR_ACT_TO_TBL: u8 = 1;
const FRA_PRIORITY: u16 = 6;
const FRA_SUPPRESS_IFGROUP: u16 = 13;
const FRA_SUPPRESS_PREFIXLEN: u16 = 14;
const FRA_TABLE: u16 = 15;
const FRA_PAD: u16 = 18;
const FRA_PROTOCOL: u16 = 21;
const FIB_RULE_HEADER_LEN: usize = 12;
const NLA_TYPE_MASK: u16 = 0x3fff;
const NLA_FLAGS_MASK: u16 = !NLA_TYPE_MASK;

/// Reads Linux interfaces and effective IPv4 routes through rtnetlink.
///
/// The provider accepts the kernel's canonical `local`, `main`, and `default`
/// IPv4 rule chain. It fails closed when policy routing, VRFs, packet marks,
/// rule selectors, or rules that select custom route tables are present
/// because this socket-independent abstraction cannot reproduce those lookups
/// safely. Unreferenced custom tables are inert under the accepted rule chain.
/// Collection presents the synchronous [`RouteProvider`] contract, but drives
/// neli's bounded asynchronous response queues on a private current-thread
/// runtime. It has a whole-snapshot deadline and should still run on the
/// blocking worker described by [`RouteProvider`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxRouteProvider;

impl LinuxRouteProvider {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl RouteProvider for LinuxRouteProvider {
    fn snapshot(&self) -> io::Result<RouteSnapshot> {
        let worker = std::thread::Builder::new()
            .name("balun-linux-routes".to_owned())
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|error| {
                        io::Error::other(format!(
                            "failed to create the Linux route snapshot runtime: {error}"
                        ))
                    })?;
                runtime.block_on(async {
                    tokio::time::timeout(SNAPSHOT_TIMEOUT, collect_snapshot())
                        .await
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::TimedOut,
                                "timed out collecting the Linux route snapshot",
                            )
                        })?
                })
            })
            .map_err(|error| {
                io::Error::other(format!(
                    "failed to start the Linux route snapshot worker: {error}"
                ))
            })?;
        worker
            .join()
            .map_err(|_| io::Error::other("the Linux route snapshot worker panicked"))?
    }
}

async fn collect_snapshot() -> io::Result<RouteSnapshot> {
    let (router, _multicast) = NlRouter::connect(NlFamily::Route, None, Groups::empty())
        .await
        .map_err(|error| netlink_error("connect to rtnetlink", error))?;
    router
        .enable_strict_checking(true)
        .map_err(|error| netlink_error("enable strict rtnetlink checking", error))?;

    let rules_before = dump_rules(&router).await?;
    let links_before = dump_links(&router).await?;
    let addresses_before = dump_addresses(&router).await?;
    let routes_before = dump_routes(&router).await?;
    let routes_after = dump_routes(&router).await?;
    let addresses_after = dump_addresses(&router).await?;
    let links_after = dump_links(&router).await?;
    let rules_after = dump_rules(&router).await?;

    if rules_before != rules_after
        || links_before != links_after
        || addresses_before != addresses_after
        || routes_before != routes_after
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Linux network configuration changed while collecting the route snapshot",
        ));
    }

    normalize_snapshot(RawSnapshot {
        links: links_after,
        addresses: addresses_after,
        routes: routes_after,
        rules: rules_after,
    })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RawLink {
    index: u32,
    name: String,
    hardware_type: u16,
    flags: u32,
    kind: Option<String>,
    tun_type: Option<u8>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RawAddress {
    interface: u32,
    network: IpNet,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RawRoute {
    destination: Ipv4Net,
    table: u32,
    interface: Option<u32>,
    priority: u32,
    kind: RouteKind,
    scope: RouteScope,
    supported_next_hop: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RawRule {
    priority: u32,
    table: u32,
    action: u8,
    has_selectors: bool,
    reserved_fields_are_zero: bool,
    flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawSnapshot {
    links: Vec<RawLink>,
    addresses: Vec<RawAddress>,
    routes: Vec<RawRoute>,
    rules: Vec<RawRule>,
}

fn normalize_snapshot(raw: RawSnapshot) -> io::Result<RouteSnapshot> {
    validate_policy_rules(&raw.rules)?;

    let mut addresses = BTreeMap::<u32, BTreeSet<IpNet>>::new();
    for address in raw.addresses {
        addresses
            .entry(address.interface)
            .or_default()
            .insert(address.network.trunc());
    }

    let mut interface_ids = BTreeSet::new();
    let mut interfaces = Vec::new();
    for link in raw.links {
        if !interface_ids.insert(link.index) {
            return Err(invalid_data("Linux link dump repeated an interface index"));
        }
        let link_addresses = addresses.remove(&link.index).unwrap_or_default();
        interfaces.push(NetworkInterface::new(
            InterfaceId::new(u64::from(link.index)),
            link.name,
            classify_link(
                link.hardware_type,
                link.flags,
                link.kind.as_deref(),
                link.tun_type,
            ),
            link.flags & Iff::UP.bits() != 0,
            link_addresses,
        ));
    }
    if !addresses.is_empty() {
        return Err(invalid_data(
            "Linux address dump referenced an unknown interface",
        ));
    }

    let mut local_routes_by_prefix = BTreeMap::<Ipv4Net, Vec<RawRoute>>::new();
    let mut main_routes_by_prefix = BTreeMap::<Ipv4Net, Vec<RawRoute>>::new();
    for route in raw.routes {
        if route
            .interface
            .is_some_and(|index| !interface_ids.contains(&index))
        {
            return Err(invalid_data(
                "Linux route dump referenced an unknown interface",
            ));
        }
        match route.table {
            LOCAL_TABLE => {
                if route.destination.prefix_len() < 32 && overlaps_rfc1918(route.destination) {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "a broad Linux local-table route overlaps private discovery space",
                    ));
                }
                local_routes_by_prefix
                    .entry(route.destination.trunc())
                    .or_default()
                    .push(route);
            }
            MAIN_TABLE => {
                main_routes_by_prefix
                    .entry(route.destination.trunc())
                    .or_default()
                    .push(route);
            }
            DEFAULT_TABLE => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the Linux default route table is nonempty and cannot be composed safely",
                ));
            }
            _ => {}
        }
    }

    let mut effective_routes = Vec::new();
    // The canonical local-table rule precedes main. Standard local-table
    // entries that can overlap discovery space are host routes, so exact
    // non-candidate blockers correctly model that precedence. Broader private
    // entries were rejected above because this flat snapshot cannot express
    // table priority over a more-specific main-table route.
    for routes in local_routes_by_prefix.values() {
        let Some(best_priority) = routes.iter().map(|route| route.priority).min() else {
            continue;
        };
        if routes.iter().any(|route| route.priority == best_priority) {
            effective_routes.push(NetworkRoute::effective(
                IpNet::V4(routes[0].destination),
                None,
                RouteKind::Other,
                RouteScope::Other,
            ));
        }
    }

    for routes in main_routes_by_prefix.values() {
        let Some(best_priority) = routes.iter().map(|route| route.priority).min() else {
            continue;
        };
        let winners = routes
            .iter()
            .filter(|route| route.priority == best_priority)
            .collect::<Vec<_>>();

        // A multipath/nexthop-object winner cannot safely be reduced to this
        // abstraction. Preserve its prefix as a non-candidate blocker so a
        // broader route cannot accidentally become effective underneath it.
        if winners.iter().any(|route| !route.supported_next_hop) {
            effective_routes.push(NetworkRoute::effective(
                IpNet::V4(routes[0].destination),
                None,
                RouteKind::Other,
                RouteScope::Other,
            ));
            continue;
        }

        effective_routes.extend(winners.into_iter().map(|route| {
            NetworkRoute::effective(
                IpNet::V4(route.destination),
                route
                    .interface
                    .map(|index| InterfaceId::new(u64::from(index))),
                route.kind,
                route.scope,
            )
        }));
    }

    Ok(RouteSnapshot::from_effective_routes(
        interfaces,
        effective_routes,
    ))
}

fn overlaps_rfc1918(network: Ipv4Net) -> bool {
    [
        Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 8).expect("valid RFC 1918 network"),
        Ipv4Net::new(Ipv4Addr::new(172, 16, 0, 0), 12).expect("valid RFC 1918 network"),
        Ipv4Net::new(Ipv4Addr::new(192, 168, 0, 0), 16).expect("valid RFC 1918 network"),
    ]
    .into_iter()
    .any(|private| private.contains(&network.network()) || network.contains(&private.network()))
}

fn validate_policy_rules(rules: &[RawRule]) -> io::Result<()> {
    let mut actual = rules.to_vec();
    actual.sort();

    if actual.iter().any(|rule| {
        rule.action != FR_ACT_TO_TBL
            || rule.has_selectors
            || !rule.reserved_fields_are_zero
            || rule.flags != 0
    }) {
        return Err(unsupported_policy());
    }

    let actual = actual
        .into_iter()
        .map(|rule| (rule.priority, rule.table))
        .collect::<Vec<_>>();
    let expected = vec![
        (0, LOCAL_TABLE),
        (32_766, MAIN_TABLE),
        (32_767, DEFAULT_TABLE),
    ];
    if actual != expected {
        return Err(unsupported_policy());
    }

    Ok(())
}

fn unsupported_policy() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Linux policy routing is not the canonical local/main/default rule chain",
    )
}

fn classify_link(
    hardware_type: u16,
    flags: u32,
    kind: Option<&str>,
    tun_type: Option<u8>,
) -> InterfaceKind {
    if hardware_type == u16::from(Arphrd::Loopback) || flags & Iff::LOOPBACK.bits() != 0 {
        return InterfaceKind::Loopback;
    }

    if kind == Some("tun") {
        return if tun_type == Some(IFF_TUN) {
            InterfaceKind::Tunnel
        } else {
            InterfaceKind::Other
        };
    }
    let is_tunnel_kind = matches!(
        kind,
        Some("wireguard" | "gre" | "ip6gre" | "ipip" | "sit" | "ip6tnl" | "vti" | "vti6" | "xfrm")
    );
    let is_tunnel_hardware = matches!(
        hardware_type,
        ARPHRD_TUNNEL | ARPHRD_TUNNEL6 | ARPHRD_SIT | ARPHRD_IPGRE
    );
    if is_tunnel_kind || (kind.is_none() && is_tunnel_hardware) {
        InterfaceKind::Tunnel
    } else {
        InterfaceKind::Other
    }
}

async fn dump_links(router: &NlRouter) -> io::Result<Vec<RawLink>> {
    let request = IfinfomsgBuilder::default()
        .ifi_family(RtAddrFamily::Unspecified)
        .build()
        .map_err(|error| netlink_error("build link request", error))?;
    let mut responses = router
        .send::<_, _, Rtm, Ifinfomsg>(Rtm::Getlink, NlmF::DUMP, NlPayload::Payload(request))
        .await
        .map_err(|error| netlink_error("request Linux links", error))?;

    let mut links = Vec::new();
    while let Some(response) = responses.next::<Rtm, Ifinfomsg>().await {
        let response = response.map_err(|error| netlink_error("read Linux links", error))?;
        ensure_dump_was_not_interrupted(response.nl_flags())?;
        let Some(payload) = response.get_payload() else {
            continue;
        };
        if response.nl_type() != &Rtm::Newlink {
            return Err(invalid_data("unexpected message in Linux link dump"));
        }
        let index = u32::try_from(*payload.ifi_index())
            .ok()
            .filter(|index| *index != 0)
            .ok_or_else(|| invalid_data("Linux interface index was not positive"))?;

        let mut name = None;
        let mut kind = None;
        let mut tun_type = None;
        let mut saw_link_info = false;
        for attribute in payload.rtattrs().iter() {
            let raw_type = u16::from(attribute.rta_type());
            match raw_type & NLA_TYPE_MASK {
                value if value == u16::from(Ifla::Ifname) => {
                    require_no_attribute_flags(raw_type, "interface name")?;
                    set_once_string(
                        &mut name,
                        attribute.rta_payload().as_ref(),
                        16,
                        "interface name",
                    )?;
                }
                value if value == u16::from(Ifla::Linkinfo) => {
                    if saw_link_info {
                        return Err(invalid_data(
                            "Linux netlink response repeated interface link information",
                        ));
                    }
                    saw_link_info = true;
                    if raw_type & NLA_FLAGS_MASK & !0x8000 != 0 {
                        return Err(invalid_data(
                            "Linux interface link information had unsupported flags",
                        ));
                    }
                    (kind, tun_type) = decode_link_info(attribute)?;
                }
                _ => {}
            }
        }
        let name = name.ok_or_else(|| invalid_data("Linux interface omitted its name"))?;

        push_bounded(
            &mut links,
            RawLink {
                index,
                name,
                hardware_type: u16::from(payload.ifi_type()),
                flags: payload.ifi_flags().bits(),
                kind,
                tun_type,
            },
            MAX_LINK_ROWS,
            "links",
        )?;
    }
    links.sort();
    Ok(links)
}

fn decode_link_info(attribute: &Rtattr<Ifla, Buffer>) -> io::Result<(Option<String>, Option<u8>)> {
    let attributes = attribute
        .get_attr_handle::<IflaInfo>()
        .map_err(|error| netlink_error("decode Linux interface link information", error))?;
    let mut kind = None;
    let mut data = None;
    for attribute in attributes.iter() {
        let raw_type = u16::from(attribute.rta_type());
        match raw_type & NLA_TYPE_MASK {
            value if value == u16::from(IflaInfo::Kind) => {
                require_no_attribute_flags(raw_type, "interface link kind")?;
                set_once_string(
                    &mut kind,
                    attribute.rta_payload().as_ref(),
                    64,
                    "interface link kind",
                )?;
            }
            value if value == u16::from(IflaInfo::Data) => {
                if raw_type & NLA_FLAGS_MASK & !0x8000 != 0 {
                    return Err(invalid_data(
                        "Linux interface link data had unsupported flags",
                    ));
                }
                set_once_bytes(
                    &mut data,
                    attribute.rta_payload().as_ref(),
                    "interface link data",
                )?;
            }
            _ => {}
        }
    }
    let tun_type = if kind.as_deref() == Some("tun") {
        data.map(decode_tun_type).transpose()?.flatten()
    } else {
        None
    };
    Ok((kind, tun_type))
}

fn decode_tun_type(bytes: &[u8]) -> io::Result<Option<u8>> {
    let attributes =
        RtBuffer::<u16, Buffer>::from_bytes_with_input(&mut Cursor::new(bytes), bytes.len())
            .map_err(|error| netlink_error("decode Linux TUN/TAP link data", error))?;
    let mut tun_type = None;
    for attribute in attributes.iter() {
        let raw_type = *attribute.rta_type();
        if raw_type & NLA_TYPE_MASK == IFLA_TUN_TYPE {
            require_no_attribute_flags(raw_type, "TUN/TAP interface type")?;
            if tun_type.is_some() {
                return Err(invalid_data(
                    "Linux netlink response repeated TUN/TAP interface type",
                ));
            }
            let [value] = attribute.rta_payload().as_ref() else {
                return Err(invalid_data(
                    "Linux TUN/TAP interface type had the wrong length",
                ));
            };
            tun_type = Some(*value);
        }
    }
    Ok(tun_type)
}

async fn dump_addresses(router: &NlRouter) -> io::Result<Vec<RawAddress>> {
    let request = IfaddrmsgBuilder::default()
        .ifa_family(RtAddrFamily::Unspecified)
        .ifa_prefixlen(0)
        .ifa_scope(RtScope::Universe)
        .ifa_index(0)
        .build()
        .map_err(|error| netlink_error("build address request", error))?;
    let mut responses = router
        .send::<_, _, Rtm, Ifaddrmsg>(Rtm::Getaddr, NlmF::DUMP, NlPayload::Payload(request))
        .await
        .map_err(|error| netlink_error("request Linux interface addresses", error))?;

    let mut addresses = Vec::new();
    while let Some(response) = responses.next::<Rtm, Ifaddrmsg>().await {
        let response =
            response.map_err(|error| netlink_error("read Linux interface addresses", error))?;
        ensure_dump_was_not_interrupted(response.nl_flags())?;
        let Some(payload) = response.get_payload() else {
            continue;
        };
        if response.nl_type() != &Rtm::Newaddr {
            return Err(invalid_data(
                "unexpected message in Linux interface-address dump",
            ));
        }
        if !matches!(
            payload.ifa_family(),
            RtAddrFamily::Inet | RtAddrFamily::Inet6
        ) {
            continue;
        }
        if *payload.ifa_index() == 0 {
            return Err(invalid_data("Linux interface address had index zero"));
        }

        let mut local = None;
        let mut address = None;
        for attribute in payload.rtattrs().iter() {
            let raw_type = u16::from(attribute.rta_type());
            match raw_type & NLA_TYPE_MASK {
                value if value == u16::from(Ifa::Local) => {
                    require_no_attribute_flags(raw_type, "local interface address")?;
                    set_once_bytes(
                        &mut local,
                        attribute.rta_payload().as_ref(),
                        "local interface address",
                    )?;
                }
                value if value == u16::from(Ifa::Address) => {
                    require_no_attribute_flags(raw_type, "interface address")?;
                    set_once_bytes(
                        &mut address,
                        attribute.rta_payload().as_ref(),
                        "interface address",
                    )?;
                }
                _ => {}
            }
        }
        let bytes = match payload.ifa_family() {
            RtAddrFamily::Inet => local.or(address),
            RtAddrFamily::Inet6 => address,
            _ => unreachable!("address family was filtered above"),
        }
        .ok_or_else(|| invalid_data("Linux interface address omitted its address attribute"))?;
        let network = decode_network(*payload.ifa_family(), *payload.ifa_prefixlen(), bytes)?;
        push_bounded(
            &mut addresses,
            RawAddress {
                interface: *payload.ifa_index(),
                network,
            },
            MAX_ADDRESS_ROWS,
            "interface addresses",
        )?;
    }
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

async fn dump_routes(router: &NlRouter) -> io::Result<Vec<RawRoute>> {
    let request = route_request()?;
    let mut responses = router
        .send::<_, _, Rtm, Rtmsg>(Rtm::Getroute, NlmF::DUMP, NlPayload::Payload(request))
        .await
        .map_err(|error| netlink_error("request Linux IPv4 routes", error))?;

    let mut routes = Vec::new();
    let mut route_rows_seen = 0;
    while let Some(response) = responses.next::<Rtm, Rtmsg>().await {
        let response = response.map_err(|error| netlink_error("read Linux IPv4 routes", error))?;
        ensure_dump_was_not_interrupted(response.nl_flags())?;
        let Some(payload) = response.get_payload() else {
            continue;
        };
        if response.nl_type() != &Rtm::Newroute {
            return Err(invalid_data("unexpected message in Linux route dump"));
        }
        if payload.rtm_family() != &RtAddrFamily::Inet {
            continue;
        }
        count_bounded(&mut route_rows_seen, MAX_ROUTE_DUMP_ROWS, "IPv4 route rows")?;

        let mut destination = None;
        let mut extended_table = None;
        let mut interface = None;
        let mut priority = None;
        let mut gateway = None;
        let mut supported_next_hop = true;
        for attribute in payload.rtattrs().iter() {
            let raw_type = u16::from(attribute.rta_type());
            let attribute_type = raw_type & NLA_TYPE_MASK;
            match attribute_type {
                value if value == u16::from(Rta::Dst) => {
                    require_no_attribute_flags(raw_type, "route destination")?;
                    set_once_bytes(
                        &mut destination,
                        attribute.rta_payload().as_ref(),
                        "route destination",
                    )?;
                }
                value if value == u16::from(Rta::Oif) => {
                    require_no_attribute_flags(raw_type, "route output interface")?;
                    set_once_u32(
                        &mut interface,
                        attribute.rta_payload().as_ref(),
                        "route output interface",
                    )?;
                }
                value if value == u16::from(Rta::Gateway) => {
                    require_no_attribute_flags(raw_type, "route gateway")?;
                    set_once_bytes(
                        &mut gateway,
                        attribute.rta_payload().as_ref(),
                        "route gateway",
                    )?;
                }
                value if value == u16::from(Rta::Priority) => {
                    require_no_attribute_flags(raw_type, "route priority")?;
                    set_once_u32(
                        &mut priority,
                        attribute.rta_payload().as_ref(),
                        "route priority",
                    )?;
                }
                value if value == u16::from(Rta::Table) => {
                    require_no_attribute_flags(raw_type, "route table")?;
                    set_once_u32(
                        &mut extended_table,
                        attribute.rta_payload().as_ref(),
                        "route table",
                    )?;
                }
                // PREFSRC, METRICS, legacy metadata, FLOW realms,
                // CACHEINFO, PREF, PAD, TTL propagation, and the
                // IPv6-only FLOWLABEL do not alter an IPv4 output interface.
                value if harmless_ipv4_route_metadata(value) => {}
                // IIF, MULTIPATH, MARK, VIA, NEWDST, encapsulation, UID,
                // protocol/port selectors, NH_ID, and future attributes need
                // flow or nested-next-hop information this provider does not
                // guess. Treat unknown values as unsupported too.
                _ => supported_next_hop = false,
            }
        }

        let header_table = u32::from(u8::from(payload.rtm_table()));
        if extended_table.is_some_and(|table| !table_fields_are_compatible(header_table, table)) {
            return Err(invalid_data(
                "Linux route header and extended table disagreed",
            ));
        }
        let table = extended_table.unwrap_or(header_table);
        if !matches!(table, LOCAL_TABLE | MAIN_TABLE | DEFAULT_TABLE) {
            continue;
        }
        if *payload.rtm_src_len() != 0 || *payload.rtm_tos() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source- or TOS-specific Linux routes cannot be modeled safely",
            ));
        }

        let destination = match destination {
            Some(bytes) => decode_ipv4_net(*payload.rtm_dst_len(), bytes)?,
            None if *payload.rtm_dst_len() == 0 => Ipv4Net::new(Ipv4Addr::UNSPECIFIED, 0)
                .map_err(|error| invalid_data(error.to_string()))?,
            None => return Err(invalid_data("IPv4 route omitted its destination attribute")),
        };
        if let Some(bytes) = gateway {
            decode_ipv4_address(bytes)?;
        }
        if interface == Some(0) {
            return Err(invalid_data("Linux route output interface was zero"));
        }
        let kind = if payload.rtm_type() == &Rtn::Unicast {
            RouteKind::Unicast
        } else {
            RouteKind::Other
        };
        if kind == RouteKind::Unicast && interface.is_none() {
            supported_next_hop = false;
        }
        supported_next_hop &= payload.rtm_flags().is_empty();
        let scope = match (payload.rtm_scope(), gateway.is_some(), interface) {
            (RtScope::Link, false, Some(_)) => RouteScope::OnLink,
            (RtScope::Universe | RtScope::Site, true, Some(_)) => RouteScope::ViaGateway,
            _ => RouteScope::Other,
        };

        push_bounded(
            &mut routes,
            RawRoute {
                destination,
                table,
                interface,
                priority: priority.unwrap_or(0),
                kind,
                scope,
                supported_next_hop,
            },
            MAX_RELEVANT_ROUTE_ROWS,
            "relevant routes",
        )?;
    }
    routes.sort();
    Ok(routes)
}

fn harmless_ipv4_route_metadata(attribute_type: u16) -> bool {
    // Deliberately exclude UNSPEC, EXPIRES, and every unrecognized value. An
    // expiring or future route attribute might change next-hop selection and
    // therefore has to become a non-candidate prefix blocker.
    matches!(
        attribute_type,
        7 | 8 | 10 | 11 | 12 | 13 | 14 | 17 | 20 | 24 | 26 | 31
    )
}

async fn dump_rules(router: &NlRouter) -> io::Result<Vec<RawRule>> {
    let mut header = [0_u8; FIB_RULE_HEADER_LEN];
    header[0] = u8::from(RtAddrFamily::Inet);
    let mut responses = router
        .send::<_, _, Rtm, Buffer>(
            Rtm::Getrule,
            NlmF::DUMP,
            NlPayload::Payload(Buffer::from(header.as_slice())),
        )
        .await
        .map_err(|error| netlink_error("request Linux IPv4 policy rules", error))?;

    let mut rules = Vec::new();
    while let Some(response) = responses.next::<Rtm, Buffer>().await {
        let response =
            response.map_err(|error| netlink_error("read Linux IPv4 policy rules", error))?;
        ensure_dump_was_not_interrupted(response.nl_flags())?;
        let Some(payload) = response.get_payload() else {
            continue;
        };
        if response.nl_type() != &Rtm::Newrule {
            return Err(invalid_data("unexpected message in Linux policy-rule dump"));
        }
        if let Some(rule) = parse_rule_payload(payload.as_ref())? {
            push_bounded(&mut rules, rule, MAX_RULE_ROWS, "policy rules")?;
        }
    }
    rules.sort();
    Ok(rules)
}

fn parse_rule_payload(bytes: &[u8]) -> io::Result<Option<RawRule>> {
    if bytes.len() < FIB_RULE_HEADER_LEN {
        return Err(invalid_data("Linux policy-rule header was truncated"));
    }
    if bytes[0] != u8::from(RtAddrFamily::Inet) {
        return Ok(None);
    }

    let attributes = RtBuffer::<u16, Buffer>::from_bytes_with_input(
        &mut Cursor::new(&bytes[FIB_RULE_HEADER_LEN..]),
        bytes.len() - FIB_RULE_HEADER_LEN,
    )
    .map_err(|error| netlink_error("decode Linux policy-rule attributes", error))?;
    let mut priority = None;
    let mut extended_table = None;
    let mut suppress_ifgroup = None;
    let mut suppress_prefix_len = None;
    let mut has_selectors = bytes[1] != 0 || bytes[2] != 0;
    for attribute in attributes.iter() {
        let raw_type = *attribute.rta_type();
        let attribute_type = raw_type & NLA_TYPE_MASK;
        match attribute_type {
            FRA_PRIORITY => {
                require_no_attribute_flags(raw_type, "policy-rule priority")?;
                set_once_u32(
                    &mut priority,
                    attribute.rta_payload().as_ref(),
                    "policy-rule priority",
                )?;
            }
            FRA_TABLE => {
                require_no_attribute_flags(raw_type, "policy-rule table")?;
                set_once_u32(
                    &mut extended_table,
                    attribute.rta_payload().as_ref(),
                    "policy-rule table",
                )?;
            }
            FRA_SUPPRESS_IFGROUP => {
                require_no_attribute_flags(raw_type, "policy-rule suppressed interface group")?;
                set_once_u32(
                    &mut suppress_ifgroup,
                    attribute.rta_payload().as_ref(),
                    "policy-rule suppressed interface group",
                )?;
            }
            FRA_SUPPRESS_PREFIXLEN => {
                require_no_attribute_flags(raw_type, "policy-rule suppressed prefix length")?;
                set_once_u32(
                    &mut suppress_prefix_len,
                    attribute.rta_payload().as_ref(),
                    "policy-rule suppressed prefix length",
                )?;
            }
            FRA_PAD => {
                require_no_attribute_flags(raw_type, "policy-rule padding")?;
                if !attribute.rta_payload().is_empty() {
                    return Err(invalid_data(
                        "Linux policy-rule padding attribute was not empty",
                    ));
                }
            }
            FRA_PROTOCOL => {
                require_no_attribute_flags(raw_type, "policy-rule protocol")?;
                if attribute.rta_payload().len() != 1 {
                    return Err(invalid_data(
                        "Linux policy-rule protocol had the wrong length",
                    ));
                }
            }
            _ => has_selectors = true,
        }
    }
    has_selectors |= suppress_ifgroup.is_some_and(|value| value != u32::MAX)
        || suppress_prefix_len.is_some_and(|value| value != u32::MAX);

    let flags = u32::from_ne_bytes(
        bytes[8..12]
            .try_into()
            .expect("fixed-length policy-rule flag slice"),
    );
    let header_table = u32::from(bytes[4]);
    if extended_table.is_some_and(|table| !table_fields_are_compatible(header_table, table)) {
        return Err(invalid_data(
            "Linux policy-rule header and extended table disagreed",
        ));
    }
    Ok(Some(RawRule {
        priority: priority.unwrap_or(0),
        table: extended_table.unwrap_or(header_table),
        action: bytes[7],
        has_selectors,
        reserved_fields_are_zero: bytes[3] == 0 && bytes[5] == 0 && bytes[6] == 0,
        flags,
    }))
}

fn route_request() -> io::Result<Rtmsg> {
    RtmsgBuilder::default()
        .rtm_family(RtAddrFamily::Inet)
        .rtm_dst_len(0)
        .rtm_src_len(0)
        .rtm_tos(0)
        .rtm_table(RtTable::Unspec)
        .rtm_protocol(neli::consts::rtnl::Rtprot::Unspec)
        .rtm_scope(RtScope::Universe)
        .rtm_type(Rtn::Unspec)
        .build()
        .map_err(|error| netlink_error("build route request", error))
}

fn table_fields_are_compatible(header: u32, extended: u32) -> bool {
    header == extended || header == 0 || (header == COMPAT_TABLE && extended > u32::from(u8::MAX))
}

fn push_bounded<T>(rows: &mut Vec<T>, row: T, maximum: usize, description: &str) -> io::Result<()> {
    if rows.len() >= maximum {
        return Err(invalid_data(format!(
            "Linux rtnetlink dump exceeded the {description} limit of {maximum} rows"
        )));
    }
    rows.push(row);
    Ok(())
}

fn count_bounded(count: &mut usize, maximum: usize, description: &str) -> io::Result<()> {
    if *count >= maximum {
        return Err(invalid_data(format!(
            "Linux rtnetlink dump exceeded the {description} limit of {maximum} rows"
        )));
    }
    *count += 1;
    Ok(())
}

fn ensure_dump_was_not_interrupted(flags: &NlmF) -> io::Result<()> {
    if flags.contains(NlmF::DUMP_INTR) {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Linux interrupted a changing rtnetlink dump",
        ))
    } else {
        Ok(())
    }
}

fn require_no_attribute_flags(raw_type: u16, field: &str) -> io::Result<()> {
    if raw_type & NLA_FLAGS_MASK != 0 {
        Err(invalid_data(format!(
            "Linux netlink {field} attribute had unsupported flags"
        )))
    } else {
        Ok(())
    }
}

fn set_once_bytes<'a>(slot: &mut Option<&'a [u8]>, bytes: &'a [u8], field: &str) -> io::Result<()> {
    if slot.replace(bytes).is_some() {
        Err(invalid_data(format!(
            "Linux netlink response repeated {field}"
        )))
    } else {
        Ok(())
    }
}

fn set_once_string(
    slot: &mut Option<String>,
    bytes: &[u8],
    maximum_len: usize,
    field: &str,
) -> io::Result<()> {
    if slot.is_some() {
        return Err(invalid_data(format!(
            "Linux netlink response repeated {field}"
        )));
    }
    if bytes.is_empty() || bytes.len() > maximum_len || bytes.last() != Some(&0) {
        return Err(invalid_data(format!(
            "Linux netlink {field} was not a bounded NUL-terminated string"
        )));
    }
    let value = &bytes[..bytes.len() - 1];
    if value.contains(&0) {
        return Err(invalid_data(format!(
            "Linux netlink {field} contained an interior NUL"
        )));
    }
    let value = std::str::from_utf8(value)
        .map_err(|_| invalid_data(format!("Linux netlink {field} was not UTF-8")))?;
    *slot = Some(value.to_owned());
    Ok(())
}

fn set_once_u32(slot: &mut Option<u32>, bytes: &[u8], field: &str) -> io::Result<()> {
    if slot.is_some() {
        return Err(invalid_data(format!(
            "Linux netlink response repeated {field}"
        )));
    }
    *slot = Some(decode_u32(bytes)?);
    Ok(())
}

fn decode_u32(bytes: &[u8]) -> io::Result<u32> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| invalid_data("Linux netlink u32 attribute had the wrong length"))?;
    Ok(u32::from_ne_bytes(bytes))
}

fn decode_ipv4_address(bytes: &[u8]) -> io::Result<Ipv4Addr> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| invalid_data("Linux netlink IPv4 address had the wrong length"))?;
    Ok(Ipv4Addr::from(bytes))
}

fn decode_network(family: RtAddrFamily, prefix: u8, bytes: &[u8]) -> io::Result<IpNet> {
    match family {
        RtAddrFamily::Inet => decode_ipv4_net(prefix, bytes).map(IpNet::V4),
        RtAddrFamily::Inet6 => {
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| invalid_data("Linux netlink IPv6 address had the wrong length"))?;
            Ipv6Net::new(Ipv6Addr::from(bytes), prefix)
                .map(IpNet::V6)
                .map_err(|error| invalid_data(error.to_string()))
        }
        _ => Err(invalid_data("unsupported Linux address family")),
    }
}

fn decode_ipv4_net(prefix: u8, bytes: &[u8]) -> io::Result<Ipv4Net> {
    Ipv4Net::new(decode_ipv4_address(bytes)?, prefix)
        .map(|network| network.trunc())
        .map_err(|error| invalid_data(error.to_string()))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn netlink_error(context: &str, error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("failed to {context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_link(index: u32, name: &str, kind: Option<&str>, flags: u32) -> RawLink {
        RawLink {
            index,
            name: name.to_owned(),
            hardware_type: u16::from(Arphrd::Ether),
            flags,
            kind: kind.map(str::to_owned),
            tun_type: None,
        }
    }

    fn canonical_rules() -> Vec<RawRule> {
        [
            (0, LOCAL_TABLE),
            (32_766, MAIN_TABLE),
            (32_767, DEFAULT_TABLE),
        ]
        .into_iter()
        .map(|(priority, table)| RawRule {
            priority,
            table,
            action: FR_ACT_TO_TBL,
            has_selectors: false,
            reserved_fields_are_zero: true,
            flags: 0,
        })
        .collect()
    }

    fn raw_route(destination: &str, interface: u32, priority: u32) -> RawRoute {
        RawRoute {
            destination: destination.parse().unwrap(),
            table: MAIN_TABLE,
            interface: Some(interface),
            priority,
            kind: RouteKind::Unicast,
            scope: RouteScope::OnLink,
            supported_next_hop: true,
        }
    }

    fn push_rt_attribute(bytes: &mut Vec<u8>, attribute_type: u16, payload: &[u8]) {
        let length = u16::try_from(4 + payload.len()).unwrap();
        bytes.extend_from_slice(&length.to_ne_bytes());
        bytes.extend_from_slice(&attribute_type.to_ne_bytes());
        bytes.extend_from_slice(payload);
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
    }

    fn policy_rule_fixture(table: u8, priority: u32, suppress_prefix_len: u32) -> Vec<u8> {
        let mut bytes = vec![0_u8; FIB_RULE_HEADER_LEN];
        bytes[0] = u8::from(RtAddrFamily::Inet);
        bytes[4] = table;
        bytes[7] = FR_ACT_TO_TBL;
        push_rt_attribute(&mut bytes, FRA_PRIORITY, &priority.to_ne_bytes());
        push_rt_attribute(
            &mut bytes,
            FRA_SUPPRESS_PREFIXLEN,
            &suppress_prefix_len.to_ne_bytes(),
        );
        push_rt_attribute(&mut bytes, FRA_PROTOCOL, &[2]);
        bytes
    }

    #[test]
    fn wireguard_classification_uses_kernel_link_kind_not_name() {
        assert_eq!(
            classify_link(
                u16::from(Arphrd::Ether),
                Iff::UP.bits(),
                Some("wireguard"),
                None
            ),
            InterfaceKind::Tunnel
        );
        assert_eq!(
            classify_link(
                u16::from(Arphrd::Ether),
                Iff::UP.bits(),
                Some("dummy"),
                None
            ),
            InterfaceKind::Other
        );
        assert_eq!(
            classify_link(u16::from(Arphrd::Ether), Iff::UP.bits(), None, None),
            InterfaceKind::Other
        );
        assert_eq!(
            classify_link(u16::from(Arphrd::Ether), Iff::UP.bits(), Some("l2tp"), None),
            InterfaceKind::Other
        );
    }

    #[test]
    fn kernel_hardware_type_is_a_tunnel_fallback() {
        assert_eq!(
            classify_link(ARPHRD_TUNNEL, Iff::UP.bits(), None, None),
            InterfaceKind::Tunnel
        );
        assert_eq!(
            classify_link(
                u16::from(Arphrd::Loopback),
                Iff::UP.bits() | Iff::LOOPBACK.bits(),
                Some("wireguard"),
                None
            ),
            InterfaceKind::Loopback
        );
        assert_eq!(
            classify_link(ARPHRD_TUNNEL, Iff::UP.bits(), Some("dummy"), None),
            InterfaceKind::Other
        );
    }

    #[test]
    fn tun_link_data_distinguishes_l3_tun_from_l2_tap() {
        let mut tun_data = Vec::new();
        push_rt_attribute(&mut tun_data, IFLA_TUN_TYPE, &[IFF_TUN]);
        let tun_type = decode_tun_type(&tun_data).unwrap();
        assert_eq!(tun_type, Some(IFF_TUN));
        assert_eq!(
            classify_link(
                u16::from(Arphrd::None),
                Iff::UP.bits(),
                Some("tun"),
                tun_type
            ),
            InterfaceKind::Tunnel
        );

        let mut tap_data = Vec::new();
        push_rt_attribute(&mut tap_data, IFLA_TUN_TYPE, &[2]);
        let tap_type = decode_tun_type(&tap_data).unwrap();
        assert_eq!(tap_type, Some(2));
        assert_eq!(
            classify_link(
                u16::from(Arphrd::Ether),
                Iff::UP.bits(),
                Some("tun"),
                tap_type
            ),
            InterfaceKind::Other
        );
        assert_eq!(
            classify_link(u16::from(Arphrd::None), Iff::UP.bits(), Some("tun"), None),
            InterfaceKind::Other
        );

        push_rt_attribute(&mut tun_data, IFLA_TUN_TYPE, &[IFF_TUN]);
        assert_eq!(
            decode_tun_type(&tun_data).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn fixture_normalization_attaches_addresses_and_selects_metric_winners() {
        let snapshot = normalize_snapshot(RawSnapshot {
            links: vec![
                raw_link(4, "office", Some("wireguard"), Iff::UP.bits()),
                raw_link(2, "wg-looking-name", Some("dummy"), Iff::UP.bits()),
            ],
            addresses: vec![
                RawAddress {
                    interface: 4,
                    network: "10.0.0.9/24".parse().unwrap(),
                },
                RawAddress {
                    interface: 2,
                    network: "192.168.1.20/24".parse().unwrap(),
                },
            ],
            routes: vec![
                raw_route("192.168.40.0/24", 4, 100),
                raw_route("192.168.40.0/24", 2, 200),
                raw_route("192.168.41.0/24", 4, 50),
                raw_route("192.168.41.0/24", 2, 50),
            ],
            rules: canonical_rules(),
        })
        .unwrap();

        let tunnel = snapshot
            .interfaces()
            .iter()
            .find(|interface| interface.id() == InterfaceId::new(4))
            .unwrap();
        assert_eq!(tunnel.kind(), InterfaceKind::Tunnel);
        assert_eq!(tunnel.addresses(), &["10.0.0.0/24".parse().unwrap()]);
        assert_eq!(snapshot.effective_routes().len(), 3);
        assert!(snapshot.effective_routes().iter().any(|route| {
            route.destination() == "192.168.40.0/24".parse().unwrap()
                && route.interface() == Some(InterfaceId::new(4))
        }));
    }

    #[test]
    fn unsupported_winning_next_hop_blocks_broader_routes_and_does_not_promote_a_loser() {
        let mut unsupported = raw_route("192.168.50.0/24", 4, 10);
        unsupported.supported_next_hop = false;
        let snapshot = normalize_snapshot(RawSnapshot {
            links: vec![raw_link(4, "wg0", Some("wireguard"), Iff::UP.bits())],
            addresses: vec![],
            routes: vec![unsupported, raw_route("192.168.50.0/24", 4, 20)],
            rules: canonical_rules(),
        })
        .unwrap();

        assert_eq!(snapshot.effective_routes().len(), 1);
        assert_eq!(snapshot.effective_routes()[0].kind(), RouteKind::Other);
        assert_eq!(snapshot.effective_routes()[0].scope(), RouteScope::Other);
        assert_eq!(snapshot.effective_routes()[0].interface(), None);
    }

    #[test]
    fn stock_policy_rule_suppress_sentinel_is_not_a_selector() {
        let fixtures = [
            policy_rule_fixture(LOCAL_TABLE as u8, 0, u32::MAX),
            policy_rule_fixture(MAIN_TABLE as u8, 32_766, u32::MAX),
            policy_rule_fixture(DEFAULT_TABLE as u8, 32_767, u32::MAX),
        ];
        let rules = fixtures
            .iter()
            .map(|bytes| parse_rule_payload(bytes).unwrap().unwrap())
            .collect::<Vec<_>>();

        assert!(rules.iter().all(|rule| !rule.has_selectors));
        validate_policy_rules(&rules).unwrap();
    }

    #[test]
    fn effective_policy_suppression_is_rejected() {
        let mut rules = canonical_rules();
        rules[1] = parse_rule_payload(&policy_rule_fixture(MAIN_TABLE as u8, 32_766, 0))
            .unwrap()
            .unwrap();

        assert!(rules[1].has_selectors);
        assert_eq!(
            validate_policy_rules(&rules).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn policy_rule_parser_rejects_conflicting_tables_and_duplicate_priority() {
        let mut conflicting = policy_rule_fixture(MAIN_TABLE as u8, 32_766, u32::MAX);
        push_rt_attribute(&mut conflicting, FRA_TABLE, &DEFAULT_TABLE.to_ne_bytes());
        assert_eq!(
            parse_rule_payload(&conflicting).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut duplicate = policy_rule_fixture(MAIN_TABLE as u8, 32_766, u32::MAX);
        push_rt_attribute(&mut duplicate, FRA_PRIORITY, &32_766_u32.to_ne_bytes());
        assert_eq!(
            parse_rule_payload(&duplicate).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn local_host_routes_are_blockers_but_broad_private_local_routes_fail_closed() {
        let mut local_host = raw_route("192.168.70.9/32", 2, 0);
        local_host.table = LOCAL_TABLE;
        local_host.kind = RouteKind::Other;
        let snapshot = normalize_snapshot(RawSnapshot {
            links: vec![raw_link(2, "eth0", None, Iff::UP.bits())],
            addresses: vec![],
            routes: vec![local_host, raw_route("192.168.70.0/24", 2, 10)],
            rules: canonical_rules(),
        })
        .unwrap();
        assert!(snapshot.effective_routes().iter().any(|route| {
            route.destination() == "192.168.70.9/32".parse().unwrap()
                && route.kind() == RouteKind::Other
                && route.interface().is_none()
        }));

        let mut broad_local = raw_route("192.168.70.0/24", 2, 0);
        broad_local.table = LOCAL_TABLE;
        assert_eq!(
            normalize_snapshot(RawSnapshot {
                links: vec![raw_link(2, "eth0", None, Iff::UP.bits())],
                addresses: vec![],
                routes: vec![broad_local],
                rules: canonical_rules(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn nonempty_default_table_and_conflicting_interface_records_fail_closed() {
        let mut default_route = raw_route("0.0.0.0/0", 2, 0);
        default_route.table = DEFAULT_TABLE;
        assert_eq!(
            normalize_snapshot(RawSnapshot {
                links: vec![raw_link(2, "eth0", None, Iff::UP.bits())],
                addresses: vec![],
                routes: vec![default_route],
                rules: canonical_rules(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Unsupported
        );

        assert_eq!(
            normalize_snapshot(RawSnapshot {
                links: vec![
                    raw_link(2, "eth0", None, Iff::UP.bits()),
                    raw_link(2, "wg0", Some("wireguard"), Iff::UP.bits()),
                ],
                addresses: vec![],
                routes: vec![],
                rules: canonical_rules(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );

        assert_eq!(
            normalize_snapshot(RawSnapshot {
                links: vec![raw_link(2, "eth0", None, Iff::UP.bits())],
                addresses: vec![],
                routes: vec![raw_route("192.168.80.0/24", 3, 0)],
                rules: canonical_rules(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn custom_or_selected_policy_rules_fail_closed() {
        let mut fixtures = Vec::new();
        let mut custom_table = canonical_rules();
        custom_table.push(RawRule {
            priority: 100,
            table: 1000,
            action: FR_ACT_TO_TBL,
            has_selectors: false,
            reserved_fields_are_zero: true,
            flags: 0,
        });
        fixtures.push(custom_table);

        let mut selected = canonical_rules();
        selected[1].has_selectors = true;
        fixtures.push(selected);

        for rules in fixtures {
            let error = normalize_snapshot(RawSnapshot {
                links: vec![],
                addresses: vec![],
                routes: vec![],
                rules,
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        }
    }

    #[test]
    fn byte_decoders_reject_truncated_fixture_fields() {
        assert_eq!(
            decode_u32(&[1, 2, 3]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            decode_ipv4_net(24, &[192, 168, 1]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            decode_network(RtAddrFamily::Inet6, 64, &[0; 15])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn bounded_dump_helper_accepts_the_limit_and_rejects_the_next_row() {
        let mut rows = Vec::new();
        push_bounded(&mut rows, 1, 2, "test rows").unwrap();
        push_bounded(&mut rows, 2, 2, "test rows").unwrap();

        let error = push_bounded(&mut rows, 3, 2, "test rows").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(rows, vec![1, 2]);

        let mut count = 0;
        count_bounded(&mut count, 1, "test rows").unwrap();
        assert_eq!(
            count_bounded(&mut count, 1, "test rows")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(count, 1);
    }

    #[test]
    fn extended_table_compatibility_is_strict_but_supports_large_table_encoding() {
        assert!(table_fields_are_compatible(MAIN_TABLE, MAIN_TABLE));
        assert!(table_fields_are_compatible(0, 1_000));
        assert!(table_fields_are_compatible(COMPAT_TABLE, 1_000));
        assert!(!table_fields_are_compatible(MAIN_TABLE, DEFAULT_TABLE));
        assert!(!table_fields_are_compatible(COMPAT_TABLE, MAIN_TABLE));
    }

    #[test]
    fn route_metadata_allowlist_blocks_unspecified_expiring_and_future_attributes() {
        assert!(harmless_ipv4_route_metadata(7));
        assert!(harmless_ipv4_route_metadata(8));
        assert!(harmless_ipv4_route_metadata(31));
        assert!(!harmless_ipv4_route_metadata(0));
        assert!(!harmless_ipv4_route_metadata(23));
        assert!(!harmless_ipv4_route_metadata(NLA_TYPE_MASK));
    }
}
