//! Bounded HDHomeRun discovery orchestration.

mod client;
mod local;
mod registry;
mod routed;
mod routes;
mod types;

pub use client::{
    DiscoveryClient, DiscoveryError, DiscoveryObservation, DiscoveryReport, DiscoveryStats,
    InvalidProbeConfig, ProbeConfig, ProbeIssue,
};
pub use local::local_probe_endpoints;
pub use registry::{
    DeviceRegistry, ExpirationOutcome, LocatorClaim, LocatorOrigin, ObservationOutcome,
    RegisteredDevice, RegistryError, RegistryInstant,
};
pub use routed::{
    ApprovedIpv4Range, InvalidRoutedScanConfig, MAX_ROUTED_CANDIDATES, MAX_ROUTED_CONCURRENCY,
    MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND, RoutedRangeError, RoutedScanConfig,
};
pub use routes::{
    InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, RouteCandidate,
    RouteCandidateError, RouteCandidateOrigin, RouteKind, RouteProvider, RouteScope, RouteSnapshot,
    route_candidates, select_route_candidates,
};
pub use types::{DiscoveryMethod, ProbeEndpoint};
