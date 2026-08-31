use std::collections::{BTreeMap, btree_map::Entry};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use super::{DiscoveryMethod, ProbeEndpoint, local_probe_endpoints};
use crate::domain::DeviceId;
use crate::hdhr::protocol::{
    DISCOVERY_UDP_PORT, MAX_PACKET_SIZE, ProtocolError, encode_tuner_discover_request,
    parse_tuner_discover_response,
};

const MIN_RESPONSE_WINDOW: Duration = Duration::from_millis(10);
const MAX_RESPONSE_WINDOW: Duration = Duration::from_secs(2);
const MAX_ATTEMPTS: u8 = 4;
const MAX_RECEIVED_DATAGRAMS: usize = 1_024;
const MAX_UNIQUE_DEVICES: usize = 256;

/// Bounded settings for one socket-level discovery probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeConfig {
    attempts: u8,
    response_window: Duration,
    max_received_datagrams: usize,
    max_unique_devices: usize,
}

impl ProbeConfig {
    pub fn new(
        attempts: u8,
        response_window: Duration,
        max_received_datagrams: usize,
        max_unique_devices: usize,
    ) -> Result<Self, InvalidProbeConfig> {
        if attempts == 0 || attempts > MAX_ATTEMPTS {
            return Err(InvalidProbeConfig::Attempts {
                value: attempts,
                maximum: MAX_ATTEMPTS,
            });
        }
        if !(MIN_RESPONSE_WINDOW..=MAX_RESPONSE_WINDOW).contains(&response_window) {
            return Err(InvalidProbeConfig::ResponseWindow {
                value: response_window,
                minimum: MIN_RESPONSE_WINDOW,
                maximum: MAX_RESPONSE_WINDOW,
            });
        }
        if max_received_datagrams == 0 || max_received_datagrams > MAX_RECEIVED_DATAGRAMS {
            return Err(InvalidProbeConfig::ReceivedDatagrams {
                value: max_received_datagrams,
                maximum: MAX_RECEIVED_DATAGRAMS,
            });
        }
        if max_unique_devices == 0 || max_unique_devices > MAX_UNIQUE_DEVICES {
            return Err(InvalidProbeConfig::UniqueDevices {
                value: max_unique_devices,
                maximum: MAX_UNIQUE_DEVICES,
            });
        }

        Ok(Self {
            attempts,
            response_window,
            max_received_datagrams,
            max_unique_devices,
        })
    }

    #[must_use]
    pub const fn attempts(self) -> u8 {
        self.attempts
    }

    #[must_use]
    pub const fn response_window(self) -> Duration {
        self.response_window
    }

    #[must_use]
    pub const fn max_received_datagrams(self) -> usize {
        self.max_received_datagrams
    }

    #[must_use]
    pub const fn max_unique_devices(self) -> usize {
        self.max_unique_devices
    }
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            attempts: 2,
            response_window: Duration::from_millis(200),
            max_received_datagrams: 256,
            max_unique_devices: 64,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidProbeConfig {
    #[error("discovery attempts must be between 1 and {maximum}; got {value}")]
    Attempts { value: u8, maximum: u8 },

    #[error("discovery response window must be between {minimum:?} and {maximum:?}; got {value:?}")]
    ResponseWindow {
        value: Duration,
        minimum: Duration,
        maximum: Duration,
    },

    #[error("maximum received datagrams must be between 1 and {maximum}; got {value}")]
    ReceivedDatagrams { value: usize, maximum: usize },

    #[error("maximum unique devices must be between 1 and {maximum}; got {value}")]
    UniqueDevices { value: usize, maximum: usize },
}

/// A validated tuner observation tied to the UDP endpoint that supplied it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryObservation {
    pub device_id: DeviceId,
    pub source: SocketAddr,
    pub method: DiscoveryMethod,
    pub interface: Option<String>,
    pub device_types: Vec<u32>,
    pub tuner_count: Option<u8>,
    /// Untrusted metadata. HTTP code must validate it against the source.
    pub advertised_base_url: Option<String>,
    /// Untrusted metadata. HTTP code must validate it against the source.
    pub advertised_lineup_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryStats {
    pub probes_started: usize,
    pub datagrams_sent: usize,
    pub datagrams_received: usize,
    pub datagrams_accepted: usize,
    pub datagrams_rejected: usize,
    pub duplicate_observations: usize,
    pub receive_limit_reached: bool,
    pub device_limit_reached: bool,
}

impl DiscoveryStats {
    fn merge(&mut self, other: Self) {
        self.probes_started += other.probes_started;
        self.datagrams_sent += other.datagrams_sent;
        self.datagrams_received += other.datagrams_received;
        self.datagrams_accepted += other.datagrams_accepted;
        self.datagrams_rejected += other.datagrams_rejected;
        self.duplicate_observations += other.duplicate_observations;
        self.receive_limit_reached |= other.receive_limit_reached;
        self.device_limit_reached |= other.device_limit_reached;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeIssue {
    pub endpoint: ProbeEndpoint,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryReport {
    pub observations: Vec<DiscoveryObservation>,
    pub stats: DiscoveryStats,
    pub issues: Vec<ProbeIssue>,
}

impl DiscoveryReport {
    pub(crate) fn merge(&mut self, other: Self) {
        let mut observations = self
            .observations
            .drain(..)
            .map(|observation| ((observation.device_id, observation.source), observation))
            .collect::<BTreeMap<_, _>>();

        for observation in other.observations {
            match observations.entry((observation.device_id, observation.source)) {
                Entry::Vacant(entry) => {
                    entry.insert(observation);
                }
                Entry::Occupied(_) => {
                    self.stats.duplicate_observations += 1;
                }
            }
        }

        self.observations = observations.into_values().collect();
        self.stats.merge(other.stats);
        self.issues.extend(other.issues);
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryClient {
    config: ProbeConfig,
}

impl DiscoveryClient {
    #[must_use]
    pub const fn new(config: ProbeConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub const fn config(&self) -> ProbeConfig {
        self.config
    }

    /// Probe one known IPv4 or IPv6 address. The destination port is always
    /// normalized to the HDHomeRun discovery port.
    pub async fn discover_target(
        &self,
        target: SocketAddr,
        expected_device: Option<DeviceId>,
        cancellation: &CancellationToken,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        let destination = with_port(target, DISCOVERY_UDP_PORT);
        if invalid_target(destination) {
            return Err(DiscoveryError::InvalidEndpoint {
                endpoint: destination,
                reason: "targeted discovery requires a unicast address",
            });
        }

        let bind = match destination {
            SocketAddr::V4(_) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0)),
        };
        let endpoint = ProbeEndpoint {
            bind,
            destination,
            method: DiscoveryMethod::Targeted,
            interface: None,
        };

        self.probe_endpoint(endpoint, expected_device, cancellation)
            .await
    }

    /// Run ordinary discovery concurrently on every eligible local interface.
    pub async fn discover_local(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        let endpoints = local_probe_endpoints().map_err(DiscoveryError::Interfaces)?;
        let mut tasks = JoinSet::new();

        for endpoint in endpoints {
            let client = self.clone();
            let task_endpoint = endpoint.clone();
            let task_cancellation = cancellation.clone();
            tasks.spawn(async move {
                let result = client
                    .probe_endpoint(task_endpoint, None, &task_cancellation)
                    .await;
                (endpoint, result)
            });
        }

        let mut report = DiscoveryReport::default();
        loop {
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
            let (endpoint, result) =
                joined.map_err(|error| DiscoveryError::Task(error.to_string()))?;
            match result {
                Ok(probe_report) => report.merge(probe_report),
                Err(DiscoveryError::Cancelled) => return Err(DiscoveryError::Cancelled),
                Err(error) => report.issues.push(ProbeIssue {
                    endpoint,
                    message: error.to_string(),
                }),
            }
        }

        Ok(report)
    }

    async fn probe_endpoint(
        &self,
        endpoint: ProbeEndpoint,
        expected_device: Option<DeviceId>,
        cancellation: &CancellationToken,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        validate_endpoint(&endpoint)?;
        if cancellation.is_cancelled() {
            return Err(DiscoveryError::Cancelled);
        }

        let request = encode_tuner_discover_request(expected_device.map(DeviceId::get))?;
        let socket = UdpSocket::bind(endpoint.bind)
            .await
            .map_err(|source| DiscoveryError::Io {
                operation: "bind discovery socket",
                endpoint: endpoint.bind,
                source,
            })?;
        if endpoint.method == DiscoveryMethod::Ipv4Broadcast {
            socket
                .set_broadcast(true)
                .map_err(|source| DiscoveryError::Io {
                    operation: "enable IPv4 broadcast",
                    endpoint: endpoint.bind,
                    source,
                })?;
        }

        let mut report = DiscoveryReport::default();
        report.stats.probes_started = 1;
        let mut observations = BTreeMap::new();
        let mut receive_buffer = [0_u8; MAX_PACKET_SIZE + 1];

        'attempts: for _ in 0..self.config.attempts {
            let sent = tokio::select! {
                () = cancellation.cancelled() => return Err(DiscoveryError::Cancelled),
                result = socket.send_to(&request, endpoint.destination) => {
                    result.map_err(|source| DiscoveryError::Io {
                        operation: "send discovery request",
                        endpoint: endpoint.destination,
                        source,
                    })?
                }
            };
            if sent != request.len() {
                return Err(DiscoveryError::ShortSend {
                    endpoint: endpoint.destination,
                    expected: request.len(),
                    actual: sent,
                });
            }
            report.stats.datagrams_sent += 1;

            let deadline = Instant::now() + self.config.response_window;
            loop {
                if report.stats.datagrams_received >= self.config.max_received_datagrams {
                    report.stats.receive_limit_reached = true;
                    break 'attempts;
                }

                let received = tokio::select! {
                    () = cancellation.cancelled() => return Err(DiscoveryError::Cancelled),
                    result = timeout_at(deadline, socket.recv_from(&mut receive_buffer)) => result,
                };
                let (length, source) = match received {
                    Ok(Ok(received)) => received,
                    Ok(Err(source)) => {
                        return Err(DiscoveryError::Io {
                            operation: "receive discovery response",
                            endpoint: endpoint.bind,
                            source,
                        });
                    }
                    Err(_) => break,
                };
                report.stats.datagrams_received += 1;

                if !source_matches(&endpoint, source) {
                    report.stats.datagrams_rejected += 1;
                    continue;
                }

                let response = match parse_tuner_discover_response(&receive_buffer[..length]) {
                    Ok(response) => response,
                    Err(_) => {
                        report.stats.datagrams_rejected += 1;
                        continue;
                    }
                };
                let device_id = match DeviceId::new(response.device_id) {
                    Ok(device_id) => device_id,
                    Err(_) => {
                        report.stats.datagrams_rejected += 1;
                        continue;
                    }
                };
                if expected_device.is_some_and(|expected| expected != device_id) {
                    report.stats.datagrams_rejected += 1;
                    continue;
                }

                report.stats.datagrams_accepted += 1;
                let key = (device_id, source);
                if observations.contains_key(&key) {
                    report.stats.duplicate_observations += 1;
                    continue;
                }
                if observations.len() >= self.config.max_unique_devices {
                    report.stats.device_limit_reached = true;
                    break 'attempts;
                }

                observations.insert(
                    key,
                    DiscoveryObservation {
                        device_id,
                        source,
                        method: endpoint.method,
                        interface: endpoint.interface.clone(),
                        device_types: response.device_types,
                        tuner_count: response.tuner_count,
                        advertised_base_url: response.base_url,
                        advertised_lineup_url: response.lineup_url,
                    },
                );
            }
        }

        report.observations = observations.into_values().collect();
        Ok(report)
    }
}

impl Default for DiscoveryClient {
    fn default() -> Self {
        Self::new(ProbeConfig::default())
    }
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("failed to enumerate local network interfaces: {0}")]
    Interfaces(#[source] io::Error),

    #[error("invalid discovery endpoint {endpoint}: {reason}")]
    InvalidEndpoint {
        endpoint: SocketAddr,
        reason: &'static str,
    },

    #[error("failed to {operation} for {endpoint}: {source}")]
    Io {
        operation: &'static str,
        endpoint: SocketAddr,
        #[source]
        source: io::Error,
    },

    #[error("short UDP discovery send to {endpoint}: wrote {actual} bytes, expected {expected}")]
    ShortSend {
        endpoint: SocketAddr,
        expected: usize,
        actual: usize,
    },

    #[error("discovery task failed: {0}")]
    Task(String),

    #[error("discovery was cancelled")]
    Cancelled,

    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

fn validate_endpoint(endpoint: &ProbeEndpoint) -> Result<(), DiscoveryError> {
    if endpoint.bind.is_ipv4() != endpoint.destination.is_ipv4() {
        return Err(DiscoveryError::InvalidEndpoint {
            endpoint: endpoint.destination,
            reason: "bind address and destination use different address families",
        });
    }
    if endpoint.destination.port() == 0 {
        return Err(DiscoveryError::InvalidEndpoint {
            endpoint: endpoint.destination,
            reason: "destination port is zero",
        });
    }

    Ok(())
}

fn source_matches(endpoint: &ProbeEndpoint, source: SocketAddr) -> bool {
    if source.port() != endpoint.destination.port()
        || source.ip().is_unspecified()
        || source.ip().is_multicast()
        || source.is_ipv4() != endpoint.destination.is_ipv4()
    {
        return false;
    }

    endpoint.method != DiscoveryMethod::Targeted || source.ip() == endpoint.destination.ip()
}

fn invalid_target(destination: SocketAddr) -> bool {
    if destination.ip().is_unspecified() || destination.ip().is_multicast() {
        return true;
    }

    match destination {
        SocketAddr::V4(address) => *address.ip() == Ipv4Addr::BROADCAST,
        SocketAddr::V6(address) => address.ip().is_unicast_link_local() && address.scope_id() == 0,
    }
}

fn with_port(address: SocketAddr, port: u16) -> SocketAddr {
    match address {
        SocketAddr::V4(address) => SocketAddr::V4(SocketAddrV4::new(*address.ip(), port)),
        SocketAddr::V6(address) => SocketAddr::V6(SocketAddrV6::new(
            *address.ip(),
            port,
            address.flowinfo(),
            address.scope_id(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_ALL_TUNERS_REQUEST: [u8; 14] = [
        0x00, 0x02, 0x00, 0x06, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x39, 0x30, 0x77, 0xE7,
    ];
    const GOLDEN_TUNER_RESPONSE: [u8; 79] = [
        0x00, 0x03, 0x00, 0x47, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x04, 0x10, 0x5A, 0x12,
        0x32, 0x10, 0x01, 0x04, 0x2A, 0x14, 0x68, 0x74, 0x74, 0x70, 0x3A, 0x2F, 0x2F, 0x31, 0x39,
        0x32, 0x2E, 0x30, 0x2E, 0x32, 0x2E, 0x31, 0x30, 0x3A, 0x38, 0x30, 0x27, 0x20, 0x68, 0x74,
        0x74, 0x70, 0x3A, 0x2F, 0x2F, 0x31, 0x39, 0x32, 0x2E, 0x30, 0x2E, 0x32, 0x2E, 0x31, 0x30,
        0x3A, 0x38, 0x30, 0x2F, 0x6C, 0x69, 0x6E, 0x65, 0x75, 0x70, 0x2E, 0x6A, 0x73, 0x6F, 0x6E,
        0x1E, 0x72, 0x20, 0x00,
    ];

    #[test]
    fn default_budget_matches_two_official_windows() {
        let config = ProbeConfig::default();

        assert_eq!(config.attempts(), 2);
        assert_eq!(config.response_window(), Duration::from_millis(200));
        assert_eq!(config.max_received_datagrams(), 256);
        assert_eq!(config.max_unique_devices(), 64);
    }

    #[test]
    fn rejects_unbounded_probe_settings() {
        assert!(matches!(
            ProbeConfig::new(5, Duration::from_millis(200), 256, 64),
            Err(InvalidProbeConfig::Attempts { .. })
        ));
        assert!(matches!(
            ProbeConfig::new(2, Duration::from_secs(3), 256, 64),
            Err(InvalidProbeConfig::ResponseWindow { .. })
        ));
        assert!(matches!(
            ProbeConfig::new(2, Duration::from_millis(200), 2_000, 64),
            Err(InvalidProbeConfig::ReceivedDatagrams { .. })
        ));
        assert!(matches!(
            ProbeConfig::new(2, Duration::from_millis(200), 256, 512),
            Err(InvalidProbeConfig::UniqueDevices { .. })
        ));
    }

    #[test]
    fn targeted_source_must_match_address_and_port() {
        let endpoint = ProbeEndpoint {
            bind: "0.0.0.0:0".parse().unwrap(),
            destination: "192.0.2.10:65001".parse().unwrap(),
            method: DiscoveryMethod::Targeted,
            interface: None,
        };

        assert!(source_matches(
            &endpoint,
            "192.0.2.10:65001".parse().unwrap()
        ));
        assert!(!source_matches(
            &endpoint,
            "192.0.2.11:65001".parse().unwrap()
        ));
        assert!(!source_matches(
            &endpoint,
            "192.0.2.10:65000".parse().unwrap()
        ));
    }

    #[test]
    fn broadcast_source_can_be_any_unicast_in_family() {
        let endpoint = ProbeEndpoint {
            bind: "192.0.2.20:0".parse().unwrap(),
            destination: "192.0.2.255:65001".parse().unwrap(),
            method: DiscoveryMethod::Ipv4Broadcast,
            interface: Some("test0".to_owned()),
        };

        assert!(source_matches(
            &endpoint,
            "192.0.2.10:65001".parse().unwrap()
        ));
        assert!(!source_matches(
            &endpoint,
            "[2001:db8::10]:65001".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn pre_cancelled_probe_does_no_network_work() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = DiscoveryClient::default()
            .discover_target("127.0.0.1:65001".parse().unwrap(), None, &cancellation)
            .await
            .expect_err("cancelled");

        assert!(matches!(error, DiscoveryError::Cancelled));
    }

    #[tokio::test]
    async fn targeted_probe_rejects_malformed_datagram_then_accepts_tuner() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_address = server.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let mut request = [0_u8; 64];
            let (length, client) = server.recv_from(&mut request).await.unwrap();
            assert_eq!(&request[..length], &GOLDEN_ALL_TUNERS_REQUEST);

            server.send_to(&[0_u8; 8], client).await.unwrap();
            server
                .send_to(&GOLDEN_TUNER_RESPONSE, client)
                .await
                .unwrap();
        });

        let config = ProbeConfig::new(1, Duration::from_millis(100), 8, 4).unwrap();
        let endpoint = ProbeEndpoint {
            bind: "127.0.0.1:0".parse().unwrap(),
            destination: server_address,
            method: DiscoveryMethod::Targeted,
            interface: None,
        };
        let report = DiscoveryClient::new(config)
            .probe_endpoint(endpoint, None, &CancellationToken::new())
            .await
            .unwrap();
        server_task.await.unwrap();

        assert_eq!(report.observations.len(), 1);
        let observation = &report.observations[0];
        assert_eq!(observation.device_id, DeviceId::new(0x105A_1232).unwrap());
        assert_eq!(observation.source, server_address);
        assert_eq!(observation.tuner_count, Some(4));
        assert_eq!(
            observation.advertised_base_url.as_deref(),
            Some("http://192.0.2.10:80")
        );
        assert_eq!(
            observation.advertised_lineup_url.as_deref(),
            Some("http://192.0.2.10:80/lineup.json")
        );
        assert_eq!(report.stats.datagrams_sent, 1);
        assert_eq!(report.stats.datagrams_received, 2);
        assert_eq!(report.stats.datagrams_accepted, 1);
        assert_eq!(report.stats.datagrams_rejected, 1);
    }

    #[tokio::test]
    async fn rejects_broadcast_and_unscoped_link_local_targets() {
        let cancellation = CancellationToken::new();
        let client = DiscoveryClient::default();

        let broadcast = client
            .discover_target("255.255.255.255:0".parse().unwrap(), None, &cancellation)
            .await
            .expect_err("broadcast is not a targeted endpoint");
        assert!(matches!(broadcast, DiscoveryError::InvalidEndpoint { .. }));

        let link_local = client
            .discover_target("[fe80::1]:0".parse().unwrap(), None, &cancellation)
            .await
            .expect_err("link-local IPv6 needs a scope");
        assert!(matches!(link_local, DiscoveryError::InvalidEndpoint { .. }));
    }
}
