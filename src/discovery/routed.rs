use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use ipnet::Ipv4Net;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::client::{DiscoveryClient, DiscoveryError, DiscoveryReport, ProbeIssue};
use super::types::{DiscoveryMethod, ProbeEndpoint};
use crate::hdhr::protocol::DISCOVERY_UDP_PORT;

/// Hard ceiling for an explicitly approved routed discovery range.
pub const MAX_ROUTED_CANDIDATES: usize = 256;
/// Hard ceiling for the nominal routed discovery packet rate.
pub const MAX_ROUTED_WIRE_DATAGRAMS_PER_SECOND: u16 = 64;
/// Hard ceiling for simultaneous targeted probes.
pub const MAX_ROUTED_CONCURRENCY: usize = 16;

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
}

impl RoutedScanConfig {
    pub fn new(
        wire_datagrams_per_second: u16,
        max_in_flight: usize,
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

        Ok(Self {
            wire_datagrams_per_second,
            max_in_flight,
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

    /// Conservative number of request datagrams for a complete range scan.
    #[must_use]
    pub fn maximum_request_datagrams(
        self,
        range: ApprovedIpv4Range,
        attempts_per_target: u8,
    ) -> usize {
        range.candidates().count() * usize::from(attempts_per_target)
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
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidRoutedScanConfig {
    #[error("routed discovery wire rate must be between 1 and {maximum}; got {value}")]
    WireRate { value: u16, maximum: u16 },

    #[error("routed discovery concurrency must be between 1 and {maximum}; got {value}")]
    Concurrency { value: usize, maximum: usize },
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
        if cancellation.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }

        let spacing = scan_config.target_start_spacing(self.config().attempts());
        let mut candidates = range.candidates();
        let mut tasks = JoinSet::new();
        let mut report = DiscoveryReport::default();
        let mut first_target = true;

        loop {
            while tasks.len() < scan_config.max_in_flight() {
                let Some(candidate) = candidates.next() else {
                    break;
                };

                if first_target {
                    first_target = false;
                } else {
                    tokio::select! {
                        () = cancellation.cancelled() => {
                            tasks.abort_all();
                            return Err(DiscoveryError::Cancelled);
                        }
                        () = sleep(spacing) => {}
                    }
                }

                let client = self.clone();
                let task_cancellation = cancellation.clone();
                tasks.spawn(async move {
                    let target = SocketAddr::new(IpAddr::V4(candidate), 0);
                    let result = client
                        .discover_target(target, None, &task_cancellation)
                        .await;
                    (candidate, result)
                });
            }

            if tasks.is_empty() {
                break;
            }

            let joined = tokio::select! {
                () = cancellation.cancelled() => {
                    tasks.abort_all();
                    return Err(DiscoveryError::Cancelled);
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
                Err(DiscoveryError::Cancelled) => return Err(DiscoveryError::Cancelled),
                Err(error) => report.issues.push(ProbeIssue {
                    endpoint: routed_endpoint(candidate),
                    message: error.to_string(),
                }),
            }
        }

        Ok(report)
    }
}

fn routed_endpoint(candidate: Ipv4Addr) -> ProbeEndpoint {
    ProbeEndpoint {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        destination: SocketAddr::new(IpAddr::V4(candidate), DISCOVERY_UDP_PORT),
        method: DiscoveryMethod::Targeted,
        interface: None,
        accepted_source_network: None,
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(config.maximum_request_datagrams(range, 2), 508);
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
}
