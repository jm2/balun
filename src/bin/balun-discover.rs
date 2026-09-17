use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::time::Duration;

use balun::discovery::{
    ApprovedIpv4Range, DiscoveryClient, DiscoveryReport, ExactDiscoveryTarget, ProbeConfig,
    RegistryError, RoutedRangeError, RoutedScanConfig,
};
#[cfg(any(target_os = "linux", test))]
use balun::discovery::{RouteCandidateError, RouteSnapshot, select_route_candidates};
use balun::domain::DeviceId;
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
  balun-discover --providers

No arguments performs ordinary local-interface discovery.
--providers reports route-provider availability and tunnel candidate counts
without sending packets or printing any address or route.
--inspect also fetches bounded device metadata and lineup counts; it never
starts a stream or allocates a tuner.
Routed enumeration requires the explicit --approved-range option and is
limited by Balun's private-/24 and packet-rate safety policy.
At most 32 actions and one approved range are accepted per invocation.
--target uses the desktop's unicast address rules and bounded reply budget.";

const MAX_CLI_ACTIONS: usize = 32;

#[derive(Clone, Copy, Debug)]
enum Action {
    Local,
    Target(SocketAddr),
    ApprovedRange(ApprovedIpv4Range),
    Providers,
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
    balun::logging::init();
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
    let exact_client = DiscoveryClient::new(ProbeConfig::exact_target());
    let inspector = DeviceInspector::default();
    let inspect = cli.inspect;
    let mut inspection = InspectionOutcome::default();
    for action in cli.actions {
        match action {
            Action::Providers => {}
            Action::Target(_) => print_probe_budget(exact_client.config()),
            Action::Local | Action::ApprovedRange(_) => print_probe_budget(client.config()),
        }
        let report = match action {
            Action::Providers => {
                print_providers();
                continue;
            }
            Action::Local => client.discover_local(&cancellation).await?,
            Action::Target(target) => {
                exact_client
                    .discover_target(target, None, &cancellation)
                    .await?
            }
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
            "--providers" => actions.push(Action::Providers),
            "--local" => actions.push(Action::Local),
            "--target" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| CliError::Usage("--target requires an IP address".to_owned()))?;
                actions.push(Action::Target(parse_target(&value)?));
            }
            "--approved-range" => {
                if actions
                    .iter()
                    .any(|action| matches!(action, Action::ApprovedRange(_)))
                {
                    return Err(CliError::Usage(
                        "only one approved range is allowed per invocation".to_owned(),
                    ));
                }
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
        if actions.len() > MAX_CLI_ACTIONS {
            return Err(CliError::Usage(format!(
                "at most {MAX_CLI_ACTIONS} actions are allowed per invocation"
            )));
        }
    }

    if actions.is_empty() {
        actions.push(Action::Local);
    }

    Ok(Some(Cli { actions, inspect }))
}

fn parse_target(value: &str) -> Result<SocketAddr, CliError> {
    ExactDiscoveryTarget::parse(value)
        .map(|target| SocketAddr::new(target.ip_addr(), 0))
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
            "probe issue: {:?} {} class={}: {}",
            issue.endpoint.method,
            issue.endpoint.destination,
            issue.class.name(),
            issue.message
        );
    }
}

/// The fixed per-probe traffic budget every action below runs under.
fn print_probe_budget(config: ProbeConfig) {
    println!(
        "probe budget: attempts={} response_window_ms={} max_received_datagrams={} max_devices={}",
        config.attempts(),
        config.response_window().as_millis(),
        config.max_received_datagrams(),
        config.max_unique_devices()
    );
}

/// Bounded counts from one route snapshot, never a route or an address.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct ProviderCounts {
    interfaces: usize,
    effective_routes: usize,
    /// Every active, unambiguously classified tunnel the provider reported,
    /// whether or not one of its routes is eligible to produce a candidate.
    tunnel_interfaces: usize,
    tunnel_candidates: Result<usize, RouteCandidateError>,
}

#[cfg(any(target_os = "linux", test))]
fn provider_counts(snapshot: &RouteSnapshot) -> ProviderCounts {
    ProviderCounts {
        interfaces: snapshot.interfaces().len(),
        effective_routes: snapshot.effective_routes().len(),
        tunnel_interfaces: snapshot.tunnel_interfaces().len(),
        tunnel_candidates: select_route_candidates(snapshot, &[])
            .map(|candidates| candidates.len()),
    }
}

#[cfg(any(target_os = "linux", test))]
impl std::fmt::Display for ProviderCounts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "interfaces={} effective_routes={} tunnel_interfaces={}",
            self.interfaces, self.effective_routes, self.tunnel_interfaces
        )?;
        match &self.tunnel_candidates {
            Ok(count) => write!(formatter, " tunnel_candidates={count}"),
            Err(error) => write!(formatter, "; candidate selection failed: {error}"),
        }
    }
}

/// Route-provider availability and bounded counts, never a route or address.
#[cfg(target_os = "linux")]
fn print_providers() {
    use balun::discovery::{LinuxRouteProvider, RouteProvider};

    match LinuxRouteProvider::new().snapshot() {
        Ok(snapshot) => println!(
            "route provider: linux rtnetlink available; {}",
            provider_counts(&snapshot)
        ),
        Err(error) => {
            // The reason names the rtnetlink step or unsupported route shape
            // that failed closed; it carries no address, prefix, or interface.
            println!("route provider: linux rtnetlink unavailable ({error})");
            return;
        }
    }
    println!("routed discovery: offered on this platform; approvals are asked for in the desktop");
}

#[cfg(not(target_os = "linux"))]
fn print_providers() {
    println!("route provider: unavailable on this platform (no native route provider yet)");
    println!("routed discovery: not offered; use --target or a hostname for a tunnelled tuner");
}

fn advertised_url_summary(_url: &str) -> &'static str {
    "present (untrusted value hidden)"
}

fn print_inspection_report(report: &DeviceInspectionReport) {
    for device in report.devices() {
        for issue in device.issues() {
            write_inspection_issue(
                &mut std::io::stderr().lock(),
                device.device_id(),
                issue.source(),
                issue.kind(),
                issue.message(),
            )
            .expect("write inspection diagnostic");
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

fn write_inspection_issue(
    stderr: &mut impl std::io::Write,
    device_id: DeviceId,
    source: SocketAddr,
    kind: DeviceInspectionIssueKind,
    message: &str,
) -> std::io::Result<()> {
    let reason = match kind {
        DeviceInspectionIssueKind::UnsupportedEndpoint => "is unsupported",
        DeviceInspectionIssueKind::SnapshotFailed => "snapshot failed",
    };
    writeln!(
        stderr,
        "inspection route issue: {device_id} source={source} {reason}: {message}"
    )
}

#[cfg(test)]
mod tests {
    use balun::discovery::{
        InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteScope,
    };

    use super::*;

    #[test]
    fn cli_inspection_stderr_keeps_json_category_and_position_without_values() {
        use balun::hdhr::{
            DeviceHttpError, DeviceSnapshotError, JsonParseError, LineupError, LineupFetchError,
        };
        const MARKER: &str =
            "http://user:PRIVATE_PASSWORD@invalid.example/stream?secret=JSON_MARKER_719";
        let parse_error = || {
            let json = serde_json::to_string(MARKER).unwrap();
            JsonParseError::from(serde_json::from_str::<u8>(&json).unwrap_err())
        };
        for error in [
            DeviceSnapshotError::Metadata(DeviceHttpError::Json(parse_error())),
            DeviceSnapshotError::Lineup(LineupFetchError::Lineup(LineupError::Json(parse_error()))),
        ] {
            let mut stderr = Vec::new();
            write_inspection_issue(
                &mut stderr,
                DeviceId::new(0x105A_1232).unwrap(),
                "192.0.2.1:65001".parse().unwrap(),
                DeviceInspectionIssueKind::SnapshotFailed,
                &error.to_string(),
            )
            .unwrap();
            let stderr = String::from_utf8(stderr).unwrap();
            assert!(stderr.contains("snapshot failed:"));
            assert!(stderr.contains("JSON field type or shape mismatch at line 1, column"));
            assert!(!stderr.contains("PRIVATE_PASSWORD"), "{stderr}");
            assert!(!stderr.contains("JSON_MARKER_719"), "{stderr}");
        }
    }

    fn parse(values: &[&str]) -> Result<Option<Cli>, CliError> {
        parse_cli(values.iter().map(|value| (*value).to_owned()))
    }

    fn snapshot(kind: InterfaceKind, is_up: bool, route: &str) -> RouteSnapshot {
        let tunnel = InterfaceId::new(7);
        RouteSnapshot::from_effective_routes(
            vec![NetworkInterface::new(
                tunnel,
                "wg0",
                kind,
                is_up,
                ["10.255.0.2/32".parse().unwrap()],
            )],
            vec![NetworkRoute::effective(
                route.parse().unwrap(),
                Some(tunnel),
                RouteKind::Unicast,
                RouteScope::OnLink,
            )],
        )
    }

    #[test]
    fn tunnel_interfaces_are_counted_before_candidate_selection() {
        // An active tunnel whose only route is public yields no candidate but
        // is still one recognized tunnel.
        let counts = provider_counts(&snapshot(InterfaceKind::Tunnel, true, "198.51.100.0/24"));
        assert_eq!(
            (
                counts.interfaces,
                counts.effective_routes,
                counts.tunnel_interfaces
            ),
            (1, 1, 1)
        );
        assert!(matches!(counts.tunnel_candidates, Ok(0)));
        assert_eq!(
            counts.to_string(),
            "interfaces=1 effective_routes=1 tunnel_interfaces=1 tunnel_candidates=0"
        );

        // The same tunnel with an eligible private route produces candidates.
        let counts = provider_counts(&snapshot(InterfaceKind::Tunnel, true, "192.168.40.8/30"));
        assert_eq!(counts.tunnel_interfaces, 1);
        assert!(matches!(counts.tunnel_candidates, Ok(count) if count > 0));

        // A down tunnel or a non-tunnel interface is not a tunnel interface.
        for (kind, is_up) in [(InterfaceKind::Tunnel, false), (InterfaceKind::Other, true)] {
            let counts = provider_counts(&snapshot(kind, is_up, "192.168.40.8/30"));
            assert_eq!(counts.tunnel_interfaces, 0);
            assert!(matches!(counts.tunnel_candidates, Ok(0)));
        }
    }

    #[test]
    fn providers_is_a_packet_free_action() {
        let cli = parse(&["--providers"]).unwrap().unwrap();
        assert!(matches!(cli.actions.as_slice(), [Action::Providers]));
        assert!(!cli.inspect);
        let mixed = parse(&["--providers", "--local"]).unwrap().unwrap();
        assert!(matches!(
            mixed.actions.as_slice(),
            [Action::Providers, Action::Local]
        ));
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
    fn target_admission_and_reply_budget_match_the_desktop_boundary() {
        for target in [
            "127.0.0.1",
            "127.255.255.254",
            "::1",
            "::ffff:127.0.0.1",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "255.255.255.255",
            "fe80::1",
            "[fe80::1%3]",
            "192.0.2.1:1234",
            "http://192.0.2.1/",
            "localhost",
        ] {
            assert!(parse(&["--target", target]).is_err(), "{target}");
        }
        let config = ProbeConfig::exact_target();
        assert_eq!(config.attempts(), 2);
        assert_eq!(config.response_window(), Duration::from_millis(200));
        assert_eq!(config.max_received_datagrams(), 16);
        assert_eq!(config.max_unique_devices(), 1);
    }

    #[test]
    fn complete_cli_is_admitted_before_any_action_can_run() {
        let mut arguments = vec!["--target", "192.0.2.1"].repeat(MAX_CLI_ACTIONS);
        assert_eq!(
            parse(&arguments).unwrap().unwrap().actions.len(),
            MAX_CLI_ACTIONS
        );
        arguments.extend(["--target", "192.0.2.2"]);
        assert!(parse(&arguments).is_err());
        assert!(parse(&["--local", "--target", "127.0.0.1"]).is_err());
        assert!(
            parse(&[
                "--approved-range",
                "10.0.0.0/24",
                "--approved-range",
                "10.0.1.0/24"
            ])
            .is_err()
        );
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
