//! Authority, persistence, and fresh-route policy for routed discovery.
//!
//! The policy state machine remains deterministic and reads no clock or
//! network itself. Its private store owns key generation, strict durable state,
//! and globally serialized reservations; its packet-free gate consumes a
//! committed [`RoutedScanPermit`] only after rebuilding the complete proposal
//! from a caller-supplied fresh [`RouteSnapshot`]. Those policy and gate layers
//! open no discovery socket and send no network traffic; the Linux controller
//! consumes their authority through its interface-pinned runner.

#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "route-derived execution is Linux-only while policy types remain compiled for cross-platform parity"
    )
)]

// The production Linux runner consumes this policy through interface-pinned
// sockets and combined route/store cancellation authority. The earlier
// standalone admission model and inspection helpers remain compiled for
// cross-platform parity and regression coverage.
mod controller;
mod gate;
#[cfg_attr(
    all(target_os = "linux", not(test)),
    allow(
        dead_code,
        reason = "the earlier standalone admission boundary remains compiled for regression coverage"
    )
)]
mod run;
mod store;
mod watch;

#[cfg(target_os = "linux")]
pub(crate) use controller::RoutedObserverCoordinator;
#[cfg(target_os = "linux")]
pub(crate) use controller::runner::{
    CompletedRoutedRun, LinuxObserverPairFactory, MonitoredRoutedDiscovery, MonitoredRoutedError,
    MonitoredRoutedRun, PinnedSocketProber, SystemRoutedClock,
};
#[cfg(target_os = "linux")]
pub(crate) use store::{ApprovalStore, StoreError, StorePaths, StoredRoutedProposal};

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use ipnet::{IpNet, Ipv4Net};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use super::client::ProbeConfig;
use super::routed::{ApprovedIpv4Targets, MAX_ROUTED_CANDIDATES, RoutedScanConfig};
use super::routes::{
    InterfaceId, InterfaceKind, RouteCandidate, RouteCandidateOrigin, RouteKind, RouteScope,
    RouteSnapshot, select_route_candidates,
};

const ROUTED_FINGERPRINT_DOMAIN: &[u8] = b"io.github.jm2.Balun/route-derived-discovery-approval";
const ROUTED_FINGERPRINT_POLICY_REVISION: u32 = 1;
const ROUTE_PROVIDER_SEMANTICS: &[u8] = b"normalized-effective-route-snapshot-v1";
#[cfg(target_os = "linux")]
const ROUTE_PROVIDER_PLATFORM: &[u8] = b"linux";
#[cfg(target_os = "macos")]
const ROUTE_PROVIDER_PLATFORM: &[u8] = b"macos";
#[cfg(target_os = "windows")]
const ROUTE_PROVIDER_PLATFORM: &[u8] = b"windows";
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const ROUTE_PROVIDER_PLATFORM: &[u8] = b"unsupported-platform";
const MAX_ROUTE_SNAPSHOT_INTERFACES: usize = 8_192;
const MAX_ROUTE_SNAPSHOT_ROUTES: usize = 131_072;
const MAX_ROUTE_SNAPSHOT_ADDRESSES: usize = 32_768;
const MAX_ROUTED_PROPOSAL_ORIGINS: usize = 1_024;
const MAX_TUNNEL_ASSIGNED_PREFIXES: usize = 256;
const MAX_INTERFACE_NAME_BYTES: usize = 255;
const BASE_AUTOMATIC_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const MAX_AUTOMATIC_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const RESERVATION_LEASE: Duration = Duration::from_secs(60);
const MAX_EMPTY_RUN_STREAK: u8 = 2;

/// An installation-scoped key supplied by the private persistence boundary.
///
/// The key is intentionally neither generated nor persisted here. Its debug
/// representation is redacted and the raw bytes cannot be recovered through
/// this public API.
pub struct RouteFingerprintKey(Zeroizing<[u8; 32]>);

impl RouteFingerprintKey {
    /// Wrap key material supplied by a trusted platform/persistence boundary.
    #[must_use]
    pub(crate) fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let key = Self(Zeroizing::new(bytes));
        // Arrays are `Copy`; scrub the caller-to-owner transfer copy too.
        bytes.zeroize();
        key
    }
}

impl fmt::Debug for RouteFingerprintKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteFingerprintKey(<redacted>)")
    }
}

/// A non-reversible, installation-scoped identity for one exact proposal.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RouteFingerprint([u8; 32]);

impl fmt::Debug for RouteFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RouteFingerprint(<redacted>)")
    }
}

/// One route origin shown only while asking the user for approval.
///
/// This is intentionally not serializable. Callers may render the fields in a
/// consent UI, but default debug output does not expose them to logs.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoutedProposalOriginSummary {
    interface_name: String,
    network: Ipv4Net,
    scopes: Vec<RouteScope>,
}

impl RoutedProposalOriginSummary {
    #[must_use]
    pub fn interface_name(&self) -> &str {
        &self.interface_name
    }

    #[must_use]
    pub const fn network(&self) -> Ipv4Net {
        self.network
    }

    #[must_use]
    pub fn scopes(&self) -> &[RouteScope] {
        &self.scopes
    }
}

impl fmt::Debug for RoutedProposalOriginSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedProposalOriginSummary")
            .field("interface_name", &"<redacted>")
            .field("network", &"<redacted>")
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

/// Bounded, ephemeral information suitable for a user approval prompt.
///
/// The raw interface and prefix values remain available through explicit
/// getters for UI rendering. They are redacted from `Debug` and must not be
/// placed in persisted approval state.
#[derive(Clone, Eq, PartialEq)]
pub struct RoutedProposalSummary {
    candidate_count: usize,
    maximum_request_datagrams: usize,
    wire_datagrams_per_second: u16,
    max_in_flight: usize,
    overall_deadline: Duration,
    origins: Vec<RoutedProposalOriginSummary>,
}

impl RoutedProposalSummary {
    #[must_use]
    pub const fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    #[must_use]
    pub const fn maximum_request_datagrams(&self) -> usize {
        self.maximum_request_datagrams
    }

    #[must_use]
    pub const fn wire_datagrams_per_second(&self) -> u16 {
        self.wire_datagrams_per_second
    }

    #[must_use]
    pub const fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    #[must_use]
    pub const fn overall_deadline(&self) -> Duration {
        self.overall_deadline
    }

    #[must_use]
    pub fn origins(&self) -> &[RoutedProposalOriginSummary] {
        &self.origins
    }
}

impl fmt::Debug for RoutedProposalSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedProposalSummary")
            .field("candidate_count", &self.candidate_count)
            .field("maximum_request_datagrams", &self.maximum_request_datagrams)
            .field("wire_datagrams_per_second", &self.wire_datagrams_per_second)
            .field("max_in_flight", &self.max_in_flight)
            .field("overall_deadline", &self.overall_deadline)
            .field("origin_count", &self.origins.len())
            .finish()
    }
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct ResolvedTunnelOrigin {
    original: RouteCandidateOrigin,
    interface_name: String,
    assigned_prefixes: Vec<IpNet>,
    scopes: Vec<RouteScope>,
}

#[derive(Clone, Eq, PartialEq)]
struct ResolvedRouteCandidate {
    address: std::net::Ipv4Addr,
    origins: Vec<ResolvedTunnelOrigin>,
}

/// An exact, bounded route-derived target proposal.
///
/// It retains every candidate origin and its resolved active tunnel binding.
/// Default debug output exposes only bounded counts and a redacted digest.
#[derive(Clone)]
pub struct RoutedScanProposal {
    fingerprint: RouteFingerprint,
    targets: ApprovedIpv4Targets,
    resolved_candidates: Vec<ResolvedRouteCandidate>,
    probe_config: ProbeConfig,
    scan_config: RoutedScanConfig,
    summary: RoutedProposalSummary,
}

impl RoutedScanProposal {
    /// Validate and fingerprint candidates against the exact snapshot from
    /// which they were selected.
    ///
    /// Explicit-range origins are rejected: this authority is exclusively for
    /// automatically route-derived proposals. The supplied candidate order is
    /// irrelevant, but the complete addresses and origin sets must equal a
    /// fresh `select_route_candidates(snapshot, &[])` result.
    pub(crate) fn from_route_candidates(
        snapshot: &RouteSnapshot,
        candidates: &[RouteCandidate],
        key: &RouteFingerprintKey,
        probe_config: ProbeConfig,
        scan_config: RoutedScanConfig,
    ) -> Result<Self, RoutedProposalError> {
        validate_snapshot_bounds(snapshot)?;

        let mut supplied = candidates.to_vec();
        supplied.sort_by_key(RouteCandidate::address);
        if supplied
            .windows(2)
            .any(|pair| pair[0].address() == pair[1].address())
        {
            return Err(RoutedProposalError::CandidateSetMismatch);
        }

        let expected = select_route_candidates(snapshot, &[])
            .map_err(|_| RoutedProposalError::CandidateSelectionFailed)?;
        if supplied != expected {
            return Err(RoutedProposalError::CandidateSetMismatch);
        }
        if supplied.is_empty() {
            return Err(RoutedProposalError::EmptyProposal);
        }
        if supplied.len() > MAX_ROUTED_CANDIDATES {
            return Err(RoutedProposalError::TooManyCandidates {
                maximum: MAX_ROUTED_CANDIDATES,
            });
        }

        let targets = ApprovedIpv4Targets::new(supplied.iter().map(RouteCandidate::address))
            .map_err(|_| RoutedProposalError::InvalidTargetSet)?;
        if targets.is_empty() || targets.len() != supplied.len() {
            return Err(RoutedProposalError::InvalidTargetSet);
        }

        let mut resolved_candidates = Vec::with_capacity(supplied.len());
        let mut summary_origins = BTreeSet::new();
        let mut total_origins = 0_usize;

        for candidate in supplied {
            if candidate.origins().is_empty() {
                return Err(RoutedProposalError::MissingOrigin);
            }
            // v0.1 deliberately refuses ECMP/multi-origin authority. We still
            // inspect the complete origin set and fail closed instead of
            // silently choosing one route or fingerprinting a lossy subset.
            if candidate.origins().len() != 1 {
                return Err(RoutedProposalError::AmbiguousOriginSet);
            }
            let mut resolved_origins = Vec::with_capacity(candidate.origins().len());
            for origin in candidate.origins() {
                total_origins = total_origins.saturating_add(1);
                if total_origins > MAX_ROUTED_PROPOSAL_ORIGINS {
                    return Err(RoutedProposalError::TooManyOrigins {
                        maximum: MAX_ROUTED_PROPOSAL_ORIGINS,
                    });
                }

                let RouteCandidateOrigin::TunnelRoute { interface, network } = *origin else {
                    return Err(RoutedProposalError::ExplicitOrigin);
                };
                if network != network.trunc()
                    || network.prefix_len() < 24
                    || !network.network().is_private()
                    || !network.broadcast().is_private()
                    || !network.contains(&candidate.address())
                {
                    return Err(RoutedProposalError::InvalidOriginNetwork);
                }

                let binding = resolve_tunnel_binding(snapshot, interface)?;
                let scopes = resolve_route_scopes(snapshot, interface, network)?;
                summary_origins.insert(RoutedProposalOriginSummary {
                    interface_name: binding.name.clone(),
                    network,
                    scopes: scopes.clone(),
                });
                resolved_origins.push(ResolvedTunnelOrigin {
                    original: *origin,
                    interface_name: binding.name,
                    assigned_prefixes: binding.assigned_prefixes,
                    scopes,
                });
            }
            resolved_origins.sort();
            resolved_candidates.push(ResolvedRouteCandidate {
                address: candidate.address(),
                origins: resolved_origins,
            });
        }

        let fingerprint = fingerprint(key, &resolved_candidates, probe_config, scan_config);
        let summary = RoutedProposalSummary {
            candidate_count: targets.len(),
            maximum_request_datagrams: targets.len() * usize::from(probe_config.attempts()),
            wire_datagrams_per_second: scan_config.wire_datagrams_per_second(),
            max_in_flight: scan_config.max_in_flight(),
            overall_deadline: scan_config.overall_deadline(),
            origins: summary_origins.into_iter().collect(),
        };

        Ok(Self {
            fingerprint,
            targets,
            resolved_candidates,
            probe_config,
            scan_config,
            summary,
        })
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub const fn fingerprint(&self) -> RouteFingerprint {
        self.fingerprint
    }

    #[must_use]
    pub fn summary(&self) -> &RoutedProposalSummary {
        &self.summary
    }
}

impl fmt::Debug for RoutedScanProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let origin_count = self
            .resolved_candidates
            .iter()
            .map(|candidate| candidate.origins.len())
            .sum::<usize>();
        formatter
            .debug_struct("RoutedScanProposal")
            .field("fingerprint", &self.fingerprint)
            .field("candidate_count", &self.targets.len())
            .field("origin_count", &origin_count)
            .finish()
    }
}

struct ResolvedTunnelBinding {
    name: String,
    assigned_prefixes: Vec<IpNet>,
}

fn validate_snapshot_bounds(snapshot: &RouteSnapshot) -> Result<(), RoutedProposalError> {
    if snapshot.interfaces().len() > MAX_ROUTE_SNAPSHOT_INTERFACES {
        return Err(RoutedProposalError::TooManySnapshotInterfaces {
            maximum: MAX_ROUTE_SNAPSHOT_INTERFACES,
        });
    }
    if snapshot.effective_routes().len() > MAX_ROUTE_SNAPSHOT_ROUTES {
        return Err(RoutedProposalError::TooManySnapshotRoutes {
            maximum: MAX_ROUTE_SNAPSHOT_ROUTES,
        });
    }

    let mut ids = BTreeSet::new();
    let mut address_count = 0_usize;
    for interface in snapshot.interfaces() {
        if !ids.insert(interface.id()) {
            return Err(RoutedProposalError::DuplicateInterfaceId);
        }
        if !is_valid_interface_name(interface.name()) {
            return Err(RoutedProposalError::InvalidInterfaceName);
        }
        if interface.addresses().len() > MAX_TUNNEL_ASSIGNED_PREFIXES {
            return Err(RoutedProposalError::TooManyInterfaceAddresses {
                maximum: MAX_TUNNEL_ASSIGNED_PREFIXES,
            });
        }
        address_count = address_count.saturating_add(interface.addresses().len());
        if address_count > MAX_ROUTE_SNAPSHOT_ADDRESSES {
            return Err(RoutedProposalError::TooManySnapshotAddresses {
                maximum: MAX_ROUTE_SNAPSHOT_ADDRESSES,
            });
        }
        if interface
            .addresses()
            .iter()
            .any(|network| *network != network.trunc())
        {
            return Err(RoutedProposalError::InvalidAssignedPrefixes);
        }
    }

    Ok(())
}

fn resolve_tunnel_binding(
    snapshot: &RouteSnapshot,
    interface_id: InterfaceId,
) -> Result<ResolvedTunnelBinding, RoutedProposalError> {
    let mut matches = snapshot.interfaces().iter().filter(|interface| {
        interface.id() == interface_id
            && interface.is_up()
            && interface.kind() == InterfaceKind::Tunnel
    });
    let Some(interface) = matches.next() else {
        return Err(RoutedProposalError::MissingTunnelBinding);
    };
    if matches.next().is_some() {
        return Err(RoutedProposalError::AmbiguousTunnelBinding);
    }
    if !is_valid_interface_name(interface.name()) {
        return Err(RoutedProposalError::InvalidInterfaceName);
    }
    if interface.addresses().len() > MAX_TUNNEL_ASSIGNED_PREFIXES {
        return Err(RoutedProposalError::TooManyAssignedPrefixes {
            maximum: MAX_TUNNEL_ASSIGNED_PREFIXES,
        });
    }

    let assigned_prefixes = interface
        .addresses()
        .iter()
        .copied()
        .map(|network| network.trunc())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if assigned_prefixes.len() != interface.addresses().len() {
        return Err(RoutedProposalError::InvalidAssignedPrefixes);
    }

    Ok(ResolvedTunnelBinding {
        name: interface.name().to_owned(),
        assigned_prefixes,
    })
}

fn is_valid_interface_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_INTERFACE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

fn resolve_route_scopes(
    snapshot: &RouteSnapshot,
    interface: InterfaceId,
    network: Ipv4Net,
) -> Result<Vec<RouteScope>, RoutedProposalError> {
    let scopes = snapshot
        .effective_routes()
        .iter()
        .filter(|route| {
            route.destination() == IpNet::V4(network)
                && route.interface() == Some(interface)
                && route.kind() == RouteKind::Unicast
                && route.scope() != RouteScope::Other
        })
        .map(|route| route.scope())
        .collect::<BTreeSet<_>>();

    if scopes.is_empty() {
        return Err(RoutedProposalError::MissingRouteScope);
    }
    Ok(scopes.into_iter().collect())
}

fn fingerprint(
    key: &RouteFingerprintKey,
    candidates: &[ResolvedRouteCandidate],
    probe_config: ProbeConfig,
    scan_config: RoutedScanConfig,
) -> RouteFingerprint {
    // `blake3::Hasher` retains a copy of the keyed state. Keep that transient
    // copy in a zeroizing wrapper as well as zeroizing the persisted key.
    let mut hasher = Zeroizing::new(blake3::Hasher::new_keyed(&key.0));
    hash_bytes(&mut hasher, ROUTED_FINGERPRINT_DOMAIN);
    hash_u32(&mut hasher, ROUTED_FINGERPRINT_POLICY_REVISION);
    hash_bytes(&mut hasher, ROUTE_PROVIDER_PLATFORM);
    hash_bytes(&mut hasher, ROUTE_PROVIDER_SEMANTICS);
    hasher.update(&[probe_config.attempts()]);
    hash_duration(&mut hasher, probe_config.response_window());
    hash_u64(
        &mut hasher,
        u64::try_from(probe_config.max_received_datagrams())
            .expect("bounded receive limit fits in u64"),
    );
    hash_u64(
        &mut hasher,
        u64::try_from(probe_config.max_unique_devices()).expect("bounded device limit fits in u64"),
    );
    hasher.update(&scan_config.wire_datagrams_per_second().to_be_bytes());
    hash_u64(
        &mut hasher,
        u64::try_from(scan_config.max_in_flight()).expect("bounded concurrency fits in u64"),
    );
    hash_duration(&mut hasher, scan_config.overall_deadline());
    hash_u32(
        &mut hasher,
        u32::try_from(candidates.len()).expect("bounded candidate count fits in u32"),
    );

    for candidate in candidates {
        hasher.update(&candidate.address.octets());
        hash_u32(
            &mut hasher,
            u32::try_from(candidate.origins.len()).expect("bounded origin count fits in u32"),
        );
        for origin in &candidate.origins {
            let RouteCandidateOrigin::TunnelRoute { network, .. } = origin.original else {
                unreachable!("route-derived proposals reject explicit origins")
            };
            hasher.update(&[1]);
            // InterfaceId is retained above to resolve and revalidate the
            // exact snapshot binding, but is intentionally not fingerprinted:
            // Linux ifindexes and analogous platform handles are transient.
            hash_ipv4_network(&mut hasher, network);
            hash_bytes(&mut hasher, origin.interface_name.as_bytes());
            hash_u32(
                &mut hasher,
                u32::try_from(origin.assigned_prefixes.len())
                    .expect("bounded assigned-prefix count fits in u32"),
            );
            for prefix in &origin.assigned_prefixes {
                hash_ip_network(&mut hasher, *prefix);
            }
            hash_u32(
                &mut hasher,
                u32::try_from(origin.scopes.len()).expect("bounded scope count fits in u32"),
            );
            for scope in &origin.scopes {
                hasher.update(&[match scope {
                    RouteScope::OnLink => 1,
                    RouteScope::ViaGateway => 2,
                    RouteScope::Other => 3,
                }]);
            }
        }
    }

    RouteFingerprint(*hasher.finalize().as_bytes())
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_u32(
        hasher,
        u32::try_from(value.len()).expect("bounded fingerprint field length fits in u32"),
    );
    hasher.update(value);
}

fn hash_u32(hasher: &mut blake3::Hasher, value: u32) {
    hasher.update(&value.to_be_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

fn hash_duration(hasher: &mut blake3::Hasher, value: Duration) {
    hash_u64(hasher, value.as_secs());
    hash_u32(hasher, value.subsec_nanos());
}

fn hash_ipv4_network(hasher: &mut blake3::Hasher, network: Ipv4Net) {
    hasher.update(&network.network().octets());
    hasher.update(&[network.prefix_len()]);
}

fn hash_ip_network(hasher: &mut blake3::Hasher, network: IpNet) {
    match network {
        IpNet::V4(network) => {
            hasher.update(&[4]);
            hash_ipv4_network(hasher, network);
        }
        IpNet::V6(network) => {
            hasher.update(&[6]);
            hasher.update(&network.network().octets());
            hasher.update(&[network.prefix_len()]);
        }
    }
}

/// A fixed, topology-redacted failure to build a route-derived proposal.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RoutedProposalError {
    #[error("route snapshot exceeds the {maximum}-interface limit")]
    TooManySnapshotInterfaces { maximum: usize },

    #[error("route snapshot exceeds the {maximum}-route limit")]
    TooManySnapshotRoutes { maximum: usize },

    #[error("route snapshot exceeds the {maximum}-address limit")]
    TooManySnapshotAddresses { maximum: usize },

    #[error("route snapshot contains a duplicate interface identity")]
    DuplicateInterfaceId,

    #[error("route-derived candidate selection failed")]
    CandidateSelectionFailed,

    #[error("the supplied candidates do not exactly match the route snapshot")]
    CandidateSetMismatch,

    #[error("route-derived discovery has no eligible targets")]
    EmptyProposal,

    #[error("route-derived discovery exceeds the {maximum}-candidate limit")]
    TooManyCandidates { maximum: usize },

    #[error("route-derived discovery contains an invalid target set")]
    InvalidTargetSet,

    #[error("a route-derived target has no origin")]
    MissingOrigin,

    #[error("a route-derived target has more than one effective tunnel origin")]
    AmbiguousOriginSet,

    #[error("route-derived discovery exceeds the {maximum}-origin limit")]
    TooManyOrigins { maximum: usize },

    #[error("an explicit range cannot be remembered as a route-derived approval")]
    ExplicitOrigin,

    #[error("a route-derived origin has an invalid private network")]
    InvalidOriginNetwork,

    #[error("a route-derived origin has no unique active tunnel binding")]
    MissingTunnelBinding,

    #[error("a route-derived origin has an ambiguous active tunnel binding")]
    AmbiguousTunnelBinding,

    #[error("a route-derived tunnel has an invalid interface name")]
    InvalidInterfaceName,

    #[error("a route-derived tunnel exceeds the {maximum}-assigned-prefix limit")]
    TooManyAssignedPrefixes { maximum: usize },

    #[error("a route snapshot interface exceeds the {maximum}-address limit")]
    TooManyInterfaceAddresses { maximum: usize },

    #[error("a route-derived tunnel has invalid assigned prefixes")]
    InvalidAssignedPrefixes,

    #[error("a route-derived origin has no eligible effective route scope")]
    MissingRouteScope,
}

/// Injected policy time in whole seconds from a caller-selected epoch.
///
/// Persistence may later map this to validated Unix time while a running
/// controller also keeps a monotonic shadow. This pure layer never reads a
/// system clock.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoutedPolicyTime(u64);

impl RoutedPolicyTime {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained for deterministic policy construction")
    )]
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn from_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    #[must_use]
    pub const fn as_seconds(self) -> u64 {
        self.0
    }

    fn saturating_add(self, duration: Duration) -> Self {
        let rounded_seconds = duration
            .as_secs()
            .saturating_add(u64::from(duration.subsec_nanos() != 0));
        Self(self.0.saturating_add(rounded_seconds))
    }

    fn remaining_until(self, now: Self) -> Duration {
        Duration::from_secs(self.0.saturating_sub(now.0))
    }
}

/// An opaque run identity supplied by persistence.
///
/// Its counter must be globally monotonic across the complete approval store,
/// not merely within one fingerprint record. Topology reapproval must carry
/// the high-water forward so an old completion can never match a new run.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct RoutedRunId([u8; 16]);

impl RoutedRunId {
    #[must_use]
    pub(crate) const fn from_counter(counter: u128) -> Self {
        Self(counter.to_be_bytes())
    }
}

impl fmt::Debug for RoutedRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RoutedRunId(<redacted>)")
    }
}

/// Why a remembered approval is being considered for a new scan.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RoutedScanTrigger {
    /// A startup, timer, or debounced network-change request.
    Automatic,
    /// A direct user refresh gesture. This may bypass cooldown, not an active
    /// reservation or fingerprint mismatch.
    ExplicitRefresh,
}

/// The authoritative classification of a completed scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutedScanOutcome {
    /// At least one validated HDHomeRun discovery response was accepted.
    Found,
    /// The complete planned scan finished with no accepted device.
    CompleteEmpty,
    /// Cancellation, deadline, route change, partial work, or transport error.
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveReservation {
    run_id: RoutedRunId,
    trigger: RoutedScanTrigger,
    expires_at: RoutedPolicyTime,
    previous_automatic_not_before: RoutedPolicyTime,
}

/// Pure remembered-approval, cooldown, and active-reservation state.
#[derive(Clone, Eq, PartialEq)]
pub struct RoutedApprovalState {
    fingerprint: RouteFingerprint,
    empty_run_streak: u8,
    automatic_not_before: RoutedPolicyTime,
    last_observed_time: RoutedPolicyTime,
    last_issued_run_id: Option<RoutedRunId>,
    active: Option<ActiveReservation>,
}

impl RoutedApprovalState {
    /// Create authority only from a proposal the user has explicitly approved.
    #[must_use]
    pub(crate) fn from_user_approval(
        proposal: &RoutedScanProposal,
        now: RoutedPolicyTime,
        issuance_high_water: Option<RoutedRunId>,
    ) -> Self {
        Self {
            fingerprint: proposal.fingerprint,
            empty_run_streak: 0,
            automatic_not_before: now,
            last_observed_time: now,
            // The persistence owner must carry this store-global high-water
            // across topology reapproval. `None` is valid only for a brand-new
            // store which has never issued a run.
            last_issued_run_id: issuance_high_water,
            active: None,
        }
    }

    /// Plan a reservation for one exact proposal without publishing authority.
    ///
    /// A successful decision contains a [`RoutedPendingReservation`], not a
    /// scan permit. Only the private store implementation can extract its
    /// permit, after durably publishing the complete
    /// `state_after_reservation()`; weaker durability releases no authority.
    pub(crate) fn plan_begin(
        &self,
        proposal: RoutedScanProposal,
        trigger: RoutedScanTrigger,
        now: RoutedPolicyTime,
        run_id: RoutedRunId,
    ) -> RoutedBeginDecision {
        if proposal.fingerprint != self.fingerprint {
            return RoutedBeginDecision::NeedsApproval(proposal.summary);
        }

        let now = now.max(self.last_observed_time);
        if let Some(active) = self.active
            && now < active.expires_at
        {
            return RoutedBeginDecision::Busy;
        }

        if trigger == RoutedScanTrigger::Automatic && now < self.automatic_not_before {
            return RoutedBeginDecision::CoolingDown {
                remaining: self.automatic_not_before.remaining_until(now),
            };
        }
        // Strict monotonicity makes reuse fail closed, including after an
        // earlier reservation expires.
        if self
            .last_issued_run_id
            .is_some_and(|last_issued| run_id <= last_issued)
        {
            return RoutedBeginDecision::InvalidRunId;
        }

        let mut next_state = self.clone();
        next_state.active = None;
        next_state.last_observed_time = now;
        let expires_at = now.saturating_add(RESERVATION_LEASE);
        let conservative_not_before = now.saturating_add(MAX_AUTOMATIC_COOLDOWN);
        let previous_automatic_not_before = next_state.automatic_not_before;
        next_state.automatic_not_before =
            next_state.automatic_not_before.max(conservative_not_before);
        next_state.last_issued_run_id = Some(run_id);
        next_state.active = Some(ActiveReservation {
            run_id,
            trigger,
            expires_at,
            previous_automatic_not_before,
        });

        RoutedBeginDecision::Pending(Box::new(RoutedPendingReservation {
            state_after_reservation: next_state,
            sealed_permit: store::seal_pending_permit(RoutedScanPermit {
                fingerprint: proposal.fingerprint,
                run_id,
                expires_at,
                targets: proposal.targets,
                resolved_candidates: proposal.resolved_candidates,
                probe_config: proposal.probe_config,
                scan_config: proposal.scan_config,
            }),
        }))
    }

    /// Apply completion only to the exact active run.
    pub(crate) fn complete(
        &mut self,
        run_id: RoutedRunId,
        outcome: RoutedScanOutcome,
        now: RoutedPolicyTime,
    ) -> RoutedCompletionDecision {
        let Some(active) = self.active else {
            return RoutedCompletionDecision::Stale;
        };
        if active.run_id != run_id {
            return RoutedCompletionDecision::Stale;
        }
        let now = now.max(self.last_observed_time);
        self.last_observed_time = now;
        if now >= active.expires_at {
            self.active = None;
            return RoutedCompletionDecision::Expired;
        }

        let cooldown = match outcome {
            RoutedScanOutcome::Found => {
                self.empty_run_streak = 0;
                BASE_AUTOMATIC_COOLDOWN
            }
            RoutedScanOutcome::CompleteEmpty if active.trigger == RoutedScanTrigger::Automatic => {
                self.empty_run_streak = self
                    .empty_run_streak
                    .saturating_add(1)
                    .min(MAX_EMPTY_RUN_STREAK);
                if self.empty_run_streak >= MAX_EMPTY_RUN_STREAK {
                    MAX_AUTOMATIC_COOLDOWN
                } else {
                    BASE_AUTOMATIC_COOLDOWN
                }
            }
            RoutedScanOutcome::CompleteEmpty | RoutedScanOutcome::Indeterminate => {
                BASE_AUTOMATIC_COOLDOWN
            }
        };

        self.active = None;
        let outcome_not_before = now.saturating_add(cooldown);
        self.automatic_not_before = if outcome == RoutedScanOutcome::Found {
            // A validated device is positive evidence and deliberately resets
            // both the empty streak and any older exponential cooldown.
            outcome_not_before
        } else {
            outcome_not_before.max(active.previous_automatic_not_before)
        };
        RoutedCompletionDecision::Applied {
            automatic_not_before: self.automatic_not_before,
            empty_run_streak: self.empty_run_streak,
        }
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub const fn fingerprint(&self) -> RouteFingerprint {
        self.fingerprint
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub const fn empty_run_streak(&self) -> u8 {
        self.empty_run_streak
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub const fn automatic_not_before(&self) -> RoutedPolicyTime {
        self.automatic_not_before
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub const fn last_observed_time(&self) -> RoutedPolicyTime {
        self.last_observed_time
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as a policy inspection accessor")
    )]
    pub fn active_run_id(&self) -> Option<RoutedRunId> {
        self.active.map(|active| active.run_id)
    }
}

impl fmt::Debug for RoutedApprovalState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedApprovalState")
            .field("fingerprint", &self.fingerprint)
            .field("empty_run_streak", &self.empty_run_streak)
            .field("automatic_not_before", &self.automatic_not_before)
            .field("last_observed_time", &self.last_observed_time)
            .field("has_issued_run", &self.last_issued_run_id.is_some())
            .field("active", &self.active.is_some())
            .finish()
    }
}

/// Result of attempting to reserve a proposal.
pub enum RoutedBeginDecision {
    Pending(Box<RoutedPendingReservation>),
    NeedsApproval(RoutedProposalSummary),
    CoolingDown { remaining: Duration },
    Busy,
    InvalidRunId,
}

impl fmt::Debug for RoutedBeginDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending(pending) => formatter.debug_tuple("Pending").field(pending).finish(),
            Self::NeedsApproval(summary) => formatter
                .debug_tuple("NeedsApproval")
                .field(summary)
                .finish(),
            Self::CoolingDown { remaining } => formatter
                .debug_struct("CoolingDown")
                .field("remaining", remaining)
                .finish(),
            Self::Busy => formatter.write_str("Busy"),
            Self::InvalidRunId => formatter.write_str("InvalidRunId"),
        }
    }
}

/// A reservation transition which has not yet been durably committed.
///
/// This value is non-cloneable. It contains a permit, but exposes no production
/// method to obtain it; extraction is private to the approval-store module and
/// occurs only after the exact next state was durably published.
pub struct RoutedPendingReservation {
    state_after_reservation: RoutedApprovalState,
    sealed_permit: store::StoreSealedPermit,
}

impl RoutedPendingReservation {
    #[must_use]
    pub(crate) fn state_after_reservation(&self) -> &RoutedApprovalState {
        &self.state_after_reservation
    }

    /// Unit tests for the pure policy and fresh-route gate have no persistent
    /// store. Keep their synthetic extraction unavailable in production.
    #[cfg(test)]
    fn into_test_persisted_parts(self) -> (RoutedApprovalState, RoutedScanPermit) {
        (
            self.state_after_reservation,
            store::unseal_pending_permit_for_test(self.sealed_permit),
        )
    }
}

impl fmt::Debug for RoutedPendingReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedPendingReservation")
            .field("state_after_reservation", &self.state_after_reservation)
            .field("permit", &self.sealed_permit)
            .finish()
    }
}

/// Result of applying a routed scan completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutedCompletionDecision {
    Applied {
        automatic_not_before: RoutedPolicyTime,
        empty_run_streak: u8,
    },
    Stale,
    Expired,
}

/// Single-use authority for one exact route-derived target set.
///
/// This type deliberately does not implement `Clone`; the fresh-route gate
/// consumes it by value. Its fields are private, so callers cannot synthesize
/// or alter authority. The gate re-snapshots, reconstructs the complete
/// resolved proposal, compares it with this retained plan, uses the fresh
/// resolved `InterfaceId` values, and shortens the scan deadline to
/// `expires_at`. Starting near lease expiry can therefore never use the
/// original longer work budget.
///
/// A compile-time ambiguity assertion in this module's tests prevents a
/// future accidental `Clone` implementation.
pub struct RoutedScanPermit {
    fingerprint: RouteFingerprint,
    run_id: RoutedRunId,
    expires_at: RoutedPolicyTime,
    targets: ApprovedIpv4Targets,
    resolved_candidates: Vec<ResolvedRouteCandidate>,
    probe_config: ProbeConfig,
    scan_config: RoutedScanConfig,
}

impl RoutedScanPermit {
    #[must_use]
    pub const fn fingerprint(&self) -> RouteFingerprint {
        self.fingerprint
    }

    #[must_use]
    pub const fn run_id(&self) -> RoutedRunId {
        self.run_id
    }

    #[must_use]
    pub const fn expires_at(&self) -> RoutedPolicyTime {
        self.expires_at
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as an admitted-scan inspection accessor")
    )]
    pub fn candidate_count(&self) -> usize {
        self.targets.len()
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as an admitted-scan inspection accessor")
    )]
    pub const fn probe_config(&self) -> ProbeConfig {
        self.probe_config
    }

    #[must_use]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained as an admitted-scan inspection accessor")
    )]
    pub const fn scan_config(&self) -> RoutedScanConfig {
        self.scan_config
    }
}

impl fmt::Debug for RoutedScanPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RoutedScanPermit")
            .field("fingerprint", &self.fingerprint)
            .field("run_id", &self.run_id)
            .field("expires_at", &self.expires_at)
            .field("candidate_count", &self.targets.len())
            .field(
                "origin_count",
                &self
                    .resolved_candidates
                    .iter()
                    .map(|candidate| candidate.origins.len())
                    .sum::<usize>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::super::routes::{NetworkInterface, NetworkRoute};
    use super::*;

    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }

    impl<T: ?Sized> AmbiguousIfClone<()> for T {}

    struct ImplementsClone;

    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}

    #[test]
    fn routed_scan_permit_does_not_implement_clone() {
        // If `RoutedScanPermit` ever implements `Clone`, both marker impls
        // apply and this inferred marker becomes a compile-time ambiguity.
        let _ = <RoutedScanPermit as AmbiguousIfClone<_>>::marker;
    }

    fn ipnet(value: &str) -> IpNet {
        value.parse().expect("valid test network")
    }

    fn interface(
        id: u64,
        name: &str,
        addresses: impl IntoIterator<Item = IpNet>,
    ) -> NetworkInterface {
        NetworkInterface::new(
            InterfaceId::new(id),
            name,
            InterfaceKind::Tunnel,
            true,
            addresses,
        )
    }

    fn route(id: u64, network: &str, scope: RouteScope) -> NetworkRoute {
        NetworkRoute::effective(
            ipnet(network),
            Some(InterfaceId::new(id)),
            RouteKind::Unicast,
            scope,
        )
    }

    fn snapshot_with(
        id: u64,
        name: &str,
        assigned: &[&str],
        destination: &str,
        scope: RouteScope,
    ) -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![interface(id, name, assigned.iter().copied().map(ipnet))],
            vec![route(id, destination, scope)],
        )
    }

    fn test_snapshot() -> RouteSnapshot {
        snapshot_with(
            7,
            "private-test-tunnel",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        )
    }

    fn key(byte: u8) -> RouteFingerprintKey {
        RouteFingerprintKey::from_bytes([byte; 32])
    }

    fn proposal_for(snapshot: &RouteSnapshot) -> RoutedScanProposal {
        let candidates = select_route_candidates(snapshot, &[]).expect("route candidates");
        RoutedScanProposal::from_route_candidates(
            snapshot,
            &candidates,
            &key(0x5a),
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
        .expect("valid route-derived proposal")
    }

    fn run(byte: u8) -> RoutedRunId {
        RoutedRunId::from_counter(u128::from(byte))
    }

    fn begin_committed(
        approval: &mut RoutedApprovalState,
        proposal: RoutedScanProposal,
        trigger: RoutedScanTrigger,
        now: RoutedPolicyTime,
        run_id: RoutedRunId,
    ) -> RoutedScanPermit {
        match approval.plan_begin(proposal, trigger, now, run_id) {
            RoutedBeginDecision::Pending(pending) => {
                let persisted = pending.state_after_reservation().clone();
                let (next_state, permit) = pending.into_test_persisted_parts();
                assert!(persisted == next_state);
                *approval = next_state;
                permit
            }
            other => panic!("expected permit, got {other:?}"),
        }
    }

    #[test]
    fn proposal_retains_exact_origins_and_builds_a_bounded_summary() {
        let snapshot = test_snapshot();
        let proposal = proposal_for(&snapshot);

        assert_eq!(proposal.summary().candidate_count(), 2);
        assert_eq!(proposal.summary().maximum_request_datagrams(), 4);
        assert_eq!(proposal.summary().origins().len(), 1);
        assert_eq!(
            proposal.summary().origins()[0].interface_name(),
            "private-test-tunnel"
        );
        assert_eq!(
            proposal.summary().origins()[0].network(),
            "172.31.90.8/30".parse::<Ipv4Net>().unwrap()
        );
        assert_eq!(
            proposal.summary().origins()[0].scopes(),
            &[RouteScope::OnLink]
        );
        assert_eq!(proposal.resolved_candidates.len(), 2);
        assert!(
            proposal
                .resolved_candidates
                .iter()
                .all(|candidate| candidate.origins.len() == 1)
        );
    }

    #[test]
    fn packet_and_timing_policy_are_fingerprinted_and_carried_into_the_permit() {
        let snapshot = test_snapshot();
        let candidates = select_route_candidates(&snapshot, &[]).unwrap();
        let one_attempt = ProbeConfig::new(1, Duration::from_millis(200), 256, 64).unwrap();
        let two_attempts = ProbeConfig::default();
        let slower_scan = RoutedScanConfig::new(32, 8).unwrap();
        let proposal_with_probe_config = |probe_config| {
            RoutedScanProposal::from_route_candidates(
                &snapshot,
                &candidates,
                &key(0x5a),
                probe_config,
                slower_scan,
            )
            .unwrap()
        };

        let first = proposal_with_probe_config(one_attempt);
        let different_attempts = proposal_with_probe_config(two_attempts);
        let different_response_window = proposal_with_probe_config(
            ProbeConfig::new(1, Duration::from_millis(201), 256, 64).unwrap(),
        );
        let different_receive_limit = proposal_with_probe_config(
            ProbeConfig::new(1, Duration::from_millis(200), 255, 64).unwrap(),
        );
        let different_device_limit = proposal_with_probe_config(
            ProbeConfig::new(1, Duration::from_millis(200), 256, 63).unwrap(),
        );
        let changed_scan_configs = [
            RoutedScanConfig::new(31, 8).unwrap(),
            RoutedScanConfig::new(32, 7).unwrap(),
            RoutedScanConfig::new_with_overall_deadline(32, 8, Duration::from_secs(14)).unwrap(),
        ];
        assert_ne!(first.fingerprint(), different_attempts.fingerprint());
        assert_ne!(first.fingerprint(), different_response_window.fingerprint());
        assert_ne!(first.fingerprint(), different_receive_limit.fingerprint());
        assert_ne!(first.fingerprint(), different_device_limit.fingerprint());
        for changed_scan_config in changed_scan_configs {
            let changed_scan = RoutedScanProposal::from_route_candidates(
                &snapshot,
                &candidates,
                &key(0x5a),
                one_attempt,
                changed_scan_config,
            )
            .unwrap();
            assert_ne!(first.fingerprint(), changed_scan.fingerprint());
        }
        assert_eq!(first.summary().maximum_request_datagrams(), 2);
        assert_eq!(first.summary().wire_datagrams_per_second(), 32);
        assert_eq!(first.summary().max_in_flight(), 8);
        assert_eq!(
            first.summary().overall_deadline(),
            slower_scan.overall_deadline()
        );

        let expected_fingerprint = first.fingerprint();
        let expected_resolved = first.resolved_candidates.clone();
        let mut approval = RoutedApprovalState::from_user_approval(
            &first,
            RoutedPolicyTime::from_seconds(0),
            None,
        );
        let permit = begin_committed(
            &mut approval,
            first,
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(0),
            run(1),
        );
        assert_eq!(permit.fingerprint(), expected_fingerprint);
        assert_eq!(permit.probe_config(), one_attempt);
        assert_eq!(permit.scan_config(), slower_scan);
        assert!(permit.resolved_candidates == expected_resolved);
    }

    #[test]
    fn canonical_order_and_irrelevant_routes_do_not_change_the_fingerprint() {
        let base = test_snapshot();
        let base_fingerprint = proposal_for(&base).fingerprint();

        let mut candidates = select_route_candidates(&base, &[]).unwrap();
        candidates.reverse();
        let reversed = RoutedScanProposal::from_route_candidates(
            &base,
            &candidates,
            &key(0x5a),
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
        .unwrap();
        assert_eq!(reversed.fingerprint(), base_fingerprint);

        let reordered = RouteSnapshot::from_effective_routes(
            vec![interface(
                7,
                "private-test-tunnel",
                [ipnet("fd00:1234::2/128"), ipnet("10.250.0.2/32")],
            )],
            vec![
                NetworkRoute::effective(
                    ipnet("203.0.113.0/24"),
                    Some(InterfaceId::new(7)),
                    RouteKind::Unicast,
                    RouteScope::OnLink,
                ),
                route(7, "172.31.90.8/30", RouteScope::OnLink),
            ],
        );
        assert_eq!(proposal_for(&reordered).fingerprint(), base_fingerprint);
    }

    #[test]
    fn every_material_route_binding_change_changes_the_fingerprint() {
        let base = proposal_for(&test_snapshot()).fingerprint();
        let transient_id_change = snapshot_with(
            8,
            "private-test-tunnel",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        );
        assert_eq!(proposal_for(&transient_id_change).fingerprint(), base);

        let changed = [
            snapshot_with(
                7,
                "renamed-private-test-tunnel",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::OnLink,
            ),
            snapshot_with(
                7,
                "private-test-tunnel",
                &["10.250.0.3/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::OnLink,
            ),
            snapshot_with(
                7,
                "private-test-tunnel",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::ViaGateway,
            ),
            snapshot_with(
                7,
                "private-test-tunnel",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.91.8/30",
                RouteScope::OnLink,
            ),
        ];

        for snapshot in changed {
            assert_ne!(proposal_for(&snapshot).fingerprint(), base);
        }
        let snapshot = test_snapshot();
        let candidates = select_route_candidates(&snapshot, &[]).unwrap();
        let changed_key = RoutedScanProposal::from_route_candidates(
            &snapshot,
            &candidates,
            &key(0xa5),
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
        .unwrap();
        assert_ne!(changed_key.fingerprint(), base);
    }

    #[test]
    fn stale_or_explicit_candidates_cannot_be_promoted_to_route_authority() {
        let first = test_snapshot();
        let second = snapshot_with(
            7,
            "private-test-tunnel",
            &["10.250.0.2/32"],
            "172.31.91.8/30",
            RouteScope::OnLink,
        );
        let stale = select_route_candidates(&first, &[]).unwrap();
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &second,
                &stale,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::CandidateSetMismatch
        );

        let explicit = select_route_candidates(&first, &[ipnet("10.99.0.9/32")]).unwrap();
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &first,
                &explicit,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::CandidateSetMismatch
        );
    }

    #[test]
    fn proposal_rejects_duplicate_interface_ids_and_retains_the_winning_scope_set() {
        let duplicate_binding = RouteSnapshot::from_effective_routes(
            vec![
                interface(7, "first-private-test-tunnel", [ipnet("10.250.0.2/32")]),
                interface(7, "second-private-test-tunnel", [ipnet("10.250.0.2/32")]),
            ],
            vec![route(7, "172.31.90.8/30", RouteScope::OnLink)],
        );
        let candidates = select_route_candidates(&duplicate_binding, &[]).unwrap();
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &duplicate_binding,
                &candidates,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::DuplicateInterfaceId
        );

        let ambiguous_scope = RouteSnapshot::from_effective_routes(
            vec![interface(
                7,
                "private-test-tunnel",
                [ipnet("10.250.0.2/32")],
            )],
            vec![
                route(7, "172.31.90.8/30", RouteScope::OnLink),
                route(7, "172.31.90.8/30", RouteScope::ViaGateway),
            ],
        );
        let candidates = select_route_candidates(&ambiguous_scope, &[]).unwrap();
        let proposal = RoutedScanProposal::from_route_candidates(
            &ambiguous_scope,
            &candidates,
            &key(0x5a),
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
        .unwrap();
        assert_eq!(
            proposal.summary().origins()[0].scopes(),
            &[RouteScope::OnLink, RouteScope::ViaGateway]
        );
    }

    #[test]
    fn proposal_rejects_multi_origin_ecmp_instead_of_choosing_a_tunnel() {
        let snapshot = RouteSnapshot::from_effective_routes(
            vec![
                interface(7, "first-private-test-tunnel", [ipnet("10.250.0.2/32")]),
                interface(8, "second-private-test-tunnel", [ipnet("10.251.0.2/32")]),
            ],
            vec![
                route(7, "172.31.90.8/30", RouteScope::OnLink),
                route(8, "172.31.90.8/30", RouteScope::ViaGateway),
            ],
        );
        let candidates = select_route_candidates(&snapshot, &[]).unwrap();
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.origins().len() == 2)
        );

        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &snapshot,
                &candidates,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::AmbiguousOriginSet
        );
    }

    #[test]
    fn generic_snapshot_identity_and_size_bounds_fail_before_authority() {
        let invalid_name = snapshot_with(
            7,
            "",
            &["10.250.0.2/32"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        );
        let candidates = select_route_candidates(&invalid_name, &[]).unwrap();
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &invalid_name,
                &candidates,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::InvalidInterfaceName
        );

        let too_many_addresses = (0_u16..=256)
            .map(|offset| {
                let third = (offset >> 8) as u8;
                let fourth = offset as u8;
                ipnet(&format!("10.200.{third}.{fourth}/32"))
            })
            .collect::<Vec<_>>();
        let oversized_binding = RouteSnapshot::from_effective_routes(
            vec![interface(7, "private-test-tunnel", too_many_addresses)],
            vec![route(7, "172.31.90.8/30", RouteScope::OnLink)],
        );
        let candidates = select_route_candidates(&oversized_binding, &[]).unwrap();
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &oversized_binding,
                &candidates,
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::TooManyInterfaceAddresses {
                maximum: MAX_TUNNEL_ASSIGNED_PREFIXES,
            }
        );

        let interfaces = (0..=MAX_ROUTE_SNAPSHOT_INTERFACES)
            .map(|index| {
                interface(
                    u64::try_from(index).unwrap().saturating_add(1),
                    "bounded-test-tunnel",
                    [],
                )
            })
            .collect();
        let oversized_snapshot = RouteSnapshot::from_effective_routes(interfaces, vec![]);
        assert_eq!(
            RoutedScanProposal::from_route_candidates(
                &oversized_snapshot,
                &[],
                &key(0x5a),
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .unwrap_err(),
            RoutedProposalError::TooManySnapshotInterfaces {
                maximum: MAX_ROUTE_SNAPSHOT_INTERFACES,
            }
        );
    }

    #[test]
    fn control_characters_in_interface_names_are_rejected_before_authority() {
        for invalid_name in ["private\0test-tunnel", "private\ntest-tunnel"] {
            let snapshot = snapshot_with(
                7,
                invalid_name,
                &["10.250.0.2/32"],
                "172.31.90.8/30",
                RouteScope::OnLink,
            );
            let candidates = select_route_candidates(&snapshot, &[]).unwrap();
            assert_eq!(
                RoutedScanProposal::from_route_candidates(
                    &snapshot,
                    &candidates,
                    &key(0x5a),
                    ProbeConfig::default(),
                    RoutedScanConfig::default(),
                )
                .unwrap_err(),
                RoutedProposalError::InvalidInterfaceName
            );
        }
    }

    #[test]
    fn default_debug_output_never_exposes_raw_topology_or_key_material() {
        let snapshot = test_snapshot();
        let proposal = proposal_for(&snapshot);
        let summary_debug = format!("{:?}", proposal.summary());
        let proposal_debug = format!("{proposal:?}");
        let fingerprint_debug = format!("{:?}", proposal.fingerprint());
        let key_debug = format!("{:?}", key(0x5a));
        let mut approval = RoutedApprovalState::from_user_approval(
            &proposal,
            RoutedPolicyTime::from_seconds(10),
            None,
        );
        let permit = begin_committed(
            &mut approval,
            proposal,
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(10),
            run(3),
        );
        let permit_debug = format!("{permit:?}");
        let approval_debug = format!("{approval:?}");

        for output in [
            summary_debug,
            proposal_debug,
            fingerprint_debug,
            key_debug,
            permit_debug,
            approval_debug,
        ] {
            assert!(!output.contains("private-test-tunnel"));
            assert!(!output.contains("172.31.90"));
            assert!(!output.contains("10.250"));
            assert!(!output.contains("fd00:1234"));
            assert!(!output.contains("5a5a5a"));
        }
    }

    #[test]
    fn automatic_runs_respect_cooldown_while_explicit_refresh_bypasses_it() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(100),
            None,
        );

        let first = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(100),
            run(1),
        );
        assert_eq!(first.candidate_count(), 2);
        assert_eq!(first.expires_at(), RoutedPolicyTime::from_seconds(160));
        assert_eq!(approval.active_run_id(), Some(run(1)));
        assert!(matches!(
            approval.plan_begin(
                proposal_for(&snapshot),
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(101),
                run(2),
            ),
            RoutedBeginDecision::Busy
        ));

        assert_eq!(
            approval.complete(
                first.run_id(),
                RoutedScanOutcome::Found,
                RoutedPolicyTime::from_seconds(110),
            ),
            RoutedCompletionDecision::Applied {
                automatic_not_before: RoutedPolicyTime::from_seconds(1_010),
                empty_run_streak: 0,
            }
        );
        match approval.plan_begin(
            proposal_for(&snapshot),
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(1_009),
            run(2),
        ) {
            RoutedBeginDecision::CoolingDown { remaining } => {
                assert_eq!(remaining, Duration::from_secs(1));
            }
            other => panic!("expected cooldown, got {other:?}"),
        }

        let explicit = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(200),
            run(3),
        );
        assert_eq!(explicit.run_id(), run(3));
    }

    #[test]
    fn complete_empty_automatic_runs_back_off_and_success_resets_the_streak() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let zero = RoutedPolicyTime::ZERO;
        assert_eq!(zero.as_seconds(), 0);
        let mut approval = RoutedApprovalState::from_user_approval(&initial, zero, None);

        let first = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(0),
            run(1),
        );
        assert_eq!(
            approval.complete(
                first.run_id(),
                RoutedScanOutcome::CompleteEmpty,
                RoutedPolicyTime::from_seconds(10),
            ),
            RoutedCompletionDecision::Applied {
                automatic_not_before: RoutedPolicyTime::from_seconds(910),
                empty_run_streak: 1,
            }
        );

        let second = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(910),
            run(2),
        );
        assert_eq!(
            approval.complete(
                second.run_id(),
                RoutedScanOutcome::CompleteEmpty,
                RoutedPolicyTime::from_seconds(920),
            ),
            RoutedCompletionDecision::Applied {
                automatic_not_before: RoutedPolicyTime::from_seconds(2_720),
                empty_run_streak: 2,
            }
        );

        let success = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(1_000),
            run(3),
        );
        approval.complete(
            success.run_id(),
            RoutedScanOutcome::Found,
            RoutedPolicyTime::from_seconds(1_001),
        );
        assert_eq!(approval.empty_run_streak(), 0);
        assert_eq!(
            approval.automatic_not_before(),
            RoutedPolicyTime::from_seconds(1_901)
        );
    }

    #[test]
    fn explicit_empty_and_indeterminate_runs_do_not_increase_empty_streak() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(0),
            None,
        );
        let automatic = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(0),
            run(1),
        );
        approval.complete(
            automatic.run_id(),
            RoutedScanOutcome::CompleteEmpty,
            RoutedPolicyTime::from_seconds(1),
        );
        assert_eq!(approval.empty_run_streak(), 1);

        let explicit = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(2),
            run(2),
        );
        approval.complete(
            explicit.run_id(),
            RoutedScanOutcome::CompleteEmpty,
            RoutedPolicyTime::from_seconds(3),
        );
        assert_eq!(approval.empty_run_streak(), 1);

        let indeterminate = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(4),
            run(3),
        );
        approval.complete(
            indeterminate.run_id(),
            RoutedScanOutcome::Indeterminate,
            RoutedPolicyTime::from_seconds(5),
        );
        assert_eq!(approval.empty_run_streak(), 1);
    }

    #[test]
    fn explicit_non_success_does_not_shorten_a_longer_existing_cooldown() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(0),
            None,
        );

        for (start, finish, run_id) in [(0, 1, run(1)), (901, 902, run(2))] {
            let permit = begin_committed(
                &mut approval,
                proposal_for(&snapshot),
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(start),
                run_id,
            );
            approval.complete(
                permit.run_id(),
                RoutedScanOutcome::CompleteEmpty,
                RoutedPolicyTime::from_seconds(finish),
            );
        }
        assert_eq!(
            approval.automatic_not_before(),
            RoutedPolicyTime::from_seconds(2_702)
        );

        let explicit = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(1_000),
            run(3),
        );
        approval.complete(
            explicit.run_id(),
            RoutedScanOutcome::Indeterminate,
            RoutedPolicyTime::from_seconds(1_001),
        );
        assert_eq!(
            approval.automatic_not_before(),
            RoutedPolicyTime::from_seconds(2_702)
        );
        assert_eq!(approval.empty_run_streak(), 2);
    }

    #[test]
    fn observed_time_high_water_makes_clock_rollback_fail_closed() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(1_000),
            None,
        );
        let active = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(1_000),
            run(1),
        );
        approval.complete(
            active.run_id(),
            RoutedScanOutcome::Found,
            RoutedPolicyTime::from_seconds(0),
        );

        assert_eq!(
            approval.last_observed_time(),
            RoutedPolicyTime::from_seconds(1_000)
        );
        assert_eq!(
            approval.automatic_not_before(),
            RoutedPolicyTime::from_seconds(1_900)
        );
        match approval.plan_begin(
            proposal_for(&snapshot),
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(1_001),
            run(2),
        ) {
            RoutedBeginDecision::CoolingDown { remaining } => {
                assert_eq!(remaining, Duration::from_secs(899));
            }
            other => panic!("clock rollback must remain cooling, got {other:?}"),
        }
    }

    #[test]
    fn exact_run_id_prevents_stale_completion_from_mutating_a_successor() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(0),
            None,
        );
        let first = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(0),
            run(1),
        );
        assert_eq!(
            approval.complete(
                run(99),
                RoutedScanOutcome::CompleteEmpty,
                RoutedPolicyTime::from_seconds(1),
            ),
            RoutedCompletionDecision::Stale
        );
        assert_eq!(approval.active_run_id(), Some(first.run_id()));

        assert!(matches!(
            approval.plan_begin(
                proposal_for(&snapshot),
                RoutedScanTrigger::ExplicitRefresh,
                first.expires_at(),
                first.run_id(),
            ),
            RoutedBeginDecision::InvalidRunId
        ));
        let second = begin_committed(
            &mut approval,
            proposal_for(&snapshot),
            RoutedScanTrigger::ExplicitRefresh,
            first.expires_at(),
            run(2),
        );
        let before = approval.clone();
        assert_eq!(
            approval.complete(
                first.run_id(),
                RoutedScanOutcome::Found,
                RoutedPolicyTime::from_seconds(61),
            ),
            RoutedCompletionDecision::Stale
        );
        assert!(approval == before);
        assert_eq!(approval.active_run_id(), Some(second.run_id()));
    }

    #[test]
    fn expired_completion_cannot_finalize_or_reuse_a_reservation() {
        let snapshot = test_snapshot();
        let initial = proposal_for(&snapshot);
        let mut approval = RoutedApprovalState::from_user_approval(
            &initial,
            RoutedPolicyTime::from_seconds(100),
            None,
        );
        let first = begin_committed(
            &mut approval,
            initial,
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(100),
            run(1),
        );
        let provisional_not_before = approval.automatic_not_before();

        assert_eq!(
            approval.complete(first.run_id(), RoutedScanOutcome::Found, first.expires_at(),),
            RoutedCompletionDecision::Expired
        );
        assert_eq!(approval.active_run_id(), None);
        assert_eq!(approval.automatic_not_before(), provisional_not_before);
        assert!(matches!(
            approval.plan_begin(
                proposal_for(&snapshot),
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(161),
                first.run_id(),
            ),
            RoutedBeginDecision::InvalidRunId
        ));
    }

    #[test]
    fn topology_reapproval_must_carry_the_store_global_run_high_water() {
        let first_snapshot = test_snapshot();
        let first_proposal = proposal_for(&first_snapshot);
        let mut first_approval = RoutedApprovalState::from_user_approval(
            &first_proposal,
            RoutedPolicyTime::from_seconds(0),
            None,
        );
        let issued = begin_committed(
            &mut first_approval,
            first_proposal,
            RoutedScanTrigger::ExplicitRefresh,
            RoutedPolicyTime::from_seconds(0),
            run(1),
        );

        let changed_snapshot = snapshot_with(
            7,
            "renamed-private-test-tunnel",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        );
        let replacement = proposal_for(&changed_snapshot);
        let replacement_fingerprint = replacement.fingerprint();
        let replacement_approval = RoutedApprovalState::from_user_approval(
            &replacement,
            RoutedPolicyTime::from_seconds(1),
            Some(issued.run_id()),
        );
        assert_eq!(replacement_approval.fingerprint(), replacement_fingerprint);
        assert!(matches!(
            replacement_approval.plan_begin(
                replacement,
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(1),
                issued.run_id(),
            ),
            RoutedBeginDecision::InvalidRunId
        ));
    }

    #[test]
    fn a_materially_changed_fingerprint_requires_new_approval_without_state_change() {
        let first_snapshot = test_snapshot();
        let first = proposal_for(&first_snapshot);
        let approval = RoutedApprovalState::from_user_approval(
            &first,
            RoutedPolicyTime::from_seconds(20),
            None,
        );
        let before = approval.clone();
        let changed = proposal_for(&snapshot_with(
            7,
            "renamed-private-test-tunnel",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        ));

        match approval.plan_begin(
            changed,
            RoutedScanTrigger::Automatic,
            RoutedPolicyTime::from_seconds(20),
            run(1),
        ) {
            RoutedBeginDecision::NeedsApproval(summary) => {
                assert_eq!(summary.candidate_count(), 2);
            }
            other => panic!("expected fresh approval, got {other:?}"),
        }
        assert!(approval == before);
    }
}
