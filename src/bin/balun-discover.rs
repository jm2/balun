use std::env;
use std::error::Error;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use balun::discovery::{
    ApprovedIpv4Range, DiscoveryClient, DiscoveryReport, RegistryError, RoutedRangeError,
    RoutedScanConfig,
};
use balun::hdhr::{
    DeviceInspectionError, DeviceInspectionIssueKind, DeviceInspectionReport, DeviceInspector,
};
use ipnet::Ipv4Net;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

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

    #[error("device inspection report has {actual} {field}; maximum is {maximum}")]
    InspectionReportLimit {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("inspection failed for {failed} of {attempted} discovered devices")]
    InspectionFailed { failed: usize, attempted: usize },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InspectionOutcome {
    attempted_devices: usize,
    failed_devices: usize,
}

impl InspectionOutcome {
    fn from_report(report: &DeviceInspectionReport) -> Self {
        Self {
            attempted_devices: report.attempted_devices(),
            failed_devices: report.failed_devices(),
        }
    }

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

impl From<DeviceInspectionError> for CliError {
    fn from(error: DeviceInspectionError) -> Self {
        match error {
            DeviceInspectionError::Registry(error) => Self::InspectionRegistry(error),
            DeviceInspectionError::Cancelled => Self::InspectionCancelled,
            DeviceInspectionError::Deadline { deadline } => Self::InspectionDeadline { deadline },
            DeviceInspectionError::ReportLimit {
                field,
                actual,
                maximum,
            } => Self::InspectionReportLimit {
                field,
                actual,
                maximum,
            },
        }
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
    let inspector = DeviceInspector::default();
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
            let inspected = inspector
                .inspect_discovery_report(&report, &cancellation)
                .await
                .map_err(CliError::from)?;
            print_inspection_report(&inspected);
            inspection.merge(InspectionOutcome::from_report(&inspected));
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

fn print_inspection_report(report: &DeviceInspectionReport) {
    for device in report.devices() {
        for issue in device.issues() {
            match issue.kind() {
                DeviceInspectionIssueKind::UnsupportedEndpoint => eprintln!(
                    "inspection route issue: {} source={} is unsupported: {}",
                    device.device_id(),
                    issue.source(),
                    issue.message(),
                ),
                DeviceInspectionIssueKind::SnapshotFailed => eprintln!(
                    "inspection route issue: {} source={} snapshot failed: {}",
                    device.device_id(),
                    issue.source(),
                    issue.message(),
                ),
            }
        }

        if let Some(summary) = device.summary() {
            println!(
                "inspection {} address={} name={:?} model={:?} firmware={:?} tuners={} channels={} favorites={} drm={}",
                summary.device_id(),
                summary.source(),
                summary.friendly_name().unwrap_or("-"),
                summary.model_number().unwrap_or("-"),
                summary.firmware_version().unwrap_or("-"),
                summary
                    .tuner_count()
                    .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
                summary.channel_count(),
                summary.favorite_count(),
                summary.drm_count(),
            );
        } else if device.supported_locator_count() == 0 {
            eprintln!(
                "inspection issue: {} has no currently supported HTTP locator",
                device.device_id()
            );
        } else {
            let supported = device.supported_locator_count();
            eprintln!(
                "inspection issue: {} failed across all {supported} supported HTTP locators",
                device.device_id(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
