use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use thiserror::Error;

/// Maximum UTF-8 size accepted from one exact-address entry field.
pub const MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES: usize = 128;

/// One canonical, numeric address approved for an exact HDHomeRun probe.
///
/// The address contains no caller-selected port. Discovery implementations
/// obtain it through [`Self::ip_addr`] and must normalize the destination to
/// the HDHomeRun discovery port internally.
///
/// Default debug output is deliberately topology-redacted. Callers must not
/// log the value returned by [`Self::ip_addr`].
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExactDiscoveryTarget {
    address: IpAddr,
}

impl ExactDiscoveryTarget {
    /// Parse one numeric IPv4 or non-link-local IPv6 address.
    ///
    /// This parser never resolves hostnames and never interprets URLs, CIDR
    /// ranges, or caller-selected ports. Scoped IPv6 is recognized only so it
    /// can fail closed while Balun's HTTP transport cannot preserve a scope.
    pub fn parse(value: &str) -> Result<Self, InvalidExactDiscoveryTarget> {
        value.parse()
    }

    /// Accept an already-parsed address under the same unicast rules as
    /// typed text; used for resolver results.
    pub(crate) fn from_ip(address: IpAddr) -> Result<Self, InvalidExactDiscoveryTarget> {
        validate_address(address)?;
        Ok(Self { address })
    }

    /// Return the validated numeric address for a discovery implementation.
    ///
    /// This is an explicit network-topology boundary. Callers must use the
    /// address only to perform the requested probe and must not log or durably
    /// persist the raw admission entry. Discovery status/state never carries
    /// it; only a separately validated responder locator may reach a device
    /// projection.
    #[must_use]
    pub const fn ip_addr(self) -> IpAddr {
        self.address
    }

    /// Return the validated target with an intentionally unspecified port.
    #[must_use]
    pub(crate) const fn socket_addr(self) -> SocketAddr {
        SocketAddr::new(self.address, 0)
    }
}

impl FromStr for ExactDiscoveryTarget {
    type Err = InvalidExactDiscoveryTarget;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES {
            return Err(InvalidExactDiscoveryTarget::TooLong {
                maximum: MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(InvalidExactDiscoveryTarget::ControlCharacter);
        }

        let value = value.trim();
        if value.is_empty() {
            return Err(InvalidExactDiscoveryTarget::Empty);
        }

        let (address, bracketed) = strip_optional_brackets(value)?;
        if address.contains(['[', ']'])
            || value.contains('/')
            || value.contains("://")
            || value.contains(['?', '#', '@', '\\'])
        {
            return Err(InvalidExactDiscoveryTarget::InvalidSyntax);
        }

        if address.contains('%') {
            validate_rejected_scope(address)?;
            return Err(InvalidExactDiscoveryTarget::ScopedIpv6Unsupported);
        }

        let address = address
            .parse::<IpAddr>()
            .map_err(|_| InvalidExactDiscoveryTarget::InvalidSyntax)?;
        if bracketed && address.is_ipv4() {
            return Err(InvalidExactDiscoveryTarget::InvalidSyntax);
        }
        validate_address(address)?;

        Ok(Self { address })
    }
}

impl fmt::Debug for ExactDiscoveryTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactDiscoveryTarget")
            .field(
                "address_family",
                &if self.address.is_ipv4() {
                    "IPv4"
                } else {
                    "IPv6"
                },
            )
            .field("address", &"<redacted>")
            .finish()
    }
}

/// Fixed, topology-free reason an exact-address entry was rejected.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidExactDiscoveryTarget {
    #[error("enter a numeric IPv4 or IPv6 address")]
    Empty,

    #[error("device address is too long; maximum is {maximum} bytes")]
    TooLong { maximum: usize },

    #[error("device address contains a forbidden control character")]
    ControlCharacter,

    #[error("enter an address without a hostname, URL, CIDR range, or port")]
    InvalidSyntax,

    #[error("device address must be a usable unicast address")]
    UnicastRequired,

    #[error("IPv4-mapped IPv6 addresses are not supported; enter the IPv4 address directly")]
    Ipv4MappedIpv6Unsupported,

    #[error(
        "link-local IPv6 requires scoped device access, which is not supported yet; use IPv4 or unscoped IPv6"
    )]
    LinkLocalIpv6ScopeRequired,

    #[error("scoped IPv6 device access is not supported yet; use IPv4 or unscoped IPv6")]
    ScopedIpv6Unsupported,
}

fn strip_optional_brackets(value: &str) -> Result<(&str, bool), InvalidExactDiscoveryTarget> {
    match (value.strip_prefix('['), value.strip_suffix(']')) {
        (Some(without_start), Some(_)) => {
            let address = without_start
                .strip_suffix(']')
                .ok_or(InvalidExactDiscoveryTarget::InvalidSyntax)?;
            if address.is_empty() {
                return Err(InvalidExactDiscoveryTarget::InvalidSyntax);
            }
            Ok((address, true))
        }
        (None, None) => Ok((value, false)),
        _ => Err(InvalidExactDiscoveryTarget::InvalidSyntax),
    }
}

fn validate_rejected_scope(value: &str) -> Result<(), InvalidExactDiscoveryTarget> {
    let mut parts = value.split('%');
    let address = parts
        .next()
        .ok_or(InvalidExactDiscoveryTarget::InvalidSyntax)?;
    let scope = parts
        .next()
        .ok_or(InvalidExactDiscoveryTarget::InvalidSyntax)?;
    if parts.next().is_some() || address.parse::<Ipv6Addr>().is_err() || scope.is_empty() {
        return Err(InvalidExactDiscoveryTarget::InvalidSyntax);
    }
    if scope.bytes().all(|byte| byte.is_ascii_digit())
        && scope.parse::<u32>().ok().is_none_or(|scope| scope == 0)
    {
        return Err(InvalidExactDiscoveryTarget::InvalidSyntax);
    }
    Ok(())
}

fn validate_address(address: IpAddr) -> Result<(), InvalidExactDiscoveryTarget> {
    match address {
        IpAddr::V4(address) => {
            if address.is_unspecified()
                || address.is_loopback()
                || address.is_multicast()
                || address == Ipv4Addr::BROADCAST
            {
                return Err(InvalidExactDiscoveryTarget::UnicastRequired);
            }
        }
        IpAddr::V6(address) => {
            if address.to_ipv4_mapped().is_some() {
                return Err(InvalidExactDiscoveryTarget::Ipv4MappedIpv6Unsupported);
            }
            if address.is_unicast_link_local() {
                return Err(InvalidExactDiscoveryTarget::LinkLocalIpv6ScopeRequired);
            }
            if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
                return Err(InvalidExactDiscoveryTarget::UnicastRequired);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_canonicalizes_numeric_ipv4_and_ipv6() {
        let ipv4 = ExactDiscoveryTarget::parse("  10.20.30.40  ").unwrap();
        let ipv4_link_local = ExactDiscoveryTarget::parse("169.254.10.20").unwrap();
        let ipv6_ula = ExactDiscoveryTarget::parse("fd12:3456::40").unwrap();
        let ipv6_bracketed = ExactDiscoveryTarget::parse("[2001:db8::40]").unwrap();

        assert_eq!(ipv4.socket_addr(), "10.20.30.40:0".parse().unwrap());
        assert_eq!(ipv4.ip_addr(), "10.20.30.40".parse::<IpAddr>().unwrap());
        assert_eq!(
            ipv4_link_local.socket_addr(),
            "169.254.10.20:0".parse().unwrap()
        );
        assert_eq!(ipv6_ula.socket_addr(), "[fd12:3456::40]:0".parse().unwrap());
        assert_eq!(
            ipv6_ula.ip_addr(),
            "fd12:3456::40".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            ipv6_bracketed.socket_addr(),
            "[2001:db8::40]:0".parse().unwrap()
        );
        assert_eq!(
            "[2001:db8::40]".parse::<ExactDiscoveryTarget>(),
            Ok(ipv6_bracketed)
        );
    }

    #[test]
    fn rejects_non_unicast_and_ipv4_mapped_addresses() {
        for value in [
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "ff02::1",
        ] {
            assert_eq!(
                ExactDiscoveryTarget::parse(value),
                Err(InvalidExactDiscoveryTarget::UnicastRequired),
                "unexpected result for {value}"
            );
        }
        assert_eq!(
            ExactDiscoveryTarget::parse("::ffff:192.0.2.10"),
            Err(InvalidExactDiscoveryTarget::Ipv4MappedIpv6Unsupported)
        );
    }

    #[test]
    fn rejects_hostname_url_cidr_port_and_malformed_brackets() {
        for value in [
            "tuner.example",
            "http://192.0.2.10/",
            "192.0.2.10/32",
            "192.0.2.10:65001",
            "[2001:db8::10]:65001",
            "[192.0.2.10]",
            "[2001:db8::10",
            "2001:db8::10]",
            "[]",
        ] {
            assert_eq!(
                ExactDiscoveryTarget::parse(value),
                Err(InvalidExactDiscoveryTarget::InvalidSyntax),
                "unexpected result for {value}"
            );
        }
    }

    #[test]
    fn scoped_and_link_local_ipv6_fail_closed() {
        assert_eq!(
            ExactDiscoveryTarget::parse("fe80::40"),
            Err(InvalidExactDiscoveryTarget::LinkLocalIpv6ScopeRequired)
        );
        for value in [
            "fe80::40%12",
            "[fe80::40%12]",
            "fd12:3456::40%12",
            "fe80::40%wg0",
        ] {
            assert_eq!(
                ExactDiscoveryTarget::parse(value),
                Err(InvalidExactDiscoveryTarget::ScopedIpv6Unsupported),
                "unexpected result for {value}"
            );
        }
        for value in [
            "fe80::40%",
            "fe80::40%0",
            "fe80::40%4294967296",
            "fe80::40%12%13",
        ] {
            assert_eq!(
                ExactDiscoveryTarget::parse(value),
                Err(InvalidExactDiscoveryTarget::InvalidSyntax),
                "unexpected result for {value}"
            );
        }
    }

    #[test]
    fn input_size_and_control_characters_are_bounded_before_parsing() {
        assert_eq!(
            ExactDiscoveryTarget::parse("   "),
            Err(InvalidExactDiscoveryTarget::Empty)
        );
        assert_eq!(
            ExactDiscoveryTarget::parse("192.0.2.10\n"),
            Err(InvalidExactDiscoveryTarget::ControlCharacter)
        );
        assert_eq!(
            ExactDiscoveryTarget::parse(&"1".repeat(MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES + 1)),
            Err(InvalidExactDiscoveryTarget::TooLong {
                maximum: MAX_EXACT_DISCOVERY_TARGET_TEXT_BYTES
            })
        );
    }

    #[test]
    fn debug_and_errors_do_not_expose_entered_topology() {
        let sentinel = "198.51.100.247";
        let target = ExactDiscoveryTarget::parse(sentinel).unwrap();
        let debug = format!("{target:?}");

        assert_eq!(
            debug,
            "ExactDiscoveryTarget { address_family: \"IPv4\", address: \"<redacted>\" }"
        );
        assert!(!debug.contains(sentinel));

        let invalid = "secret-tuner.example";
        let error = ExactDiscoveryTarget::parse(invalid).unwrap_err();
        assert!(!format!("{error:?}").contains(invalid));
        assert!(!error.to_string().contains(invalid));
    }
}
