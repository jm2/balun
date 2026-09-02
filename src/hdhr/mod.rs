//! HDHomeRun protocol support.

#[cfg(all(test, feature = "desktop"))]
pub(crate) mod fake_device;
mod fallback;
mod http;
mod inspection;
mod lineup;
pub mod protocol;
mod resolver;
#[cfg(test)]
mod test_support;

pub use http::{
    DeviceEndpoint, DeviceHttpClient, DeviceHttpConfig, DeviceHttpError, DeviceInfo, EndpointError,
    InvalidHttpConfig, MAX_ADVERTISED_URL_BYTES, MAX_DEVICE_JSON_BYTES, MAX_LINEUP_CHANNELS,
    MAX_LINEUP_JSON_BYTES, UrlRole,
};
pub use inspection::{
    DEFAULT_INSPECTION_DEADLINE, DeviceInspection, DeviceInspectionError, DeviceInspectionIssue,
    DeviceInspectionIssueKind, DeviceInspectionReport, DeviceInspectionSummary, DeviceInspector,
    MAX_INSPECTION_OBSERVATIONS,
};
pub use lineup::{
    DeviceLineup, DeviceSnapshot, DeviceSnapshotError, LineupChannel, LineupError,
    LineupFetchError, MAX_GUIDE_NAME_BYTES, MAX_TAG_BYTES, MAX_TAG_COUNT, MAX_TAGS_BYTES,
};
pub use resolver::{
    DEFAULT_DEVICE_SNAPSHOT_DEADLINE, DeviceSnapshotIssue, DeviceSnapshotIssueKind,
    DeviceSnapshotResolutionError, DeviceSnapshotResolver, DeviceSnapshotTarget,
    DeviceSnapshotTargetError, DeviceSnapshotUnavailable, MAX_DEVICE_SNAPSHOT_ISSUES,
    MAX_DEVICE_SNAPSHOT_LOCATORS, ResolvedDeviceSnapshot,
};
