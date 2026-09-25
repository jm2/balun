//! Bounded HDHomeRun discovery orchestration.

mod changes;
mod client;
mod hostname;
mod local;
mod manual;
mod registry;
mod subnet;
mod typed_subnet;
mod types;

#[cfg(target_os = "linux")]
pub use changes::LinuxNetworkChangeWatcher;
#[cfg(target_os = "macos")]
pub use changes::MacosNetworkChangeWatcher;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub use changes::NetworkChangeWatchError;
#[cfg(windows)]
pub use changes::WindowsNetworkChangeWatcher;
pub use changes::{
    Coalesced, InterfaceInventory, InterfaceLoss, NETWORK_CHANGE_MAX_DELAY,
    NETWORK_CHANGE_QUIET_PERIOD, NetworkChange, ObservationGate, ObservationGeneration,
    ObservationState, ObservationWatch, coalesce_burst,
};
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
#[cfg(test)]
pub(crate) use subnet::tests::{ScriptedFault, scripted_search};
#[cfg(test)]
pub(crate) use subnet::{PROCESS_LANE_TESTS, claim_process_lane};
pub use subnet::{
    SubnetAdmissionError, SubnetScanError, SubnetScanIncomplete, SubnetScanOutcome,
    SubnetScanPermit, SubnetScanReport, SubnetSearchConsent, discover_typed_subnet,
};
pub use typed_subnet::{InvalidTypedSubnetScope, TypedSubnetScope};
pub use types::{DiscoveryMethod, ProbeEndpoint};
