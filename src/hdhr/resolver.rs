//! Identity-safe resolution of one selected device's complete snapshot.
//!
//! This is deliberately separate from [`super::DeviceInspector`]. Inspection
//! publishes metadata and counts; selection resolution is the narrow core
//! boundary that may return responder-pinned stream URLs for later playback.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use thiserror::Error;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use super::fallback::preferred_first_locators;
use super::{
    DeviceEndpoint, DeviceHttpClient, DeviceHttpError, DeviceSnapshot, DeviceSnapshotError,
    LineupFetchError,
};
use crate::discovery::{DeviceRegistry, RegisteredDevice};
use crate::domain::DeviceId;

/// Overall monotonic budget for resolving one selected device, including all
/// deterministic locator fallbacks.
pub const DEFAULT_DEVICE_SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
/// Maximum locator candidates copied into one selected-device target.
pub const MAX_DEVICE_SNAPSHOT_LOCATORS: usize = DeviceRegistry::ABSOLUTE_MAX_LOCATORS_PER_DEVICE;
/// Maximum structured issues retained by one resolution operation.
pub const MAX_DEVICE_SNAPSHOT_ISSUES: usize = MAX_DEVICE_SNAPSHOT_LOCATORS;

/// An opaque, bounded copy of the registry evidence needed to resolve one
/// selected DeviceID. Its debug representation never exposes network locators.
#[derive(Clone)]
pub struct DeviceSnapshotTarget {
    device_id: DeviceId,
    candidates: Vec<SnapshotCandidate>,
    supported_locator_count: usize,
}

impl DeviceSnapshotTarget {
    /// Freeze the preferred-first locator order for one registered device.
    /// Unsupported or unsafe advertised endpoints are retained only as
    /// redacted issue slots and can never reach the HTTP client.
    pub fn from_registered(device: &RegisteredDevice) -> Result<Self, DeviceSnapshotTargetError> {
        let locators = preferred_first_locators(device);
        if locators.is_empty() {
            return Err(DeviceSnapshotTargetError::NoLocators);
        }
        if locators.len() > MAX_DEVICE_SNAPSHOT_LOCATORS {
            return Err(DeviceSnapshotTargetError::TooManyLocators {
                actual: locators.len(),
                maximum: MAX_DEVICE_SNAPSHOT_LOCATORS,
            });
        }

        let candidates = locators
            .into_iter()
            .map(|locator| {
                DeviceEndpoint::from_locator(locator)
                    .map_or(SnapshotCandidate::Unsupported, |endpoint| {
                        SnapshotCandidate::Supported(Box::new(endpoint))
                    })
            })
            .collect::<Vec<_>>();
        let supported_locator_count = candidates
            .iter()
            .filter(|candidate| matches!(candidate, SnapshotCandidate::Supported(_)))
            .count();

        Ok(Self {
            device_id: device.device_id(),
            candidates,
            supported_locator_count,
        })
    }

    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub fn locator_count(&self) -> usize {
        self.candidates.len()
    }

    #[must_use]
    pub const fn supported_locator_count(&self) -> usize {
        self.supported_locator_count
    }
}

impl TryFrom<&RegisteredDevice> for DeviceSnapshotTarget {
    type Error = DeviceSnapshotTargetError;

    fn try_from(device: &RegisteredDevice) -> Result<Self, Self::Error> {
        Self::from_registered(device)
    }
}

impl fmt::Debug for DeviceSnapshotTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceSnapshotTarget")
            .field("device_id", &self.device_id)
            .field("locator_count", &self.candidates.len())
            .field("supported_locator_count", &self.supported_locator_count)
            .finish()
    }
}

#[derive(Clone)]
enum SnapshotCandidate {
    Supported(Box<DeviceEndpoint>),
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DeviceSnapshotTargetError {
    #[error("registered device has no locator candidates")]
    NoLocators,

    #[error("registered device has {actual} locators; maximum is {maximum}")]
    TooManyLocators { actual: usize, maximum: usize },
}

/// Fixed, redacted reason that one locator could not provide the selected
/// device's identity-checked snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceSnapshotIssueKind {
    UnsupportedEndpoint,
    IdentityMismatch,
    MetadataUnreachable,
    MetadataInvalid,
    LineupUnreachable,
    LineupInvalid,
}

/// One bounded locator outcome. The ordinal is one-based within the opaque
/// target; no address, advertised URL, response body, or dynamic error text is
/// retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceSnapshotIssue {
    locator_ordinal: u8,
    kind: DeviceSnapshotIssueKind,
}

impl DeviceSnapshotIssue {
    #[must_use]
    pub const fn locator_ordinal(self) -> u8 {
        self.locator_ordinal
    }

    #[must_use]
    pub const fn kind(self) -> DeviceSnapshotIssueKind {
        self.kind
    }
}

/// Successful selected-device resolution. The complete URL-bearing snapshot
/// remains in this HDHomeRun core type, whose debug representation publishes
/// only identity and bounded counts.
#[derive(Clone)]
pub struct ResolvedDeviceSnapshot {
    snapshot: DeviceSnapshot,
    selected_source: IpAddr,
    selected_locator_ordinal: u8,
    http_attempt_count: usize,
    issues: Vec<DeviceSnapshotIssue>,
}

impl ResolvedDeviceSnapshot {
    #[must_use]
    pub fn snapshot(&self) -> &DeviceSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn into_snapshot(self) -> DeviceSnapshot {
        self.snapshot
    }

    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.snapshot.info().device_id()
    }

    /// Return the responder address that supplied this complete snapshot.
    ///
    /// This stays crate-private so UI projections cannot acquire topology;
    /// the controller uses it only to revalidate a stream origin at handoff.
    #[must_use]
    pub(crate) const fn selected_source(&self) -> IpAddr {
        self.selected_source
    }

    #[must_use]
    pub const fn selected_locator_ordinal(&self) -> u8 {
        self.selected_locator_ordinal
    }

    #[must_use]
    pub const fn http_attempt_count(&self) -> usize {
        self.http_attempt_count
    }

    pub fn issues(&self) -> &[DeviceSnapshotIssue] {
        &self.issues
    }

    #[cfg(test)]
    pub(crate) fn controller_test_fixture(device_id: DeviceId) -> Self {
        Self {
            snapshot: DeviceSnapshot::debug_redaction_fixture(device_id),
            selected_source: "127.0.0.1".parse().unwrap(),
            selected_locator_ordinal: 1,
            http_attempt_count: 1,
            issues: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn controller_stream_test_fixture(
        device_id: DeviceId,
        protected: bool,
        selected_source: std::net::IpAddr,
    ) -> Self {
        Self {
            snapshot: DeviceSnapshot::stream_handoff_test_fixture(device_id, protected),
            selected_source,
            selected_locator_ordinal: 1,
            http_attempt_count: 1,
            issues: Vec::new(),
        }
    }
}

impl fmt::Debug for ResolvedDeviceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedDeviceSnapshot")
            .field("device_id", &self.device_id())
            .field("selected_locator_ordinal", &self.selected_locator_ordinal)
            .field("http_attempt_count", &self.http_attempt_count)
            .field("issue_count", &self.issues.len())
            .field("channel_count", &self.snapshot.lineup().channels().len())
            .finish()
    }
}

/// A bounded, redacted description of an unsuccessful fallback sequence.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error(
    "no identity-checked snapshot was available for {device_id} after {http_attempt_count} HTTP attempts"
)]
pub struct DeviceSnapshotUnavailable {
    device_id: DeviceId,
    http_attempt_count: usize,
    issues: Vec<DeviceSnapshotIssue>,
}

impl DeviceSnapshotUnavailable {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub const fn http_attempt_count(&self) -> usize {
        self.http_attempt_count
    }

    pub fn issues(&self) -> &[DeviceSnapshotIssue] {
        &self.issues
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DeviceSnapshotResolutionError {
    #[error("selected-device snapshot resolution was cancelled")]
    Cancelled,

    #[error("selected-device snapshot resolution exceeded its {deadline:?} overall deadline")]
    Deadline { deadline: Duration },

    #[error(transparent)]
    Unavailable(#[from] DeviceSnapshotUnavailable),
}

#[cfg(test)]
impl DeviceSnapshotResolutionError {
    pub(crate) fn controller_test_unavailable(
        device_id: DeviceId,
        kinds: &[DeviceSnapshotIssueKind],
    ) -> Self {
        let issues = kinds
            .iter()
            .copied()
            .enumerate()
            .map(|(index, kind)| DeviceSnapshotIssue {
                locator_ordinal: u8::try_from(index + 1).unwrap(),
                kind,
            })
            .collect::<Vec<_>>();
        DeviceSnapshotUnavailable {
            device_id,
            http_attempt_count: issues
                .iter()
                .filter(|issue| issue.kind != DeviceSnapshotIssueKind::UnsupportedEndpoint)
                .count(),
            issues,
        }
        .into()
    }
}

/// Resolves one frozen selected-device target with a single overall deadline.
#[derive(Clone, Debug)]
pub struct DeviceSnapshotResolver {
    http: DeviceHttpClient,
}

impl DeviceSnapshotResolver {
    #[must_use]
    pub const fn new(http: DeviceHttpClient) -> Self {
        Self { http }
    }

    /// Try each candidate sequentially in the target's frozen preferred-first
    /// order. Cancellation is terminal; endpoint and snapshot failures fall
    /// back while the one monotonic operation budget remains.
    pub async fn resolve(
        &self,
        target: &DeviceSnapshotTarget,
        cancellation: &CancellationToken,
    ) -> Result<ResolvedDeviceSnapshot, DeviceSnapshotResolutionError> {
        let resolution = resolve_with_deadline(
            &self.http,
            target,
            cancellation,
            DEFAULT_DEVICE_SNAPSHOT_DEADLINE,
        )
        .await?;
        Ok(ResolvedDeviceSnapshot {
            snapshot: resolution.snapshot,
            selected_source: resolution.selected_source,
            selected_locator_ordinal: resolution.selected_locator_ordinal,
            http_attempt_count: resolution.http_attempt_count,
            issues: resolution.issues,
        })
    }
}

impl Default for DeviceSnapshotResolver {
    fn default() -> Self {
        Self::new(DeviceHttpClient::default())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotAttemptError {
    Cancelled,
    Failed(DeviceSnapshotIssueKind),
}

trait SnapshotFetcher {
    type Snapshot;

    async fn fetch_snapshot(
        &self,
        endpoint: &DeviceEndpoint,
        expected_device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<Self::Snapshot, SnapshotAttemptError>;

    fn snapshot_device_id(snapshot: &Self::Snapshot) -> DeviceId;
}

impl SnapshotFetcher for DeviceHttpClient {
    type Snapshot = DeviceSnapshot;

    async fn fetch_snapshot(
        &self,
        endpoint: &DeviceEndpoint,
        expected_device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<Self::Snapshot, SnapshotAttemptError> {
        self.fetch_device_snapshot(endpoint, expected_device_id, cancellation)
            .await
            .map_err(classify_snapshot_error)
    }

    fn snapshot_device_id(snapshot: &Self::Snapshot) -> DeviceId {
        snapshot.info().device_id()
    }
}

fn classify_snapshot_error(error: DeviceSnapshotError) -> SnapshotAttemptError {
    match error {
        DeviceSnapshotError::Metadata(DeviceHttpError::Cancelled)
        | DeviceSnapshotError::Lineup(LineupFetchError::Http(DeviceHttpError::Cancelled)) => {
            SnapshotAttemptError::Cancelled
        }
        DeviceSnapshotError::Metadata(DeviceHttpError::DeviceIdMismatch { .. }) => {
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::IdentityMismatch)
        }
        DeviceSnapshotError::Metadata(error) => classify_http_error(
            error,
            DeviceSnapshotIssueKind::MetadataUnreachable,
            DeviceSnapshotIssueKind::MetadataInvalid,
        ),
        DeviceSnapshotError::Lineup(LineupFetchError::Http(
            DeviceHttpError::DeviceIdMismatch { .. },
        )) => SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::IdentityMismatch),
        DeviceSnapshotError::Lineup(LineupFetchError::Http(error)) => classify_http_error(
            error,
            DeviceSnapshotIssueKind::LineupUnreachable,
            DeviceSnapshotIssueKind::LineupInvalid,
        ),
        DeviceSnapshotError::Lineup(LineupFetchError::Lineup(_)) => {
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::LineupInvalid)
        }
    }
}

fn classify_http_error(
    error: DeviceHttpError,
    unreachable: DeviceSnapshotIssueKind,
    invalid: DeviceSnapshotIssueKind,
) -> SnapshotAttemptError {
    match error {
        DeviceHttpError::Cancelled => SnapshotAttemptError::Cancelled,
        DeviceHttpError::DeviceIdMismatch { .. } => {
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::IdentityMismatch)
        }
        DeviceHttpError::Transport(_)
        | DeviceHttpError::UnexpectedStatus { .. }
        | DeviceHttpError::Deadline { .. } => SnapshotAttemptError::Failed(unreachable),
        DeviceHttpError::BodyTooLarge { .. }
        | DeviceHttpError::Json(_)
        | DeviceHttpError::InvalidDeviceId
        | DeviceHttpError::InvalidField { .. }
        | DeviceHttpError::Endpoint(_) => SnapshotAttemptError::Failed(invalid),
    }
}

#[derive(Debug)]
struct Resolution<T> {
    snapshot: T,
    selected_source: IpAddr,
    selected_locator_ordinal: u8,
    http_attempt_count: usize,
    issues: Vec<DeviceSnapshotIssue>,
}

async fn resolve_with_deadline<F: SnapshotFetcher>(
    fetcher: &F,
    target: &DeviceSnapshotTarget,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<Resolution<F::Snapshot>, DeviceSnapshotResolutionError> {
    if cancellation.is_cancelled() {
        return Err(DeviceSnapshotResolutionError::Cancelled);
    }

    let expires_at = Instant::now()
        .checked_add(deadline)
        .expect("bounded snapshot deadlines fit Tokio's monotonic clock");
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(DeviceSnapshotResolutionError::Cancelled),
        result = timeout_at(
            expires_at,
            resolve_candidates(fetcher, target, cancellation),
        ) => result.map_err(|_| DeviceSnapshotResolutionError::Deadline { deadline })?,
    }
}

async fn resolve_candidates<F: SnapshotFetcher>(
    fetcher: &F,
    target: &DeviceSnapshotTarget,
    cancellation: &CancellationToken,
) -> Result<Resolution<F::Snapshot>, DeviceSnapshotResolutionError> {
    let mut issues = Vec::with_capacity(target.candidates.len());
    let mut http_attempt_count = 0;

    for (index, candidate) in target.candidates.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(DeviceSnapshotResolutionError::Cancelled);
        }
        let ordinal = u8::try_from(index + 1)
            .expect("snapshot target locator count is strictly bounded below u8::MAX");

        let SnapshotCandidate::Supported(endpoint) = candidate else {
            push_issue(
                &mut issues,
                DeviceSnapshotIssue {
                    locator_ordinal: ordinal,
                    kind: DeviceSnapshotIssueKind::UnsupportedEndpoint,
                },
            );
            continue;
        };

        http_attempt_count += 1;
        let attempt = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(DeviceSnapshotResolutionError::Cancelled);
            }
            attempt = fetcher.fetch_snapshot(endpoint, target.device_id, cancellation) => attempt,
        };
        let snapshot = match attempt {
            Ok(snapshot) if F::snapshot_device_id(&snapshot) == target.device_id => snapshot,
            Ok(_) => {
                push_issue(
                    &mut issues,
                    DeviceSnapshotIssue {
                        locator_ordinal: ordinal,
                        kind: DeviceSnapshotIssueKind::IdentityMismatch,
                    },
                );
                continue;
            }
            Err(SnapshotAttemptError::Cancelled) => {
                return Err(DeviceSnapshotResolutionError::Cancelled);
            }
            Err(SnapshotAttemptError::Failed(kind)) => {
                push_issue(
                    &mut issues,
                    DeviceSnapshotIssue {
                        locator_ordinal: ordinal,
                        kind,
                    },
                );
                continue;
            }
        };

        if cancellation.is_cancelled() {
            return Err(DeviceSnapshotResolutionError::Cancelled);
        }
        return Ok(Resolution {
            snapshot,
            selected_source: endpoint.source().ip(),
            selected_locator_ordinal: ordinal,
            http_attempt_count,
            issues,
        });
    }

    Err(DeviceSnapshotUnavailable {
        device_id: target.device_id,
        http_attempt_count,
        issues,
    }
    .into())
}

fn push_issue(issues: &mut Vec<DeviceSnapshotIssue>, issue: DeviceSnapshotIssue) {
    assert!(
        issues.len() < MAX_DEVICE_SNAPSHOT_ISSUES,
        "validated snapshot targets cannot exceed the issue bound"
    );
    issues.push(issue);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::IpAddr;
    use std::sync::Mutex;

    use super::*;
    use crate::discovery::{
        DeviceRegistry, DiscoveryMethod, DiscoveryObservation, RegistryInstant,
    };

    const EXPECTED_ID: u32 = 0x105A_1232;
    const OTHER_ID: u32 = 0x105A_1243;

    #[test]
    fn production_snapshot_errors_map_to_fixed_redacted_kinds() {
        let expected = DeviceId::new(EXPECTED_ID).unwrap();
        let wrong = DeviceId::new(OTHER_ID).unwrap();

        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Metadata(
                DeviceHttpError::DeviceIdMismatch {
                    expected,
                    actual: wrong,
                }
            )),
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::IdentityMismatch)
        );
        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Metadata(
                DeviceHttpError::UnexpectedStatus {
                    operation: "fetch device metadata",
                    status: 404,
                }
            )),
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::MetadataUnreachable)
        );
        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Metadata(
                DeviceHttpError::BodyTooLarge {
                    operation: "fetch device metadata",
                    maximum: 1,
                }
            )),
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::MetadataInvalid)
        );
        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Lineup(LineupFetchError::Http(
                DeviceHttpError::UnexpectedStatus {
                    operation: "fetch channel lineup",
                    status: 404,
                }
            ))),
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::LineupUnreachable)
        );
        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Lineup(LineupFetchError::Lineup(
                crate::hdhr::LineupError::TooManyChannels {
                    actual: 2,
                    maximum: 1,
                }
            ))),
            SnapshotAttemptError::Failed(DeviceSnapshotIssueKind::LineupInvalid)
        );
        assert_eq!(
            classify_snapshot_error(DeviceSnapshotError::Lineup(LineupFetchError::Http(
                DeviceHttpError::Cancelled
            ))),
            SnapshotAttemptError::Cancelled
        );
    }

    #[tokio::test]
    async fn preferred_success_does_not_probe_fallback_locators() {
        let target = target_with_supported_locators(&["192.0.2.1", "192.0.2.3"]);
        let expected = target.device_id();
        let fetcher = FakeFetcher::new(vec![FakeOutcome::Result(Ok(FakeSnapshot(expected)))]);

        let resolution = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(resolution.snapshot, FakeSnapshot(expected));
        assert_eq!(resolution.selected_source, ip("192.0.2.3"));
        assert_eq!(resolution.selected_locator_ordinal, 1);
        assert_eq!(resolution.http_attempt_count, 1);
        assert!(resolution.issues.is_empty());
        assert_eq!(fetcher.attempts(), vec![(ip("192.0.2.3"), expected)]);
    }

    #[tokio::test]
    async fn fallback_is_preferred_first_then_address_order() {
        let target = target_with_supported_locators(&["192.0.2.2", "192.0.2.1", "192.0.2.3"]);
        let expected = target.device_id();
        let fetcher = FakeFetcher::new(vec![
            FakeOutcome::Result(Err(SnapshotAttemptError::Failed(
                DeviceSnapshotIssueKind::MetadataUnreachable,
            ))),
            FakeOutcome::Result(Ok(FakeSnapshot(expected))),
        ]);

        let resolution = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(resolution.selected_locator_ordinal, 2);
        assert_eq!(resolution.selected_source, ip("192.0.2.1"));
        assert_eq!(resolution.http_attempt_count, 2);
        assert_eq!(
            resolution.issues,
            vec![DeviceSnapshotIssue {
                locator_ordinal: 1,
                kind: DeviceSnapshotIssueKind::MetadataUnreachable,
            }]
        );
        assert_eq!(
            fetcher.attempts(),
            vec![(ip("192.0.2.3"), expected), (ip("192.0.2.1"), expected),]
        );
    }

    #[tokio::test]
    async fn identity_mismatch_and_missing_lineup_never_publish_a_snapshot() {
        let target = target_with_supported_locators(&["192.0.2.1", "192.0.2.2"]);
        let expected = target.device_id();
        let wrong = DeviceId::new(OTHER_ID).unwrap();
        let fetcher = FakeFetcher::new(vec![
            FakeOutcome::Result(Ok(FakeSnapshot(wrong))),
            FakeOutcome::Result(Err(SnapshotAttemptError::Failed(
                DeviceSnapshotIssueKind::LineupInvalid,
            ))),
        ]);

        let error = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        let DeviceSnapshotResolutionError::Unavailable(unavailable) = error else {
            panic!("expected bounded unavailable result");
        };

        assert_eq!(unavailable.device_id(), expected);
        assert_eq!(unavailable.http_attempt_count(), 2);
        assert_eq!(
            unavailable.issues(),
            [
                DeviceSnapshotIssue {
                    locator_ordinal: 1,
                    kind: DeviceSnapshotIssueKind::IdentityMismatch,
                },
                DeviceSnapshotIssue {
                    locator_ordinal: 2,
                    kind: DeviceSnapshotIssueKind::LineupInvalid,
                },
            ]
        );
    }

    #[tokio::test]
    async fn fetcher_cancellation_is_terminal_without_fallback() {
        let target = target_with_supported_locators(&["192.0.2.1", "192.0.2.2"]);
        let expected = target.device_id();
        let fetcher = FakeFetcher::new(vec![
            FakeOutcome::Result(Err(SnapshotAttemptError::Cancelled)),
            FakeOutcome::Result(Ok(FakeSnapshot(expected))),
        ]);

        let error = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert_eq!(error, DeviceSnapshotResolutionError::Cancelled);
        assert_eq!(fetcher.attempts().len(), 1);
    }

    #[tokio::test]
    async fn token_cancellation_interrupts_an_uncooperative_fetcher() {
        let target = target_with_supported_locators(&["192.0.2.1", "192.0.2.2"]);
        let fetcher = FakeFetcher::new(vec![FakeOutcome::Pending]);
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel.cancel();
        });

        let error = resolve_with_deadline(&fetcher, &target, &cancellation, Duration::from_secs(1))
            .await
            .unwrap_err();

        assert_eq!(error, DeviceSnapshotResolutionError::Cancelled);
        assert_eq!(fetcher.attempts().len(), 1);
    }

    #[tokio::test]
    async fn one_deadline_bounds_the_complete_fallback_sequence() {
        let target = target_with_supported_locators(&["192.0.2.1", "192.0.2.2"]);
        let fetcher = FakeFetcher::new(vec![FakeOutcome::Pending]);
        let deadline = Duration::from_millis(1);

        let error = resolve_with_deadline(&fetcher, &target, &CancellationToken::new(), deadline)
            .await
            .unwrap_err();

        assert_eq!(error, DeviceSnapshotResolutionError::Deadline { deadline });
        assert_eq!(fetcher.attempts().len(), 1);
    }

    #[tokio::test]
    async fn unsafe_preferred_locator_is_filtered_before_http() {
        let expected = DeviceId::new(EXPECTED_ID).unwrap();
        let mut registry = DeviceRegistry::default();
        observe(
            &mut registry,
            "192.0.2.1",
            1,
            Some("http://192.0.2.1/".to_owned()),
        );
        observe(
            &mut registry,
            "192.0.2.3",
            2,
            Some("http://operator:topsecret@192.0.2.3/".to_owned()),
        );
        let target =
            DeviceSnapshotTarget::from_registered(registry.get(expected).unwrap()).unwrap();
        let fetcher = FakeFetcher::new(vec![FakeOutcome::Result(Ok(FakeSnapshot(expected)))]);

        assert_eq!(target.locator_count(), 2);
        assert_eq!(target.supported_locator_count(), 1);
        let target_debug = format!("{target:?}");
        assert!(!target_debug.contains("192.0.2"));
        assert!(!target_debug.contains("topsecret"));

        let resolution = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();

        assert_eq!(resolution.selected_locator_ordinal, 2);
        assert_eq!(resolution.http_attempt_count, 1);
        assert_eq!(
            resolution.issues,
            [DeviceSnapshotIssue {
                locator_ordinal: 1,
                kind: DeviceSnapshotIssueKind::UnsupportedEndpoint,
            }]
        );
        assert_eq!(fetcher.attempts(), vec![(ip("192.0.2.1"), expected)]);
    }

    #[tokio::test]
    async fn issue_storage_is_hard_bounded_and_redacted() {
        let expected = DeviceId::new(EXPECTED_ID).unwrap();
        let mut registry = DeviceRegistry::with_limits(1, MAX_DEVICE_SNAPSHOT_LOCATORS, 1).unwrap();
        for suffix in 1..=MAX_DEVICE_SNAPSHOT_LOCATORS {
            let address = format!("192.0.2.{suffix}");
            observe(
                &mut registry,
                &address,
                u64::try_from(suffix).unwrap(),
                Some(format!("https://credential-{suffix}@{address}/")),
            );
        }
        let target =
            DeviceSnapshotTarget::from_registered(registry.get(expected).unwrap()).unwrap();
        let fetcher = FakeFetcher::new(Vec::new());

        let error = resolve_with_deadline(
            &fetcher,
            &target,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        let rendered = format!("{error:?} {error}");
        let DeviceSnapshotResolutionError::Unavailable(unavailable) = error else {
            panic!("expected unavailable result");
        };

        assert_eq!(target.locator_count(), MAX_DEVICE_SNAPSHOT_LOCATORS);
        assert_eq!(target.supported_locator_count(), 0);
        assert_eq!(unavailable.http_attempt_count(), 0);
        assert_eq!(unavailable.issues().len(), MAX_DEVICE_SNAPSHOT_ISSUES);
        assert!(
            unavailable
                .issues()
                .iter()
                .all(|issue| issue.kind() == DeviceSnapshotIssueKind::UnsupportedEndpoint)
        );
        assert!(fetcher.attempts().is_empty());
        assert!(!rendered.contains("192.0.2"));
        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains("https://"));
    }

    #[test]
    fn snapshot_and_resolution_debug_never_render_stream_urls() {
        let snapshot = DeviceSnapshot::debug_redaction_fixture(DeviceId::new(EXPECTED_ID).unwrap());
        let snapshot_debug = format!("{snapshot:?}");
        let resolution_debug = format!(
            "{:?}",
            ResolvedDeviceSnapshot {
                snapshot,
                selected_source: "127.0.0.1".parse().unwrap(),
                selected_locator_ordinal: 1,
                http_attempt_count: 1,
                issues: Vec::new(),
            }
        );

        for rendered in [snapshot_debug, resolution_debug] {
            assert!(!rendered.contains("127.0.0.1"));
            assert!(!rendered.contains("auto/v5.1"));
            assert!(!rendered.contains("private fixture"));
            assert!(!rendered.contains("private channel"));
        }
    }

    fn target_with_supported_locators(addresses: &[&str]) -> DeviceSnapshotTarget {
        let expected = DeviceId::new(EXPECTED_ID).unwrap();
        let mut registry = DeviceRegistry::default();
        for (index, address) in addresses.iter().enumerate() {
            observe(
                &mut registry,
                address,
                u64::try_from(index + 1).unwrap(),
                Some(format!("http://{address}/")),
            );
        }
        DeviceSnapshotTarget::from_registered(registry.get(expected).unwrap()).unwrap()
    }

    fn observe(
        registry: &mut DeviceRegistry,
        address: &str,
        seen_at: u64,
        advertised_base_url: Option<String>,
    ) {
        registry
            .observe(
                DiscoveryObservation {
                    device_id: DeviceId::new(EXPECTED_ID).unwrap(),
                    source: format!("{address}:65001").parse().unwrap(),
                    method: DiscoveryMethod::Targeted,
                    interface: None,
                    device_types: vec![1],
                    tuner_count: Some(4),
                    advertised_base_url,
                    advertised_lineup_url: None,
                },
                RegistryInstant::from_duration(Duration::from_secs(seen_at)),
            )
            .unwrap();
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct FakeSnapshot(DeviceId);

    enum FakeOutcome {
        Result(Result<FakeSnapshot, SnapshotAttemptError>),
        Pending,
    }

    struct FakeFetcher {
        attempts: Mutex<Vec<(IpAddr, DeviceId)>>,
        outcomes: Mutex<VecDeque<FakeOutcome>>,
    }

    impl FakeFetcher {
        fn new(outcomes: Vec<FakeOutcome>) -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
                outcomes: Mutex::new(outcomes.into()),
            }
        }

        fn attempts(&self) -> Vec<(IpAddr, DeviceId)> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl SnapshotFetcher for FakeFetcher {
        type Snapshot = FakeSnapshot;

        async fn fetch_snapshot(
            &self,
            endpoint: &DeviceEndpoint,
            expected_device_id: DeviceId,
            _cancellation: &CancellationToken,
        ) -> Result<Self::Snapshot, SnapshotAttemptError> {
            self.attempts
                .lock()
                .unwrap()
                .push((endpoint.source().ip(), expected_device_id));
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("one fake outcome per expected HTTP attempt");
            match outcome {
                FakeOutcome::Result(result) => result,
                FakeOutcome::Pending => std::future::pending().await,
            }
        }

        fn snapshot_device_id(snapshot: &Self::Snapshot) -> DeviceId {
            snapshot.0
        }
    }
}
