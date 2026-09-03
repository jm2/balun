use std::collections::BTreeSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ipnet::Ipv4Net;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use super::client::{DiscoveryClient, DiscoveryError, DiscoveryReport, ProbeIssue};
use super::types::{DiscoveryMethod, ProbeEndpoint};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;

#[cfg(target_os = "linux")]
pub(super) mod linux;

/// Hard ceiling for an explicitly approved routed discovery range.
pub const MAX_ROUTED_CANDIDATES: usize = 256;
/// Hard ceiling for the nominal routed discovery packet rate.
pub const MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND: u16 = 64;
/// Hard ceiling for simultaneous targeted probes.
pub const MAX_ROUTED_CONCURRENCY: usize = 16;
/// Default wall-clock budget for one complete routed discovery scan.
pub const DEFAULT_ROUTED_SCAN_DEADLINE: Duration = Duration::from_secs(15);
/// Smallest configurable routed discovery wall-clock budget.
pub const MIN_ROUTED_SCAN_DEADLINE: Duration = Duration::from_millis(1);
/// Hard ceiling for one routed discovery wall-clock budget.
pub const MAX_ROUTED_SCAN_DEADLINE: Duration = Duration::from_secs(30);

/// A private IPv4 range that has passed Balun's routed-scan safety policy.
///
/// Construction validates only the technical boundary. The caller remains
/// responsible for obtaining and persisting the user's approval before
/// starting a scan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ApprovedIpv4Range {
    network: Ipv4Net,
}

impl ApprovedIpv4Range {
    /// Validate a candidate routed range.
    pub fn new(network: Ipv4Net) -> Result<Self, RoutedRangeError> {
        if network.prefix_len() < 24 {
            return Err(RoutedRangeError::TooWide {
                network,
                maximum_prefix: 24,
            });
        }

        if !network.network().is_private() || !network.broadcast().is_private() {
            return Err(RoutedRangeError::NotPrivate(network));
        }

        let candidate_count = network.hosts().count();
        if candidate_count == 0 || candidate_count > MAX_ROUTED_CANDIDATES {
            return Err(RoutedRangeError::CandidateCount {
                network,
                count: candidate_count,
                maximum: MAX_ROUTED_CANDIDATES,
            });
        }

        Ok(Self { network })
    }

    /// Return the canonical network.
    #[must_use]
    pub const fn network(self) -> Ipv4Net {
        self.network
    }

    /// Return usable host addresses, excluding network and broadcast
    /// addresses where IPv4 reserves them.
    pub fn candidates(self) -> impl Iterator<Item = Ipv4Addr> {
        self.network.hosts()
    }
}

/// A deterministic, bounded set of routed IPv4 targets that has received
/// caller-level approval.
///
/// Construction validates only Balun's technical safety boundary. The caller
/// remains responsible for associating the set with explicit user approval
/// before starting a scan. Duplicate addresses are removed before the hard
/// candidate cap is applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovedIpv4Targets {
    addresses: Vec<Ipv4Addr>,
}

impl ApprovedIpv4Targets {
    /// Validate and canonicalize an approved address list.
    pub(crate) fn new(
        targets: impl IntoIterator<Item = Ipv4Addr>,
    ) -> Result<Self, RoutedTargetsError> {
        let mut addresses = BTreeSet::new();
        for address in targets {
            if !address.is_private() {
                return Err(RoutedTargetsError::NotPrivate(address));
            }
            addresses.insert(address);
            if addresses.len() > MAX_ROUTED_CANDIDATES {
                return Err(RoutedTargetsError::TooManyCandidates {
                    count: addresses.len(),
                    maximum: MAX_ROUTED_CANDIDATES,
                });
            }
        }

        Ok(Self {
            addresses: addresses.into_iter().collect(),
        })
    }

    fn from_range(range: ApprovedIpv4Range) -> Self {
        Self::new(range.candidates())
            .expect("an approved routed range always yields bounded private targets")
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.addresses.len()
    }

    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    /// Return canonical targets in ascending address order.
    pub(crate) fn candidates(&self) -> impl ExactSizeIterator<Item = Ipv4Addr> + '_ {
        self.addresses.iter().copied()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum RoutedTargetsError {
    #[error("routed discovery target {0} is not an RFC 1918 private address")]
    NotPrivate(Ipv4Addr),

    #[error("routed discovery has {count} unique targets; the maximum is {maximum}")]
    TooManyCandidates { count: usize, maximum: usize },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RoutedRangeError {
    #[error("routed discovery range {network} is wider than /{maximum_prefix}")]
    TooWide {
        network: Ipv4Net,
        maximum_prefix: u8,
    },

    #[error("routed discovery range {0} is not wholly RFC 1918 private space")]
    NotPrivate(Ipv4Net),

    #[error("routed discovery range {network} has {count} candidates; the maximum is {maximum}")]
    CandidateCount {
        network: Ipv4Net,
        count: usize,
        maximum: usize,
    },
}

/// Neighbor-friendly limits for a routed range scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutedScanConfig {
    wire_datagrams_per_second: u16,
    max_in_flight: usize,
    overall_deadline: Duration,
}

impl RoutedScanConfig {
    pub fn new(
        wire_datagrams_per_second: u16,
        max_in_flight: usize,
    ) -> Result<Self, InvalidRoutedScanConfig> {
        Self::new_with_overall_deadline(
            wire_datagrams_per_second,
            max_in_flight,
            DEFAULT_ROUTED_SCAN_DEADLINE,
        )
    }

    pub fn new_with_overall_deadline(
        wire_datagrams_per_second: u16,
        max_in_flight: usize,
        overall_deadline: Duration,
    ) -> Result<Self, InvalidRoutedScanConfig> {
        if wire_datagrams_per_second == 0
            || wire_datagrams_per_second > MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND
        {
            return Err(InvalidRoutedScanConfig::WireRate {
                value: wire_datagrams_per_second,
                maximum: MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND,
            });
        }
        if max_in_flight == 0 || max_in_flight > MAX_ROUTED_CONCURRENCY {
            return Err(InvalidRoutedScanConfig::Concurrency {
                value: max_in_flight,
                maximum: MAX_ROUTED_CONCURRENCY,
            });
        }
        if !(MIN_ROUTED_SCAN_DEADLINE..=MAX_ROUTED_SCAN_DEADLINE).contains(&overall_deadline) {
            return Err(InvalidRoutedScanConfig::Deadline {
                value: overall_deadline,
                minimum: MIN_ROUTED_SCAN_DEADLINE,
                maximum: MAX_ROUTED_SCAN_DEADLINE,
            });
        }

        Ok(Self {
            wire_datagrams_per_second,
            max_in_flight,
            overall_deadline,
        })
    }

    #[must_use]
    pub const fn wire_datagrams_per_second(self) -> u16 {
        self.wire_datagrams_per_second
    }

    #[must_use]
    pub const fn max_in_flight(self) -> usize {
        self.max_in_flight
    }

    #[must_use]
    pub const fn overall_deadline(self) -> Duration {
        self.overall_deadline
    }

    /// Conservative number of request datagrams for a complete range scan.
    #[must_use]
    pub fn maximum_request_datagrams(
        self,
        range: ApprovedIpv4Range,
        attempts_per_target: u8,
    ) -> usize {
        range.candidates().count() * usize::from(attempts_per_target)
    }

    /// Conservative number of request datagrams for an approved target set.
    #[must_use]
    pub(crate) fn maximum_target_request_datagrams(
        self,
        targets: &ApprovedIpv4Targets,
        attempts_per_target: u8,
    ) -> usize {
        targets.len() * usize::from(attempts_per_target)
    }

    fn target_start_spacing(self, attempts_per_target: u8) -> Duration {
        let numerator = 1_000_000_000_u128 * u128::from(attempts_per_target);
        let denominator = u128::from(self.wire_datagrams_per_second);
        let nanoseconds = numerator.div_ceil(denominator);
        Duration::from_nanos(
            u64::try_from(nanoseconds).expect("bounded routed rate fits in a Duration"),
        )
    }
}

impl Default for RoutedScanConfig {
    fn default() -> Self {
        Self {
            wire_datagrams_per_second: MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND,
            max_in_flight: MAX_ROUTED_CONCURRENCY,
            overall_deadline: DEFAULT_ROUTED_SCAN_DEADLINE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidRoutedScanConfig {
    #[error("routed discovery wire rate must be between 1 and {maximum}; got {value}")]
    WireRate { value: u16, maximum: u16 },

    #[error("routed discovery concurrency must be between 1 and {maximum}; got {value}")]
    Concurrency { value: usize, maximum: usize },

    #[error("routed discovery deadline must be between {minimum:?} and {maximum:?}; got {value:?}")]
    Deadline {
        value: Duration,
        minimum: Duration,
        maximum: Duration,
    },
}

impl DiscoveryClient {
    /// Probe every host in a range that has already passed the routed safety
    /// policy and received user approval.
    pub async fn discover_approved_range(
        &self,
        range: ApprovedIpv4Range,
        scan_config: RoutedScanConfig,
        cancellation: &CancellationToken,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        let targets = ApprovedIpv4Targets::from_range(range);
        self.discover_approved_targets(&targets, scan_config, cancellation)
            .await
    }

    /// Probe a caller-approved, policy-validated set of routed targets.
    ///
    /// This path sends only targeted HDHomeRun UDP discovery datagrams. HTTP
    /// metadata enrichment remains a separate operation over responders in
    /// the returned report, so nonresponders cannot cause TCP or HTTP work.
    pub(crate) async fn discover_approved_targets(
        &self,
        targets: &ApprovedIpv4Targets,
        scan_config: RoutedScanConfig,
        cancellation: &CancellationToken,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        let client = self.clone();
        scan_approved_targets_with(
            targets,
            scan_config,
            self.config().attempts(),
            cancellation,
            move |candidate, task_cancellation| {
                let client = client.clone();
                async move {
                    client
                        .discover_routed_target(candidate, &task_cancellation)
                        .await
                }
            },
        )
        .await
    }
}

async fn scan_approved_targets_with<F, Fut>(
    targets: &ApprovedIpv4Targets,
    scan_config: RoutedScanConfig,
    attempts_per_target: u8,
    cancellation: &CancellationToken,
    probe: F,
) -> Result<DiscoveryReport, DiscoveryError>
where
    F: Fn(Ipv4Addr, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<DiscoveryReport, DiscoveryError>> + Send + 'static,
{
    if cancellation.is_cancelled() {
        return Err(DiscoveryError::Cancelled);
    }
    if targets.is_empty() {
        return Ok(DiscoveryReport::default());
    }

    debug_assert!(
        scan_config.maximum_target_request_datagrams(targets, attempts_per_target)
            <= MAX_ROUTED_CANDIDATES * usize::from(attempts_per_target)
    );

    let overall_deadline = Instant::now() + scan_config.overall_deadline();
    scan_approved_targets_until(
        targets,
        scan_config,
        attempts_per_target,
        cancellation,
        overall_deadline,
        Arc::new(probe),
    )
    .await
}

pub(super) async fn scan_approved_targets_until<F, Fut>(
    targets: &ApprovedIpv4Targets,
    scan_config: RoutedScanConfig,
    attempts_per_target: u8,
    cancellation: &CancellationToken,
    overall_deadline: Instant,
    probe: Arc<F>,
) -> Result<DiscoveryReport, DiscoveryError>
where
    F: Fn(Ipv4Addr, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<DiscoveryReport, DiscoveryError>> + Send + 'static,
{
    let spacing = scan_config.target_start_spacing(attempts_per_target);
    let mut candidates = targets.candidates();
    let mut tasks = JoinSet::new();
    let mut report = DiscoveryReport::default();
    let mut first_target = true;

    loop {
        while tasks.len() < scan_config.max_in_flight() {
            if cancellation.is_cancelled() {
                tasks.abort_all();
                return Err(DiscoveryError::Cancelled);
            }
            if Instant::now() >= overall_deadline {
                tasks.abort_all();
                return Err(routed_deadline_error(scan_config));
            }

            let Some(candidate) = candidates.next() else {
                break;
            };

            if first_target {
                first_target = false;
            } else {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        tasks.abort_all();
                        return Err(DiscoveryError::Cancelled);
                    }
                    () = sleep_until(overall_deadline) => {
                        tasks.abort_all();
                        return Err(routed_deadline_error(scan_config));
                    }
                    () = sleep(spacing) => {}
                }
            }

            let probe = Arc::clone(&probe);
            let task_cancellation = cancellation.clone();
            tasks.spawn(async move {
                let result = probe(candidate, task_cancellation).await;
                (candidate, result)
            });
        }

        if tasks.is_empty() {
            break;
        }

        let joined = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                tasks.abort_all();
                return Err(DiscoveryError::Cancelled);
            }
            () = sleep_until(overall_deadline) => {
                tasks.abort_all();
                return Err(routed_deadline_error(scan_config));
            }
            joined = tasks.join_next() => joined,
        };
        let Some(joined) = joined else {
            break;
        };
        let (candidate, result) =
            joined.map_err(|error| DiscoveryError::Task(error.to_string()))?;
        match result {
            Ok(target_report) => report.merge(target_report),
            Err(DiscoveryError::Cancelled) => {
                tasks.abort_all();
                return Err(DiscoveryError::Cancelled);
            }
            Err(error) => report.issues.push(ProbeIssue {
                endpoint: routed_endpoint(candidate),
                message: error.to_string(),
            }),
        }
    }

    Ok(report)
}

fn routed_deadline_error(scan_config: RoutedScanConfig) -> DiscoveryError {
    DiscoveryError::RoutedScanDeadline {
        deadline: scan_config.overall_deadline(),
    }
}

fn routed_endpoint(candidate: Ipv4Addr) -> ProbeEndpoint {
    ProbeEndpoint {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        destination: SocketAddr::new(IpAddr::V4(candidate), DISCOVERY_UDP_PORT),
        method: DiscoveryMethod::RoutedTargeted,
        interface: None,
        accepted_source_network: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn network(value: &str) -> Ipv4Net {
        value.parse().expect("valid test network")
    }

    #[test]
    fn accepts_one_private_slash_24() {
        let range = ApprovedIpv4Range::new(network("10.42.7.0/24")).expect("safe range");
        let candidates = range.candidates().collect::<Vec<_>>();

        assert_eq!(candidates.len(), 254);
        assert_eq!(candidates[0], Ipv4Addr::new(10, 42, 7, 1));
        assert_eq!(candidates[253], Ipv4Addr::new(10, 42, 7, 254));
    }

    #[test]
    fn accepts_small_private_ranges() {
        let point_to_point = ApprovedIpv4Range::new(network("172.16.8.4/31")).expect("safe /31");
        assert_eq!(point_to_point.candidates().count(), 2);

        let one_host = ApprovedIpv4Range::new(network("192.168.3.9/32")).expect("safe /32");
        assert_eq!(
            one_host.candidates().collect::<Vec<_>>(),
            vec![Ipv4Addr::new(192, 168, 3, 9)]
        );
    }

    #[test]
    fn approved_target_list_is_private_deduplicated_and_deterministic() {
        let targets = ApprovedIpv4Targets::new([
            Ipv4Addr::new(192, 168, 8, 3),
            Ipv4Addr::new(10, 0, 0, 2),
            Ipv4Addr::new(192, 168, 8, 3),
            Ipv4Addr::new(10, 0, 0, 1),
        ])
        .unwrap();

        assert_eq!(targets.len(), 3);
        assert!(!targets.is_empty());
        assert_eq!(
            targets.candidates().collect::<Vec<_>>(),
            vec![
                Ipv4Addr::new(10, 0, 0, 1),
                Ipv4Addr::new(10, 0, 0, 2),
                Ipv4Addr::new(192, 168, 8, 3),
            ]
        );
    }

    #[test]
    fn approved_target_list_rejects_non_private_and_more_than_hard_cap() {
        for address in [
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(169, 254, 1, 1),
        ] {
            assert_eq!(
                ApprovedIpv4Targets::new([address]),
                Err(RoutedTargetsError::NotPrivate(address))
            );
        }

        let too_many = (0_u16..=u16::try_from(MAX_ROUTED_CANDIDATES).unwrap())
            .map(|offset| Ipv4Addr::new(10, 0, (offset >> 8) as u8, offset as u8));
        assert_eq!(
            ApprovedIpv4Targets::new(too_many),
            Err(RoutedTargetsError::TooManyCandidates {
                count: MAX_ROUTED_CANDIDATES + 1,
                maximum: MAX_ROUTED_CANDIDATES
            })
        );
    }

    #[test]
    fn rejects_ranges_wider_than_slash_24() {
        let candidate = network("10.42.0.0/23");

        assert_eq!(
            ApprovedIpv4Range::new(candidate),
            Err(RoutedRangeError::TooWide {
                network: candidate,
                maximum_prefix: 24,
            })
        );
    }

    #[test]
    fn rejects_public_and_special_ranges() {
        for value in ["192.0.2.0/24", "127.0.0.0/24", "169.254.9.0/24"] {
            let candidate = network(value);
            assert_eq!(
                ApprovedIpv4Range::new(candidate),
                Err(RoutedRangeError::NotPrivate(candidate))
            );
        }
    }

    #[test]
    fn default_scan_budget_is_508_small_datagrams_for_a_slash_24() {
        let range = ApprovedIpv4Range::new(network("10.42.7.0/24")).unwrap();
        let config = RoutedScanConfig::default();

        assert_eq!(config.wire_datagrams_per_second(), 64);
        assert_eq!(config.max_in_flight(), 16);
        assert_eq!(config.overall_deadline(), Duration::from_secs(15));
        assert_eq!(config.maximum_request_datagrams(range, 2), 508);
        let targets = ApprovedIpv4Targets::new([
            Ipv4Addr::new(10, 42, 7, 1),
            Ipv4Addr::new(10, 42, 7, 1),
            Ipv4Addr::new(10, 42, 7, 2),
        ])
        .unwrap();
        assert_eq!(config.maximum_target_request_datagrams(&targets, 2), 4);
        assert_eq!(
            config.target_start_spacing(2),
            Duration::from_micros(31_250)
        );
    }

    #[test]
    fn rejects_excessive_scan_rate_and_concurrency() {
        assert_eq!(
            RoutedScanConfig::new(65, 16),
            Err(InvalidRoutedScanConfig::WireRate {
                value: 65,
                maximum: 64,
            })
        );
        assert_eq!(
            RoutedScanConfig::new(64, 17),
            Err(InvalidRoutedScanConfig::Concurrency {
                value: 17,
                maximum: 16,
            })
        );
        for deadline in [Duration::ZERO, Duration::from_secs(31)] {
            assert_eq!(
                RoutedScanConfig::new_with_overall_deadline(64, 16, deadline),
                Err(InvalidRoutedScanConfig::Deadline {
                    value: deadline,
                    minimum: MIN_ROUTED_SCAN_DEADLINE,
                    maximum: MAX_ROUTED_SCAN_DEADLINE,
                })
            );
        }
    }

    #[tokio::test]
    async fn cancelled_range_scan_sends_nothing() {
        let range = ApprovedIpv4Range::new(network("10.42.7.9/32")).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = DiscoveryClient::default()
            .discover_approved_range(range, RoutedScanConfig::default(), &cancellation)
            .await
            .expect_err("pre-cancelled scan");

        assert!(matches!(error, DiscoveryError::Cancelled));
    }

    #[tokio::test]
    async fn cancelled_target_scan_starts_no_probe() {
        let targets = ApprovedIpv4Targets::new([Ipv4Addr::new(10, 42, 7, 9)]).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let probes = Arc::new(AtomicUsize::new(0));
        let probe_counter = Arc::clone(&probes);

        let error = scan_approved_targets_with(
            &targets,
            RoutedScanConfig::default(),
            2,
            &cancellation,
            move |_, _| {
                probe_counter.fetch_add(1, Ordering::SeqCst);
                async { Ok(DiscoveryReport::default()) }
            },
        )
        .await
        .expect_err("pre-cancelled target scan");

        assert!(matches!(error, DiscoveryError::Cancelled));
        assert_eq!(probes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_overall_deadline_starts_no_probe() {
        let targets = ApprovedIpv4Targets::new([Ipv4Addr::new(10, 42, 7, 9)]).unwrap();
        let cancellation = CancellationToken::new();
        let probes = Arc::new(AtomicUsize::new(0));
        let probe_counter = Arc::clone(&probes);
        let probe = Arc::new(move |_, _| {
            probe_counter.fetch_add(1, Ordering::SeqCst);
            async { Ok(DiscoveryReport::default()) }
        });

        let error = scan_approved_targets_until(
            &targets,
            RoutedScanConfig::default(),
            2,
            &cancellation,
            Instant::now(),
            probe,
        )
        .await
        .expect_err("expired routed deadline");

        assert!(matches!(
            error,
            DiscoveryError::RoutedScanDeadline {
                deadline: DEFAULT_ROUTED_SCAN_DEADLINE
            }
        ));
        assert_eq!(probes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn overall_deadline_aborts_an_in_flight_probe() {
        let targets = ApprovedIpv4Targets::new([Ipv4Addr::new(10, 42, 7, 9)]).unwrap();
        let scan_config =
            RoutedScanConfig::new_with_overall_deadline(64, 1, MIN_ROUTED_SCAN_DEADLINE).unwrap();

        let error = scan_approved_targets_with(
            &targets,
            scan_config,
            1,
            &CancellationToken::new(),
            |_, _| std::future::pending(),
        )
        .await
        .expect_err("pending probe must be bounded by the scan deadline");

        assert!(matches!(
            error,
            DiscoveryError::RoutedScanDeadline {
                deadline: MIN_ROUTED_SCAN_DEADLINE
            }
        ));
    }

    #[test]
    fn routed_endpoint_retains_targeted_route_provenance() {
        let endpoint = routed_endpoint(Ipv4Addr::new(10, 42, 7, 9));

        assert_eq!(endpoint.method, DiscoveryMethod::RoutedTargeted);
        assert!(endpoint.method.is_targeted());
        assert_eq!(
            endpoint.destination,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 7, 9)), DISCOVERY_UDP_PORT)
        );
    }
}
