//! Bounded HDHomeRun discovery orchestration.

mod client;
mod local;
mod routed;
mod types;

pub use client::{
    DiscoveryClient, DiscoveryError, DiscoveryObservation, DiscoveryReport, DiscoveryStats,
    InvalidProbeConfig, ProbeConfig, ProbeIssue,
};
pub use local::local_probe_endpoints;
pub use routed::{
    ApprovedIpv4Range, InvalidRoutedScanConfig, MAX_ROUTED_CANDIDATES, MAX_ROUTED_CONCURRENCY,
    MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND, RoutedRangeError, RoutedScanConfig,
};
pub use types::{DiscoveryMethod, ProbeEndpoint};
