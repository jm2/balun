use std::env;
use std::error::Error;
use std::net::{IpAddr, SocketAddr};

use balun::discovery::{
    ApprovedIpv4Range, DiscoveryClient, DiscoveryReport, RoutedRangeError, RoutedScanConfig,
};
use ipnet::Ipv4Net;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "\
Usage:
  balun-discover
  balun-discover --local
  balun-discover --target <IP> [--target <IP> ...]
  balun-discover --approved-range <PRIVATE-CIDR>

No arguments performs ordinary local-interface discovery.
Routed enumeration requires the explicit --approved-range option and is
limited by Balun's private-/24 and packet-rate safety policy.";

#[derive(Clone, Copy, Debug)]
enum Action {
    Local,
    Target(SocketAddr),
    ApprovedRange(ApprovedIpv4Range),
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(actions) = parse_actions(env::args().skip(1))? else {
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
    for action in actions {
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
    }

    Ok(())
}

fn parse_actions(arguments: impl Iterator<Item = String>) -> Result<Option<Vec<Action>>, CliError> {
    let mut arguments = arguments.peekable();
    if arguments.peek().is_none() {
        return Ok(Some(vec![Action::Local]));
    }

    let mut actions = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
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
        return Err(CliError::Usage(
            "no discovery action was selected".to_owned(),
        ));
    }

    Ok(Some(actions))
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
            println!("  advertised_base_url={url:?}");
        }
        if let Some(url) = &observation.advertised_lineup_url {
            println!("  advertised_lineup_url={url:?}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Result<Option<Vec<Action>>, CliError> {
        parse_actions(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn no_arguments_selects_local_discovery() {
        assert!(matches!(
            parse(&[]).unwrap().unwrap().as_slice(),
            [Action::Local]
        ));
    }

    #[test]
    fn parses_targeted_ipv4_and_ipv6() {
        let actions = parse(&["--target", "192.168.1.10", "--target", "2001:db8::10"])
            .unwrap()
            .unwrap();

        assert!(matches!(actions[0], Action::Target(address) if address.ip().is_ipv4()));
        assert!(matches!(actions[1], Action::Target(address) if address.ip().is_ipv6()));
    }

    #[test]
    fn routed_range_requires_safe_private_cidr() {
        assert!(matches!(
            parse(&["--approved-range", "10.7.8.0/24"])
                .unwrap()
                .unwrap()[0],
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
}
