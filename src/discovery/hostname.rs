//! Hostname entry for exact-address discovery.
//!
//! A hostname is resolved once through the system resolver into a bounded set
//! of usable unicast addresses, each of which is then probed exactly like a
//! typed address. Resolution never turns a name into scan authority: at most
//! [`MAX_RESOLVED_ADDRESSES`] addresses are kept, every one must pass the same
//! unicast checks as typed text, and the lookup is bounded by
//! [`HOSTNAME_RESOLUTION_TIMEOUT`].

use std::fmt;
use std::io;
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

use super::manual::{ExactDiscoveryTarget, InvalidExactDiscoveryTarget};

/// Longest hostname accepted from the entry field, per RFC 1123.
pub const MAX_HOSTNAME_BYTES: usize = 253;
/// Longest single label in a hostname.
const MAX_LABEL_BYTES: usize = 63;
/// Most addresses kept from one resolution.
pub const MAX_RESOLVED_ADDRESSES: usize = 4;
/// Time allowed for one resolution.
pub const HOSTNAME_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(5);

/// One validated, lowercase hostname that may be resolved into exact targets.
///
/// Default debug output is topology-redacted; the name is exposed only
/// through [`Self::name`] for the resolver.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HostnameTarget {
    name: String,
}

impl HostnameTarget {
    /// Parse one hostname. Trailing dots are removed, ASCII letters are
    /// lowercased, and IP literals, URLs, ports, and ranges are rejected.
    pub fn parse(value: &str) -> Result<Self, InvalidHostnameTarget> {
        value.parse()
    }

    /// The normalized name, for resolution only.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl FromStr for HostnameTarget {
    type Err = InvalidHostnameTarget;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err(InvalidHostnameTarget::Empty);
        }
        if value.len() > MAX_HOSTNAME_BYTES + 1 {
            return Err(InvalidHostnameTarget::TooLong {
                maximum: MAX_HOSTNAME_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(InvalidHostnameTarget::ControlCharacter);
        }
        if value.parse::<std::net::IpAddr>().is_ok() {
            return Err(InvalidHostnameTarget::IpAddressLiteral);
        }
        if value
            .chars()
            .any(|character| matches!(character, ':' | '/' | '@' | '[' | ']' | '%' | '?' | '#'))
            || value.chars().any(char::is_whitespace)
        {
            return Err(InvalidHostnameTarget::InvalidSyntax);
        }

        let name = value
            .strip_suffix('.')
            .unwrap_or(value)
            .to_ascii_lowercase();
        if name.is_empty() || name.len() > MAX_HOSTNAME_BYTES {
            return Err(InvalidHostnameTarget::InvalidSyntax);
        }
        for label in name.split('.') {
            let valid_label = !label.is_empty()
                && label.len() <= MAX_LABEL_BYTES
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-');
            if !valid_label {
                return Err(InvalidHostnameTarget::InvalidSyntax);
            }
        }
        Ok(Self { name })
    }
}

impl fmt::Debug for HostnameTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostnameTarget")
            .field("name", &"<redacted>")
            .finish()
    }
}

/// Fixed, topology-free reason a hostname entry was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidHostnameTarget {
    #[error("enter a device name or address")]
    Empty,
    #[error("device name is too long; maximum is {maximum} bytes")]
    TooLong { maximum: usize },
    #[error("device name contains a forbidden control character")]
    ControlCharacter,
    #[error("enter a hostname without a URL, port, path, or range")]
    InvalidSyntax,
    #[error("the value is an IP address, not a hostname")]
    IpAddressLiteral,
}

/// One entry from the address field: a numeric address or a hostname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryEntry {
    Address(ExactDiscoveryTarget),
    Hostname(HostnameTarget),
}

/// Why an entry was rejected, keeping the address parser's exact reasons.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidDiscoveryEntry {
    #[error(transparent)]
    Address(InvalidExactDiscoveryTarget),
    #[error(transparent)]
    Hostname(InvalidHostnameTarget),
}

impl DiscoveryEntry {
    /// Parse one field value. Text that looks numeric, bracketed, scoped, or
    /// URL-like goes to the address parser so its specific messages apply;
    /// everything else must be a valid hostname.
    pub fn parse(value: &str) -> Result<Self, InvalidDiscoveryEntry> {
        let trimmed = value.trim();
        if looks_like_address(trimmed) {
            ExactDiscoveryTarget::parse(trimmed)
                .map(Self::Address)
                .map_err(InvalidDiscoveryEntry::Address)
        } else {
            HostnameTarget::parse(trimmed)
                .map(Self::Hostname)
                .map_err(InvalidDiscoveryEntry::Hostname)
        }
    }
}

fn looks_like_address(value: &str) -> bool {
    value.is_empty()
        || value.starts_with('[')
        || value.contains(':')
        || value.contains('%')
        || value.contains('/')
        || value
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
}

/// Why a hostname produced no exact targets. No name or address is carried.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HostnameResolutionError {
    #[error("the device name did not resolve within {} seconds", HOSTNAME_RESOLUTION_TIMEOUT.as_secs())]
    Timeout,
    #[error("the device name could not be resolved ({0:?})")]
    Lookup(io::ErrorKind),
    #[error("the device name resolved to no usable unicast address")]
    NoUsableAddress,
    #[error("the controller stopped before resolving the device name")]
    ControllerStopped,
}

/// Resolve `target` through the system resolver into at most
/// [`MAX_RESOLVED_ADDRESSES`] distinct usable unicast addresses.
pub async fn resolve_hostname(
    target: &HostnameTarget,
) -> Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError> {
    let lookup = tokio::net::lookup_host((target.name(), 0));
    let addresses = tokio::time::timeout(HOSTNAME_RESOLUTION_TIMEOUT, lookup)
        .await
        .map_err(|_| HostnameResolutionError::Timeout)?
        .map_err(|error| HostnameResolutionError::Lookup(error.kind()))?;

    let mut targets: Vec<ExactDiscoveryTarget> = Vec::new();
    for address in addresses {
        let Ok(target) = ExactDiscoveryTarget::from_ip(address.ip()) else {
            continue;
        };
        if !targets.contains(&target) {
            targets.push(target);
            if targets.len() == MAX_RESOLVED_ADDRESSES {
                break;
            }
        }
    }
    if targets.is_empty() {
        return Err(HostnameResolutionError::NoUsableAddress);
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_normalized_and_bounded() {
        let target = HostnameTarget::parse("  Tuner.Example.  ").expect("valid hostname");
        assert_eq!(target.name(), "tuner.example");
        assert_eq!(
            HostnameTarget::parse("hdhr").expect("single label").name(),
            "hdhr"
        );
        assert_eq!(
            HostnameTarget::parse("a-1.b2.c")
                .expect("hyphens inside labels")
                .name(),
            "a-1.b2.c"
        );
        assert!(!format!("{target:?}").contains("tuner"));

        assert_eq!(
            HostnameTarget::parse("   "),
            Err(InvalidHostnameTarget::Empty)
        );
        assert_eq!(
            HostnameTarget::parse(&"a".repeat(MAX_HOSTNAME_BYTES + 2)),
            Err(InvalidHostnameTarget::TooLong {
                maximum: MAX_HOSTNAME_BYTES
            })
        );
        assert_eq!(
            HostnameTarget::parse("tuner\u{7}"),
            Err(InvalidHostnameTarget::ControlCharacter)
        );
        assert_eq!(
            HostnameTarget::parse("192.0.2.9"),
            Err(InvalidHostnameTarget::IpAddressLiteral)
        );
        for value in [
            "http://tuner.example",
            "tuner.example:5004",
            "tuner.example/path",
            "user@tuner",
            "-tuner.example",
            "tuner-.example",
            "tuner..example",
            "tun er",
            "tuner.exämple",
        ] {
            assert_eq!(
                HostnameTarget::parse(value),
                Err(InvalidHostnameTarget::InvalidSyntax),
                "{value:?}"
            );
        }
    }

    #[test]
    fn entries_route_numeric_text_to_the_address_parser() {
        assert_eq!(
            DiscoveryEntry::parse("192.0.2.9"),
            Ok(DiscoveryEntry::Address(
                ExactDiscoveryTarget::parse("192.0.2.9").unwrap()
            ))
        );
        assert_eq!(
            DiscoveryEntry::parse("[fd12::9]"),
            Ok(DiscoveryEntry::Address(
                ExactDiscoveryTarget::parse("fd12::9").unwrap()
            ))
        );
        assert_eq!(
            DiscoveryEntry::parse("Tuner.Example"),
            Ok(DiscoveryEntry::Hostname(
                HostnameTarget::parse("tuner.example").unwrap()
            ))
        );
        assert!(matches!(
            DiscoveryEntry::parse(""),
            Err(InvalidDiscoveryEntry::Address(
                InvalidExactDiscoveryTarget::Empty
            ))
        ));
        assert!(matches!(
            DiscoveryEntry::parse("192.0.2.0/24"),
            Err(InvalidDiscoveryEntry::Address(_))
        ));
        assert!(matches!(
            DiscoveryEntry::parse("127.0.0.1"),
            Err(InvalidDiscoveryEntry::Address(
                InvalidExactDiscoveryTarget::UnicastRequired
            ))
        ));
        assert!(matches!(
            DiscoveryEntry::parse("http://tuner.example/"),
            Err(InvalidDiscoveryEntry::Address(_))
        ));
        assert!(matches!(
            DiscoveryEntry::parse("tuner_example"),
            Err(InvalidDiscoveryEntry::Hostname(
                InvalidHostnameTarget::InvalidSyntax
            ))
        ));
    }

    #[tokio::test]
    async fn loopback_only_names_resolve_to_no_usable_address() {
        let target = HostnameTarget::parse("localhost").unwrap();
        assert_eq!(
            resolve_hostname(&target).await,
            Err(HostnameResolutionError::NoUsableAddress)
        );
    }

    #[test]
    fn resolution_errors_carry_no_name_or_address() {
        for error in [
            HostnameResolutionError::Timeout,
            HostnameResolutionError::Lookup(io::ErrorKind::NotFound),
            HostnameResolutionError::NoUsableAddress,
            HostnameResolutionError::ControllerStopped,
        ] {
            let text = error.to_string();
            assert!(!text.contains("localhost") && !text.contains("192."));
        }
    }
}
