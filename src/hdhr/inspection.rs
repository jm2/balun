//! Bounded, identity-safe inspection of discovered HDHomeRun devices.
//!
//! Inspection reads only responder-pinned `discover.json` and `lineup.json`
//! resources. It never opens a channel stream or allocates a tuner. Complete
//! lineup rows (and therefore their stream URLs) remain inside the HDHomeRun
//! protocol boundary; callers receive only validated metadata and counts.

use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::fallback::preferred_first_locators;
use super::{
    DeviceEndpoint, DeviceHttpClient, DeviceHttpError, DeviceSnapshotError, LineupFetchError,
    MAX_ADVERTISED_URL_BYTES,
};
use crate::discovery::{DeviceRegistry, DiscoveryReport, RegistryError, RegistryInstant};
use crate::domain::DeviceId;

/// Default wall-clock budget for inspecting every device in one discovery
/// report, including locator fallback.
pub const DEFAULT_INSPECTION_DEADLINE: Duration = Duration::from_secs(60);
/// Maximum observations accepted from one discovery report before inspection.
pub const MAX_INSPECTION_OBSERVATIONS: usize = 4_096;

const MAX_INSPECTION_INTERFACE_BYTES: usize = 256;
const MAX_INSPECTION_DEVICE_TYPES: usize = 64;
const INSPECTION_PREPROCESS_YIELD_INTERVAL: usize = 64;

/// Fetches bounded, responder-pinned device metadata and lineup counts.
#[derive(Clone, Debug)]
pub struct DeviceInspector {
    http: DeviceHttpClient,
}

impl DeviceInspector {
    #[must_use]
    pub const fn new(http: DeviceHttpClient) -> Self {
        Self { http }
    }

    /// Inspect each stable DeviceID in a discovery report within one deadline.
    ///
    /// The preferred locator is attempted first. A failure then falls back to
    /// each remaining supported locator in deterministic address order. Every
    /// attempt validates `/discover.json` against the registry DeviceID before
    /// accepting lineup-derived counts. Cancellation or deadline expiry is
    /// atomic: no partial inspection report is returned.
    pub async fn inspect_discovery_report(
        &self,
        report: &DiscoveryReport,
        cancellation: &CancellationToken,
    ) -> Result<DeviceInspectionReport, DeviceInspectionError> {
        inspect_discovery_report_with_deadline(
            &self.http,
            report,
            cancellation,
            DEFAULT_INSPECTION_DEADLINE,
        )
        .await
    }
}

impl Default for DeviceInspector {
    fn default() -> Self {
        Self::new(DeviceHttpClient::default())
    }
}

/// One safe display summary from an identity-checked endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInspectionSummary {
    device_id: DeviceId,
    source: SocketAddr,
    friendly_name: Option<String>,
    model_number: Option<String>,
    firmware_version: Option<String>,
    tuner_count: Option<u8>,
    channel_count: usize,
    favorite_count: usize,
    drm_count: usize,
}

impl DeviceInspectionSummary {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub const fn source(&self) -> SocketAddr {
        self.source
    }

    #[must_use]
    pub fn friendly_name(&self) -> Option<&str> {
        self.friendly_name.as_deref()
    }

    #[must_use]
    pub fn model_number(&self) -> Option<&str> {
        self.model_number.as_deref()
    }

    #[must_use]
    pub fn firmware_version(&self) -> Option<&str> {
        self.firmware_version.as_deref()
    }

    #[must_use]
    pub const fn tuner_count(&self) -> Option<u8> {
        self.tuner_count
    }

    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.channel_count
    }

    #[must_use]
    pub const fn favorite_count(&self) -> usize {
        self.favorite_count
    }

    #[must_use]
    pub const fn drm_count(&self) -> usize {
        self.drm_count
    }
}

/// Fixed category for one locator that could not supply a summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceInspectionIssueKind {
    UnsupportedEndpoint,
    SnapshotFailed,
}

/// One failed locator attempt. Messages originate only from endpoint and HTTP
/// errors that reject credentials and cross-responder authorities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInspectionIssue {
    source: SocketAddr,
    kind: DeviceInspectionIssueKind,
    message: String,
}

impl DeviceInspectionIssue {
    #[must_use]
    pub const fn source(&self) -> SocketAddr {
        self.source
    }

    #[must_use]
    pub const fn kind(&self) -> DeviceInspectionIssueKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Inspection result for exactly one stable DeviceID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInspection {
    device_id: DeviceId,
    supported_locator_count: usize,
    issues: Vec<DeviceInspectionIssue>,
    summary: Option<DeviceInspectionSummary>,
}

impl DeviceInspection {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub const fn supported_locator_count(&self) -> usize {
        self.supported_locator_count
    }

    pub fn issues(&self) -> &[DeviceInspectionIssue] {
        &self.issues
    }

    #[must_use]
    pub const fn summary(&self) -> Option<&DeviceInspectionSummary> {
        self.summary.as_ref()
    }

    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.summary.is_some()
    }
}

/// Ordered results for the stable devices in one discovery report.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceInspectionReport {
    devices: Vec<DeviceInspection>,
}

impl DeviceInspectionReport {
    pub fn devices(&self) -> &[DeviceInspection] {
        &self.devices
    }

    #[must_use]
    pub fn attempted_devices(&self) -> usize {
        self.devices.len()
    }

    #[must_use]
    pub fn failed_devices(&self) -> usize {
        self.devices
            .iter()
            .filter(|inspection| !inspection.succeeded())
            .count()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DeviceInspectionError {
    #[error("could not build the device inspection registry: {0}")]
    Registry(#[from] RegistryError),

    #[error("device inspection was cancelled")]
    Cancelled,

    #[error("device inspection exceeded its {deadline:?} report deadline")]
    Deadline { deadline: Duration },

    #[error("device inspection report has {actual} {field}; maximum is {maximum}")]
    ReportLimit {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
}

#[derive(Debug)]
enum InspectionAttemptError {
    Cancelled,
    Failed(String),
}

enum InspectionCandidate {
    Supported(Box<DeviceEndpoint>),
    Unsupported { source: SocketAddr, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InspectionDetails {
    device_id: DeviceId,
    friendly_name: Option<String>,
    model_number: Option<String>,
    firmware_version: Option<String>,
    tuner_count: Option<u8>,
    channel_count: usize,
    favorite_count: usize,
    drm_count: usize,
}

trait SnapshotInspector {
    async fn fetch_snapshot_summary(
        &self,
        endpoint: &DeviceEndpoint,
        expected_device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<InspectionDetails, InspectionAttemptError>;
}

impl SnapshotInspector for DeviceHttpClient {
    async fn fetch_snapshot_summary(
        &self,
        endpoint: &DeviceEndpoint,
        expected_device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<InspectionDetails, InspectionAttemptError> {
        let snapshot = match self
            .fetch_device_snapshot(endpoint, expected_device_id, cancellation)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(DeviceSnapshotError::Metadata(DeviceHttpError::Cancelled))
            | Err(DeviceSnapshotError::Lineup(LineupFetchError::Http(
                DeviceHttpError::Cancelled,
            ))) => return Err(InspectionAttemptError::Cancelled),
            Err(error) => return Err(InspectionAttemptError::Failed(error.to_string())),
        };
        let info = snapshot.info();
        let lineup = snapshot.lineup();

        debug_assert_eq!(info.device_id(), lineup.device_id());
        Ok(InspectionDetails {
            device_id: info.device_id(),
            friendly_name: info.friendly_name().map(str::to_owned),
            model_number: info.model_number().map(str::to_owned),
            firmware_version: info.firmware_version().map(str::to_owned),
            tuner_count: info.tuner_count(),
            channel_count: lineup.channels().len(),
            favorite_count: lineup
                .channels()
                .iter()
                .filter(|channel| channel.is_favorite())
                .count(),
            drm_count: lineup
                .channels()
                .iter()
                .filter(|channel| channel.is_drm())
                .count(),
        })
    }
}

async fn inspect_discovery_report<I: SnapshotInspector>(
    inspector: &I,
    report: &DiscoveryReport,
    cancellation: &CancellationToken,
) -> Result<DeviceInspectionReport, DeviceInspectionError> {
    if cancellation.is_cancelled() {
        return Err(DeviceInspectionError::Cancelled);
    }
    require_report_limit(
        "observations",
        report.observations.len(),
        MAX_INSPECTION_OBSERVATIONS,
    )?;

    let mut registry = DeviceRegistry::default();
    for (index, observation) in report.observations.iter().enumerate() {
        if index > 0 && index % INSPECTION_PREPROCESS_YIELD_INTERVAL == 0 {
            tokio::task::yield_now().await;
        }
        if cancellation.is_cancelled() {
            return Err(DeviceInspectionError::Cancelled);
        }
        validate_observation_bounds(observation)?;
        registry.observe(observation.clone(), RegistryInstant::default())?;
    }

    let mut devices = Vec::with_capacity(registry.len());
    for (device_index, device) in registry.devices().enumerate() {
        if device_index > 0 {
            tokio::task::yield_now().await;
        }
        if cancellation.is_cancelled() {
            return Err(DeviceInspectionError::Cancelled);
        }

        let candidates = preferred_first_locators(device)
            .into_iter()
            .map(|locator| match DeviceEndpoint::from_locator(locator) {
                Ok(endpoint) => InspectionCandidate::Supported(Box::new(endpoint)),
                Err(error) => InspectionCandidate::Unsupported {
                    source: locator.source(),
                    message: error.to_string(),
                },
            })
            .collect::<Vec<_>>();
        let supported_locator_count = candidates
            .iter()
            .filter(|candidate| matches!(candidate, InspectionCandidate::Supported(_)))
            .count();
        let mut issues = Vec::new();
        let mut summary = None;
        for candidate in candidates {
            if cancellation.is_cancelled() {
                return Err(DeviceInspectionError::Cancelled);
            }

            let endpoint = match candidate {
                InspectionCandidate::Supported(endpoint) => endpoint,
                InspectionCandidate::Unsupported { source, message } => {
                    issues.push(DeviceInspectionIssue {
                        source,
                        kind: DeviceInspectionIssueKind::UnsupportedEndpoint,
                        message,
                    });
                    continue;
                }
            };

            let attempt = tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(DeviceInspectionError::Cancelled);
                }
                attempt = inspector.fetch_snapshot_summary(
                    &endpoint,
                    device.device_id(),
                    cancellation,
                ) => attempt,
            };
            let details = match attempt {
                Ok(details) if details.device_id == device.device_id() => details,
                Ok(details) => {
                    issues.push(DeviceInspectionIssue {
                        source: endpoint.source(),
                        kind: DeviceInspectionIssueKind::SnapshotFailed,
                        message: format!(
                            "device metadata identifies {}, expected {}",
                            details.device_id,
                            device.device_id()
                        ),
                    });
                    continue;
                }
                Err(InspectionAttemptError::Cancelled) => {
                    return Err(DeviceInspectionError::Cancelled);
                }
                Err(InspectionAttemptError::Failed(message)) => {
                    issues.push(DeviceInspectionIssue {
                        source: endpoint.source(),
                        kind: DeviceInspectionIssueKind::SnapshotFailed,
                        message,
                    });
                    continue;
                }
            };

            summary = Some(DeviceInspectionSummary {
                device_id: details.device_id,
                source: endpoint.source(),
                friendly_name: details.friendly_name,
                model_number: details.model_number,
                firmware_version: details.firmware_version,
                tuner_count: details.tuner_count,
                channel_count: details.channel_count,
                favorite_count: details.favorite_count,
                drm_count: details.drm_count,
            });
            break;
        }

        devices.push(DeviceInspection {
            device_id: device.device_id(),
            supported_locator_count,
            issues,
            summary,
        });
    }

    if cancellation.is_cancelled() {
        return Err(DeviceInspectionError::Cancelled);
    }
    Ok(DeviceInspectionReport { devices })
}

fn validate_observation_bounds(
    observation: &crate::discovery::DiscoveryObservation,
) -> Result<(), DeviceInspectionError> {
    if let Some(interface) = &observation.interface {
        require_report_limit(
            "interface-name bytes",
            interface.len(),
            MAX_INSPECTION_INTERFACE_BYTES,
        )?;
    }
    require_report_limit(
        "device types",
        observation.device_types.len(),
        MAX_INSPECTION_DEVICE_TYPES,
    )?;
    for value in [
        observation.advertised_base_url.as_deref(),
        observation.advertised_lineup_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        require_report_limit(
            "advertised-URL bytes",
            value.len(),
            MAX_ADVERTISED_URL_BYTES,
        )?;
    }
    Ok(())
}

fn require_report_limit(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), DeviceInspectionError> {
    if actual > maximum {
        return Err(DeviceInspectionError::ReportLimit {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

async fn inspect_discovery_report_with_deadline<I: SnapshotInspector>(
    inspector: &I,
    report: &DiscoveryReport,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<DeviceInspectionReport, DeviceInspectionError> {
    if cancellation.is_cancelled() {
        return Err(DeviceInspectionError::Cancelled);
    }
    timeout(
        deadline,
        inspect_discovery_report(inspector, report, cancellation),
    )
    .await
    .map_err(|_| DeviceInspectionError::Deadline { deadline })?
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::discovery::{DiscoveryMethod, DiscoveryObservation};

    const FIRST_ID: u32 = 0x105A_1232;
    const SECOND_ID: u32 = 0x105A_1243;

    #[tokio::test]
    async fn retries_each_locator_with_the_same_expected_identity() {
        let expected = DeviceId::new(FIRST_ID).unwrap();
        let inspector = FakeInspector::new(vec![
            Err(InspectionAttemptError::Failed("fixture failure".to_owned())),
            Ok(details(expected, "Fallback tuner")),
        ]);
        let report = DiscoveryReport {
            observations: vec![
                observation(FIRST_ID, 65_002, DiscoveryMethod::Targeted),
                observation(FIRST_ID, 65_001, DiscoveryMethod::Ipv4Broadcast),
            ],
            ..DiscoveryReport::default()
        };

        let result = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.attempted_devices(), 1);
        assert_eq!(result.failed_devices(), 0);
        let inspection = &result.devices()[0];
        assert_eq!(inspection.supported_locator_count(), 2);
        assert_eq!(inspection.summary().unwrap().device_id(), expected);
        assert_eq!(inspection.summary().unwrap().source().port(), 65_001);
        assert_eq!(inspection.issues().len(), 1);
        assert_eq!(
            inspector.attempts(),
            vec![(65_002, expected), (65_001, expected)]
        );
    }

    #[tokio::test]
    async fn preferred_success_counts_every_supported_locator() {
        let expected = DeviceId::new(FIRST_ID).unwrap();
        let inspector = FakeInspector::new(vec![Ok(details(expected, "Preferred tuner"))]);
        let report = DiscoveryReport {
            observations: vec![
                observation(FIRST_ID, 65_003, DiscoveryMethod::Targeted),
                observation(FIRST_ID, 65_002, DiscoveryMethod::Ipv4Broadcast),
                observation(FIRST_ID, 65_001, DiscoveryMethod::Ipv6LinkLocalMulticast),
            ],
            ..DiscoveryReport::default()
        };

        let result = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.devices()[0].supported_locator_count(), 3);
        assert_eq!(inspector.attempts(), vec![(65_003, expected)]);
    }

    #[tokio::test]
    async fn mismatched_summary_identity_is_never_published() {
        let expected = DeviceId::new(FIRST_ID).unwrap();
        let wrong = DeviceId::new(SECOND_ID).unwrap();
        let inspector = FakeInspector::new(vec![Ok(details(wrong, "Wrong tuner"))]);
        let report = DiscoveryReport {
            observations: vec![observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted)],
            ..DiscoveryReport::default()
        };

        let result = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.failed_devices(), 1);
        assert!(result.devices()[0].summary().is_none());
        assert_eq!(
            result.devices()[0].issues()[0].kind(),
            DeviceInspectionIssueKind::SnapshotFailed
        );
        assert!(
            !result.devices()[0].issues()[0]
                .message()
                .contains("Wrong tuner")
        );
        assert_eq!(inspector.attempts(), vec![(65_001, expected)]);
    }

    #[tokio::test]
    async fn conflicting_locator_identity_fails_before_http() {
        let inspector = FakeInspector::new(Vec::new());
        let report = DiscoveryReport {
            observations: vec![
                observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted),
                observation(SECOND_ID, 65_001, DiscoveryMethod::Targeted),
            ],
            ..DiscoveryReport::default()
        };

        let error = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            DeviceInspectionError::Registry(RegistryError::LocatorConflict { .. })
        ));
        assert!(inspector.attempts().is_empty());
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_uncooperative_attempt() {
        let report = DiscoveryReport {
            observations: vec![observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted)],
            ..DiscoveryReport::default()
        };
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel.cancel();
        });

        let error = inspect_discovery_report(&PendingInspector, &report, &cancellation)
            .await
            .unwrap_err();

        assert_eq!(error, DeviceInspectionError::Cancelled);
    }

    #[tokio::test]
    async fn pre_cancelled_report_skips_http_even_with_an_elapsed_deadline() {
        let inspector = FakeInspector::new(Vec::new());
        let report = DiscoveryReport {
            observations: vec![observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted)],
            ..DiscoveryReport::default()
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = inspect_discovery_report_with_deadline(
            &inspector,
            &report,
            &cancellation,
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert_eq!(error, DeviceInspectionError::Cancelled);
        assert!(inspector.attempts().is_empty());
    }

    #[tokio::test]
    async fn oversized_public_report_is_rejected_before_cloning_or_http() {
        let inspector = FakeInspector::new(Vec::new());
        let report = DiscoveryReport {
            observations: vec![
                observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted);
                MAX_INSPECTION_OBSERVATIONS + 1
            ],
            ..DiscoveryReport::default()
        };

        let error = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap_err();

        assert_eq!(
            error,
            DeviceInspectionError::ReportLimit {
                field: "observations",
                actual: MAX_INSPECTION_OBSERVATIONS + 1,
                maximum: MAX_INSPECTION_OBSERVATIONS,
            }
        );
        assert!(inspector.attempts().is_empty());
    }

    #[tokio::test]
    async fn oversized_public_observation_field_is_rejected_before_clone_or_http() {
        let inspector = FakeInspector::new(Vec::new());
        let mut unsafe_observation = observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted);
        unsafe_observation.advertised_base_url = Some("x".repeat(MAX_ADVERTISED_URL_BYTES + 1));
        let report = DiscoveryReport {
            observations: vec![unsafe_observation],
            ..DiscoveryReport::default()
        };

        let error = inspect_discovery_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap_err();

        assert_eq!(
            error,
            DeviceInspectionError::ReportLimit {
                field: "advertised-URL bytes",
                actual: MAX_ADVERTISED_URL_BYTES + 1,
                maximum: MAX_ADVERTISED_URL_BYTES,
            }
        );
        assert!(inspector.attempts().is_empty());
    }

    #[tokio::test]
    async fn report_deadline_bounds_all_locator_work() {
        let report = DiscoveryReport {
            observations: vec![observation(FIRST_ID, 65_001, DiscoveryMethod::Targeted)],
            ..DiscoveryReport::default()
        };
        let error = inspect_discovery_report_with_deadline(
            &PendingInspector,
            &report,
            &CancellationToken::new(),
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error,
            DeviceInspectionError::Deadline {
                deadline: Duration::ZERO
            }
        );
    }

    fn observation(
        device_id: u32,
        source_port: u16,
        method: DiscoveryMethod,
    ) -> DiscoveryObservation {
        DiscoveryObservation {
            device_id: DeviceId::new(device_id).unwrap(),
            source: SocketAddr::new("127.0.0.1".parse().unwrap(), source_port),
            method,
            interface: None,
            device_types: vec![1],
            tuner_count: Some(4),
            advertised_base_url: Some("http://127.0.0.1/".to_owned()),
            advertised_lineup_url: None,
        }
    }

    fn details(device_id: DeviceId, name: &str) -> InspectionDetails {
        InspectionDetails {
            device_id,
            friendly_name: Some(name.to_owned()),
            model_number: None,
            firmware_version: None,
            tuner_count: Some(4),
            channel_count: 0,
            favorite_count: 0,
            drm_count: 0,
        }
    }

    struct FakeInspector {
        attempts: Mutex<Vec<(u16, DeviceId)>>,
        outcomes: Mutex<VecDeque<Result<InspectionDetails, InspectionAttemptError>>>,
    }

    impl FakeInspector {
        fn new(outcomes: Vec<Result<InspectionDetails, InspectionAttemptError>>) -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
                outcomes: Mutex::new(outcomes.into()),
            }
        }

        fn attempts(&self) -> Vec<(u16, DeviceId)> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl SnapshotInspector for FakeInspector {
        async fn fetch_snapshot_summary(
            &self,
            endpoint: &DeviceEndpoint,
            expected_device_id: DeviceId,
            _cancellation: &CancellationToken,
        ) -> Result<InspectionDetails, InspectionAttemptError> {
            self.attempts
                .lock()
                .unwrap()
                .push((endpoint.source().port(), expected_device_id));
            self.outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("one fake outcome per expected attempt")
        }
    }

    struct PendingInspector;

    impl SnapshotInspector for PendingInspector {
        async fn fetch_snapshot_summary(
            &self,
            _endpoint: &DeviceEndpoint,
            _expected_device_id: DeviceId,
            _cancellation: &CancellationToken,
        ) -> Result<InspectionDetails, InspectionAttemptError> {
            std::future::pending().await
        }
    }
}
