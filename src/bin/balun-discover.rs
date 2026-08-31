use std::env;
use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use balun::discovery::{
    ApprovedIpv4Range, DeviceRegistry, DiscoveryClient, DiscoveryReport, RegistryError,
    RegistryInstant, RoutedRangeError, RoutedScanConfig,
};
use balun::domain::DeviceId;
use balun::hdhr::{
    DeviceEndpoint, DeviceHttpClient, DeviceHttpError, DeviceSnapshotError, LineupFetchError,
};
use ipnet::Ipv4Net;
use thiserror::Error;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const INSPECTION_REPORT_DEADLINE: Duration = Duration::from_secs(60);

const USAGE: &str = "\
Usage:
  balun-discover
  balun-discover [--inspect] --local
  balun-discover [--inspect] --target <IP> [--target <IP> ...]
  balun-discover [--inspect] --approved-range <PRIVATE-CIDR>

No arguments performs ordinary local-interface discovery.
--inspect also fetches bounded device metadata and lineup counts; it never
starts a stream or allocates a tuner.
Routed enumeration requires the explicit --approved-range option and is
limited by Balun's private-/24 and packet-rate safety policy.";

#[derive(Clone, Copy, Debug)]
enum Action {
    Local,
    Target(SocketAddr),
    ApprovedRange(ApprovedIpv4Range),
}

#[derive(Debug)]
struct Cli {
    actions: Vec<Action>,
    inspect: bool,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("{0}\n\n{USAGE}")]
    Usage(String),

    #[error("invalid targeted address {value:?}: {message}")]
    Target { value: String, message: String },

    #[error("invalid routed range {value:?}: {source}")]
    Range {
        value: String,
        #[source]
        source: RoutedRangeError,
    },

    #[error("invalid routed range {value:?}: {message}")]
    RangeSyntax { value: String, message: String },

    #[error("could not build the device inspection registry: {0}")]
    InspectionRegistry(#[from] RegistryError),

    #[error("device inspection was cancelled")]
    InspectionCancelled,

    #[error("device inspection exceeded its {deadline:?} report deadline")]
    InspectionDeadline { deadline: Duration },

    #[error("inspection failed for {failed} of {attempted} discovered devices")]
    InspectionFailed { failed: usize, attempted: usize },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InspectionOutcome {
    attempted_devices: usize,
    failed_devices: usize,
}

impl InspectionOutcome {
    fn merge(&mut self, other: Self) {
        self.attempted_devices += other.attempted_devices;
        self.failed_devices += other.failed_devices;
    }

    fn require_success(self) -> Result<(), CliError> {
        if self.failed_devices == 0 {
            return Ok(());
        }
        Err(CliError::InspectionFailed {
            failed: self.failed_devices,
            attempted: self.attempted_devices,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InspectionDetails {
    friendly_name: String,
    model_number: String,
    firmware_version: String,
    tuner_count: String,
    channel_count: usize,
    favorite_count: usize,
    drm_count: usize,
}

#[derive(Debug)]
enum InspectionAttemptError {
    Cancelled,
    Failed(String),
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

        Ok(InspectionDetails {
            friendly_name: info.friendly_name().unwrap_or("-").to_owned(),
            model_number: info.model_number().unwrap_or("-").to_owned(),
            firmware_version: info.firmware_version().unwrap_or("-").to_owned(),
            tuner_count: info
                .tuner_count()
                .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(cli) = parse_cli(env::args().skip(1))? else {
        println!("{USAGE}");
        return Ok(());
    };

    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });

    let client = DiscoveryClient::default();
    let http_client = DeviceHttpClient::default();
    let inspect = cli.inspect;
    let mut inspection = InspectionOutcome::default();
    for action in cli.actions {
        let report = match action {
            Action::Local => client.discover_local(&cancellation).await?,
            Action::Target(target) => client.discover_target(target, None, &cancellation).await?,
            Action::ApprovedRange(range) => {
                let scan = RoutedScanConfig::default();
                eprintln!(
                    "approved routed scan: {} candidates, at most {} request datagrams, {} datagrams/s",
                    range.candidates().count(),
                    scan.maximum_request_datagrams(range, client.config().attempts()),
                    scan.wire_datagrams_per_second()
                );
                client
                    .discover_approved_range(range, scan, &cancellation)
                    .await?
            }
        };
        print_report(&report);
        if inspect {
            inspection.merge(
                inspect_report_with_deadline(
                    &http_client,
                    &report,
                    &cancellation,
                    INSPECTION_REPORT_DEADLINE,
                )
                .await?,
            );
        }
    }

    if inspect {
        inspection.require_success()?;
    }

    Ok(())
}

fn parse_cli(arguments: impl Iterator<Item = String>) -> Result<Option<Cli>, CliError> {
    let mut arguments = arguments.peekable();
    if arguments.peek().is_none() {
        return Ok(Some(Cli {
            actions: vec![Action::Local],
            inspect: false,
        }));
    }

    let mut actions = Vec::new();
    let mut inspect = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "--inspect" => inspect = true,
            "--local" => actions.push(Action::Local),
            "--target" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--target requires an IP address".to_owned()))?;
                actions.push(Action::Target(parse_target(&value)?));
            }
            "--approved-range" => {
                let value = arguments.next().ok_or_else(|| {
                    CliError::Usage("--approved-range requires a private IPv4 CIDR".to_owned())
                })?;
                let network = value
                    .parse::<Ipv4Net>()
                    .map_err(|error| CliError::RangeSyntax {
                        value: value.clone(),
                        message: error.to_string(),
                    })?;
                let range = ApprovedIpv4Range::new(network)
                    .map_err(|source| CliError::Range { value, source })?;
                actions.push(Action::ApprovedRange(range));
            }
            _ => return Err(CliError::Usage(format!("unknown option {argument:?}"))),
        }
    }

    if actions.is_empty() {
        actions.push(Action::Local);
    }

    Ok(Some(Cli { actions, inspect }))
}

fn parse_target(value: &str) -> Result<SocketAddr, CliError> {
    if let Ok(address) = value.parse::<SocketAddr>() {
        return Ok(address);
    }
    value
        .parse::<IpAddr>()
        .map(|address| SocketAddr::new(address, 0))
        .map_err(|error| CliError::Target {
            value: value.to_owned(),
            message: error.to_string(),
        })
}

fn print_report(report: &DiscoveryReport) {
    for observation in &report.observations {
        println!(
            "{} source={} method={:?} interface={} tuners={}",
            observation.device_id,
            observation.source,
            observation.method,
            observation.interface.as_deref().unwrap_or("-"),
            observation
                .tuner_count
                .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
        );
        if let Some(url) = &observation.advertised_base_url {
            println!("  advertised_base_url={}", advertised_url_summary(url));
        }
        if let Some(url) = &observation.advertised_lineup_url {
            println!("  advertised_lineup_url={}", advertised_url_summary(url));
        }
    }
    if report.observations.is_empty() {
        println!("no HDHomeRun tuners found");
    }

    println!(
        "probes={} sent={} received={} accepted={} rejected={} duplicates={}",
        report.stats.probes_started,
        report.stats.datagrams_sent,
        report.stats.datagrams_received,
        report.stats.datagrams_accepted,
        report.stats.datagrams_rejected,
        report.stats.duplicate_observations
    );
    if report.stats.receive_limit_reached || report.stats.device_limit_reached {
        println!(
            "limits: receive={} devices={}",
            report.stats.receive_limit_reached, report.stats.device_limit_reached
        );
    }
    for issue in &report.issues {
        eprintln!(
            "probe issue: {:?} {}: {}",
            issue.endpoint.method, issue.endpoint.destination, issue.message
        );
    }
}

fn advertised_url_summary(_url: &str) -> &'static str {
    "present (untrusted value hidden)"
}

async fn inspect_report_with_deadline<I: SnapshotInspector>(
    inspector: &I,
    report: &DiscoveryReport,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<InspectionOutcome, CliError> {
    timeout(deadline, inspect_report(inspector, report, cancellation))
        .await
        .map_err(|_| CliError::InspectionDeadline { deadline })?
}

async fn inspect_report<I: SnapshotInspector>(
    inspector: &I,
    report: &DiscoveryReport,
    cancellation: &CancellationToken,
) -> Result<InspectionOutcome, CliError> {
    if cancellation.is_cancelled() {
        return Err(CliError::InspectionCancelled);
    }

    let mut registry = DeviceRegistry::default();
    for observation in report.observations.iter().cloned() {
        registry.observe(observation, RegistryInstant::default())?;
    }

    let mut outcome = InspectionOutcome::default();
    for device in registry.devices() {
        if cancellation.is_cancelled() {
            return Err(CliError::InspectionCancelled);
        }
        outcome.attempted_devices += 1;

        let preferred_source = device.preferred_locator().map(|locator| locator.source());
        let mut locators = device.locators().collect::<Vec<_>>();
        locators
            .sort_by_key(|locator| (Some(locator.source()) != preferred_source, locator.source()));

        let mut supported_locators = 0_usize;
        let mut inspected = false;
        for locator in locators {
            if cancellation.is_cancelled() {
                return Err(CliError::InspectionCancelled);
            }

            let endpoint = match DeviceEndpoint::from_locator(locator) {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    eprintln!(
                        "inspection route issue: {} source={} is unsupported: {error}",
                        device.device_id(),
                        locator.source(),
                    );
                    continue;
                }
            };
            supported_locators += 1;

            let details = match inspector
                .fetch_snapshot_summary(&endpoint, device.device_id(), cancellation)
                .await
            {
                Ok(details) => details,
                Err(InspectionAttemptError::Cancelled) => {
                    return Err(CliError::InspectionCancelled);
                }
                Err(InspectionAttemptError::Failed(error)) => {
                    eprintln!(
                        "inspection route issue: {} source={} snapshot failed: {error}",
                        device.device_id(),
                        locator.source(),
                    );
                    continue;
                }
            };

            println!(
                "inspection {} name={:?} model={:?} firmware={:?} tuners={} channels={} favorites={} drm={}",
                device.device_id(),
                details.friendly_name,
                details.model_number,
                details.firmware_version,
                details.tuner_count,
                details.channel_count,
                details.favorite_count,
                details.drm_count,
            );
            inspected = true;
            break;
        }

        if !inspected {
            outcome.failed_devices += 1;
            if supported_locators == 0 {
                eprintln!(
                    "inspection issue: {} has no currently supported HTTP locator",
                    device.device_id()
                );
            } else {
                eprintln!(
                    "inspection issue: {} failed across all {supported_locators} supported HTTP locators",
                    device.device_id(),
                );
            }
        }
    }

    if cancellation.is_cancelled() {
        return Err(CliError::InspectionCancelled);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use balun::discovery::{DiscoveryMethod, DiscoveryObservation};

    fn parse(values: &[&str]) -> Result<Option<Cli>, CliError> {
        parse_cli(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn no_arguments_selects_local_discovery() {
        assert!(matches!(
            parse(&[]).unwrap().unwrap().actions.as_slice(),
            [Action::Local]
        ));
    }

    #[test]
    fn parses_targeted_ipv4_and_ipv6() {
        let actions = parse(&["--target", "192.168.1.10", "--target", "2001:db8::10"])
            .unwrap()
            .unwrap()
            .actions;

        assert!(matches!(actions[0], Action::Target(address) if address.ip().is_ipv4()));
        assert!(matches!(actions[1], Action::Target(address) if address.ip().is_ipv6()));
    }

    #[test]
    fn routed_range_requires_safe_private_cidr() {
        assert!(matches!(
            parse(&["--approved-range", "10.7.8.0/24"])
                .unwrap()
                .unwrap()
                .actions[0],
            Action::ApprovedRange(_)
        ));
        assert!(parse(&["--approved-range", "10.7.8.0/16"]).is_err());
        assert!(parse(&["--approved-range", "192.0.2.0/24"]).is_err());
        assert!(
            parse(&["--approved-range", "not-a-cidr"])
                .unwrap_err()
                .to_string()
                .starts_with("invalid routed range")
        );
    }

    #[test]
    fn help_short_circuits_actions() {
        assert!(parse(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn inspect_alone_selects_local_discovery() {
        let cli = parse(&["--inspect"]).unwrap().unwrap();

        assert!(cli.inspect);
        assert!(matches!(cli.actions.as_slice(), [Action::Local]));
    }

    #[test]
    fn advertised_url_summary_never_exposes_the_value() {
        let value = "http://user:password@192.0.2.10/private/path?token=secret#fragment";
        let summary = advertised_url_summary(value);

        assert_eq!(summary, "present (untrusted value hidden)");
        for secret in ["user", "password", "private", "token", "secret", "fragment"] {
            assert!(!summary.contains(secret));
        }
    }

    #[test]
    fn failed_inspection_is_a_cli_failure() {
        let error = InspectionOutcome {
            attempted_devices: 2,
            failed_devices: 1,
        }
        .require_success()
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::InspectionFailed {
                failed: 1,
                attempted: 2
            }
        ));
    }

    #[tokio::test]
    async fn inspection_retries_the_next_locator_as_a_complete_pair() {
        let inspector = FakeInspector::new(vec![
            Err(InspectionAttemptError::Failed("fixture failure".to_owned())),
            Ok(InspectionDetails {
                friendly_name: "Fallback tuner".to_owned(),
                model_number: "-".to_owned(),
                firmware_version: "-".to_owned(),
                tuner_count: "4".to_owned(),
                channel_count: 0,
                favorite_count: 0,
                drm_count: 0,
            }),
        ]);
        let report = DiscoveryReport {
            observations: vec![
                observation(65_002, DiscoveryMethod::Targeted),
                observation(65_001, DiscoveryMethod::Ipv4Broadcast),
            ],
            ..DiscoveryReport::default()
        };

        let outcome = inspect_report(&inspector, &report, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            outcome,
            InspectionOutcome {
                attempted_devices: 1,
                failed_devices: 0
            }
        );
        assert_eq!(
            inspector.attempts(),
            vec![
                "127.0.0.1:65002".parse().unwrap(),
                "127.0.0.1:65001".parse().unwrap(),
            ]
        );
    }

    #[tokio::test]
    async fn pre_cancelled_inspection_stops_before_http() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = inspect_report(
            &DeviceHttpClient::default(),
            &DiscoveryReport::default(),
            &cancellation,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, CliError::InspectionCancelled));
    }

    #[tokio::test]
    async fn inspection_report_deadline_bounds_all_locator_work() {
        let report = DiscoveryReport {
            observations: vec![observation(65_001, DiscoveryMethod::Targeted)],
            ..DiscoveryReport::default()
        };

        let error = inspect_report_with_deadline(
            &PendingInspector,
            &report,
            &CancellationToken::new(),
            Duration::ZERO,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::InspectionDeadline {
                deadline: Duration::ZERO
            }
        ));
    }

    fn observation(source_port: u16, method: DiscoveryMethod) -> DiscoveryObservation {
        DiscoveryObservation {
            device_id: DeviceId::new(0x105A_1232).unwrap(),
            source: SocketAddr::new("127.0.0.1".parse().unwrap(), source_port),
            method,
            interface: None,
            device_types: vec![1],
            tuner_count: Some(4),
            advertised_base_url: Some("http://127.0.0.1/".to_owned()),
            advertised_lineup_url: None,
        }
    }

    struct FakeInspector {
        attempts: Mutex<Vec<SocketAddr>>,
        outcomes: Mutex<VecDeque<Result<InspectionDetails, InspectionAttemptError>>>,
    }

    impl FakeInspector {
        fn new(outcomes: Vec<Result<InspectionDetails, InspectionAttemptError>>) -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
                outcomes: Mutex::new(outcomes.into()),
            }
        }

        fn attempts(&self) -> Vec<SocketAddr> {
            self.attempts.lock().unwrap().clone()
        }
    }

    impl SnapshotInspector for FakeInspector {
        async fn fetch_snapshot_summary(
            &self,
            endpoint: &DeviceEndpoint,
            _expected_device_id: DeviceId,
            _cancellation: &CancellationToken,
        ) -> Result<InspectionDetails, InspectionAttemptError> {
            self.attempts.lock().unwrap().push(endpoint.source());
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
