#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use balun::discovery::{
    DiscoveryClient, DiscoveryReport, ExactDiscoveryTarget, InvalidTypedSubnetScope,
    ObservationGeneration, ObservationWatch, ProbeConfig, RegistryError, SubnetAdmissionError,
    SubnetScanError, SubnetScanIncomplete, SubnetScanOutcome, SubnetScanPermit, SubnetScanReport,
    SubnetSearchConsent, TypedSubnetScope,
};
use balun::domain::DeviceId;
use balun::hdhr::{
    DeviceInspectionError, DeviceInspectionIssueKind, DeviceInspectionReport, DeviceInspector,
};
use thiserror::Error;
use tokio::time::Instant;
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
--approved-range searches one canonical RFC 1918 subnet, /23 through /32, and
is this invocation's confirmation of that subnet and its request budget: at
most 510 addresses, two requests each, 64 requests per second, 30 seconds.
The system's current routing selects the path. It needs network-change
observation, stops if the network changes, and is never repeated.
At most 32 actions and one approved range are accepted per invocation.
--target uses the desktop's unicast address rules and bounded reply budget.";

const MAX_CLI_ACTIONS: usize = 32;
/// Longest wait for network-change observation to establish its baseline.
const OBSERVATION_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum Action {
    Local,
    Target(SocketAddr),
    ApprovedRange(TypedSubnetScope),
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

    #[error("invalid approved range {value:?}: {source}")]
    Range {
        value: String,
        #[source]
        source: InvalidTypedSubnetScope,
    },

    #[error("subnet search needs network-change observation, which is unavailable")]
    ObservationUnavailable,

    #[error("network-change observation did not become ready within {0:?}")]
    ObservationTimeout(Duration),

    #[error("subnet search was cancelled before it started")]
    SubnetCancelled,

    #[error(transparent)]
    SubnetAdmission(#[from] SubnetAdmissionError),

    #[error(transparent)]
    SubnetScan(#[from] SubnetScanError),

    #[error("subnet search incomplete: {0}")]
    SubnetIncomplete(&'static str),

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
    let mut incomplete = None;
    for action in cli.actions {
        // A subnet search prints its own scope and budget once admitted.
        match action {
            Action::Target(_) => print_probe_budget(exact_client.config()),
            Action::Local => print_probe_budget(client.config()),
            Action::ApprovedRange(_) => {}
        }
        let report = match action {
            Action::Local => client.discover_local(&cancellation).await?,
            Action::Target(target) => {
                exact_client
                    .discover_target(target, None, &cancellation)
                    .await?
            }
            Action::ApprovedRange(scope) => {
                let Some(observation) = network_observation() else {
                    return Err(CliError::ObservationUnavailable.into());
                };
                let scan = search_subnet(
                    scope,
                    observation.watch,
                    OBSERVATION_WAIT,
                    &cancellation,
                    |permit| balun::discovery::discover_typed_subnet(permit, &cancellation),
                )
                .await?;
                print_subnet_outcome(&scan);
                incomplete = incomplete.or(incomplete_reason(scan.outcome));
                scan.report
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
    if let Some(reason) = incomplete {
        return Err(CliError::SubnetIncomplete(reason).into());
    }

    Ok(())
}

/// The native network-change source behind one subnet search. The source
/// stays alive, and its changes drained, for as long as this is held.
struct NetworkObservation {
    watch: ObservationWatch,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    _source: balun::controller::NativeNetworkChangeSource,
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn network_observation() -> Option<NetworkObservation> {
    use balun::controller::{NativeNetworkChangeSource, NetworkChangeSource};

    let source = NativeNetworkChangeSource::new();
    let subscription = source.subscribe()?;
    let mut changes = subscription.changes;
    // Readiness, not the debounced changes, governs the search; draining
    // keeps the watcher from waiting on a full stream.
    tokio::spawn(async move { while changes.recv().await.is_some() {} });
    Some(NetworkObservation {
        watch: subscription.observation,
        _source: source,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn network_observation() -> Option<NetworkObservation> {
    None
}

/// Wait for the first healthy observation baseline, at most `limit`.
async fn wait_for_baseline(
    observation: &mut ObservationWatch,
    limit: Duration,
    cancellation: &CancellationToken,
) -> Result<ObservationGeneration, CliError> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(generation) = observation.current().generation() {
            return Ok(generation);
        }
        if observation.is_closed() {
            return Err(CliError::ObservationUnavailable);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(CliError::SubnetCancelled),
            () = tokio::time::sleep_until(deadline) => {
                return Err(CliError::ObservationTimeout(limit));
            }
            _ = observation.changed() => {}
        }
    }
}

/// Run this invocation's one subnet search.
///
/// The explicit argument is validated without granting anything. Only once
/// observation has a healthy baseline is the argument taken as consent for
/// exactly the printed scope and budget, bound to that generation, and
/// consumed once by admission. `search` runs at most once; a changed
/// network ends the invocation instead of replaying the argument.
async fn search_subnet<F, Fut>(
    scope: TypedSubnetScope,
    mut observation: ObservationWatch,
    limit: Duration,
    cancellation: &CancellationToken,
    search: F,
) -> Result<SubnetScanReport, CliError>
where
    F: FnOnce(SubnetScanPermit) -> Fut,
    Fut: Future<Output = Result<SubnetScanReport, SubnetScanError>>,
{
    let generation = wait_for_baseline(&mut observation, limit, cancellation).await?;
    let candidates = scope.candidate_count();
    let requests = scope.maximum_request_attempts();
    eprintln!(
        "subnet search: {scope} as entered, {candidates} addresses, at most {requests} \
         outbound requests; the system's current routing selects the path"
    );
    let consent = SubnetSearchConsent::confirm(scope, generation);
    let permit = consent.admit(&observation)?;
    Ok(search(permit).await?)
}

fn incomplete_reason(outcome: SubnetScanOutcome) -> Option<&'static str> {
    match outcome {
        SubnetScanOutcome::Complete => None,
        SubnetScanOutcome::Incomplete(reason) => Some(match reason {
            SubnetScanIncomplete::Deadline => "the 30-second deadline expired",
            SubnetScanIncomplete::DeviceLimit => "the 64-device limit was reached",
            SubnetScanIncomplete::NetworkChanged => "the network changed",
            SubnetScanIncomplete::Cancelled => "it was cancelled",
        }),
    }
}

fn print_subnet_outcome(scan: &SubnetScanReport) {
    eprintln!(
        "subnet search: {} outbound requests, {} refused by the system and not retried; {}",
        scan.requests_attempted,
        scan.refused_sends,
        incomplete_reason(scan.outcome).map_or_else(
            || "complete".to_owned(),
            |reason| format!("incomplete because {reason}")
        )
    );
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
                // Validation only: consent is taken once observation is ready.
                let scope = value
                    .parse::<TypedSubnetScope>()
                    .map_err(|source| CliError::Range { value, source })?;
                actions.push(Action::ApprovedRange(scope));
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
        DeviceInspectionIssueKind::LocatorConflict => "is claimed by another device",
    };
    writeln!(
        stderr,
        "inspection route issue: {device_id} source={source} {reason}: {message}"
    )
}

#[cfg(test)]
mod tests {
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
        let mut arguments = ["--target", "192.0.2.1"].repeat(MAX_CLI_ACTIONS);
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
    fn approved_range_uses_the_typed_subnet_policy() {
        for (value, candidates) in [
            ("10.7.8.0/23", 510),
            ("10.7.8.0/24", 254),
            ("172.16.0.0/31", 2),
            ("192.168.1.9/32", 1),
        ] {
            let actions = parse(&["--approved-range", value])
                .unwrap()
                .unwrap()
                .actions;
            let [Action::ApprovedRange(scope)] = actions.as_slice() else {
                panic!("{value} is one approved range");
            };
            assert_eq!(scope.to_string(), value);
            assert_eq!(scope.candidate_count(), candidates);
            assert_eq!(scope.maximum_request_attempts(), candidates * 2);
        }
        for value in [
            "10.7.8.0/22",
            "10.7.8.1/24",
            "192.0.2.0/24",
            "127.0.0.0/24",
            "169.254.0.0/24",
            "fd00::/120",
            "not-a-cidr",
        ] {
            let error = parse(&["--approved-range", value]).unwrap_err();
            assert!(
                error.to_string().starts_with("invalid approved range"),
                "{value}: {error}"
            );
        }
    }

    fn scan_report(outcome: SubnetScanOutcome) -> SubnetScanReport {
        SubnetScanReport {
            report: DiscoveryReport::default(),
            outcome,
            requests_attempted: 0,
            refused_sends: 0,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn subnet_consent_waits_for_observation_and_is_spent_once() {
        use balun::discovery::ObservationGate;
        use std::cell::Cell;

        let scope: TypedSubnetScope = "192.168.2.0/23".parse().unwrap();
        let cancellation = CancellationToken::new();

        // Nothing observes: no search, no consent.
        let searches = Cell::new(0);
        let unavailable = search_subnet(
            scope,
            ObservationWatch::unavailable(),
            OBSERVATION_WAIT,
            &cancellation,
            |_| {
                searches.set(searches.get() + 1);
                async { Ok(scan_report(SubnetScanOutcome::Complete)) }
            },
        )
        .await;
        assert!(matches!(unavailable, Err(CliError::ObservationUnavailable)));
        let gate = ObservationGate::new();
        let pending = search_subnet(scope, gate.watch(), OBSERVATION_WAIT, &cancellation, |_| {
            searches.set(searches.get() + 1);
            async { Ok(scan_report(SubnetScanOutcome::Complete)) }
        })
        .await;
        assert!(matches!(pending, Err(CliError::ObservationTimeout(_))));
        assert_eq!(searches.get(), 0);

        // A ready baseline admits exactly one search, bound to it. A network
        // change during that search ends the invocation; nothing is replayed.
        let ready = {
            let gate = gate.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                gate.establish();
            }
        };
        let search = search_subnet(
            scope,
            gate.watch(),
            OBSERVATION_WAIT,
            &cancellation,
            |permit| {
                searches.set(searches.get() + 1);
                assert!(permit.is_live());
                assert_eq!(permit.scope(), scope);
                gate.invalidate();
                assert!(!permit.is_live());
                async {
                    Ok(scan_report(SubnetScanOutcome::Incomplete(
                        SubnetScanIncomplete::NetworkChanged,
                    )))
                }
            },
        );
        let (search, ()) = tokio::join!(search, ready);
        let report = search.unwrap();
        assert_eq!(searches.get(), 1);
        assert_eq!(
            incomplete_reason(report.outcome),
            Some("the network changed")
        );
        assert_eq!(incomplete_reason(SubnetScanOutcome::Complete), None);

        // Consent from an earlier generation is refused, even once the
        // network is observed again.
        let earlier = gate.watch();
        gate.establish();
        let current = gate.state().generation().unwrap();
        let stale = SubnetSearchConsent::confirm(
            scope,
            ObservationGeneration::new(current.get() - 1).unwrap(),
        );
        assert_eq!(
            stale.admit(&earlier).unwrap_err(),
            SubnetAdmissionError::Stale
        );

        cancellation.cancel();
        gate.invalidate();
        let cancelled = search_subnet(scope, gate.watch(), OBSERVATION_WAIT, &cancellation, |_| {
            searches.set(searches.get() + 1);
            async { Ok(scan_report(SubnetScanOutcome::Complete)) }
        })
        .await;
        assert!(matches!(cancelled, Err(CliError::SubnetCancelled)));
        assert_eq!(searches.get(), 1);
    }

    #[test]
    fn help_short_circuits_actions() {
        assert!(parse(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn retired_route_provider_report_is_an_unknown_option() {
        assert!(
            parse(&["--providers"])
                .unwrap_err()
                .to_string()
                .starts_with("unknown option \"--providers\"")
        );
        assert!(!USAGE.contains("--providers"));
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
