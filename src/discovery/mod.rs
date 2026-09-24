//! Bounded HDHomeRun discovery orchestration.

mod changes;
mod client;
mod hostname;
mod local;
mod manual;
mod registry;
mod routed;
mod routes;
mod typed_subnet;
mod types;

pub use changes::{
    Coalesced, InterfaceInventory, InterfaceLoss, NETWORK_CHANGE_MAX_DELAY,
    NETWORK_CHANGE_QUIET_PERIOD, NetworkChange, coalesce_burst,
};
#[cfg(target_os = "linux")]
pub use changes::{LinuxNetworkChangeWatcher, NetworkChangeWatchError};
#[cfg(all(test, feature = "desktop"))]
pub(crate) use client::DiscoveryPortOverride;
pub use client::{
    DiscoveryClient, DiscoveryError, DiscoveryObservation, DiscoveryReport, DiscoveryStats,
    InvalidProbeConfig, ProbeConfig, ProbeFailureClass, ProbeIssue,
};
pub(crate) use hostname::HostnameResolver;
pub use hostname::{
    DiscoveryEntry, HOSTNAME_RESOLUTION_TIMEOUT, HostnameResolutionError, HostnameTarget,
    InvalidDiscoveryEntry, InvalidHostnameTarget, MAX_CONCURRENT_HOSTNAME_LOOKUPS,
    MAX_HOSTNAME_BYTES, MAX_RESOLVED_ADDRESSES, resolve_hostname,
};
pub use local::local_probe_endpoints;
pub use manual::{
    ExactDiscoveryTarget, InvalidExactDiscoveryTarget, MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES,
};
pub use registry::{
    DeviceRegistry, ExpirationOutcome, LocatorClaim, LocatorOrigin, ObservationOutcome,
    RegisteredDevice, RegistryError, RegistryInstant,
};
pub use routed::{
    ApprovedIpv4Range, DEFAULT_ROUTED_SCAN_DEADLINE, InvalidRoutedScanConfig,
    MAX_ROUTED_CANDIDATES, MAX_ROUTED_CONCURRENCY, MAX_ROUTED_SCAN_DEADLINE,
    MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND, MIN_ROUTED_SCAN_DEADLINE, RoutedRangeError,
    RoutedScanConfig,
};
pub use typed_subnet::{InvalidTypedSubnetScope, TypedSubnetScope};
pub use types::{DiscoveryMethod, ProbeEndpoint};
