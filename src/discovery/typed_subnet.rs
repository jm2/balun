//! Side-effect-free scope and budget for the approved typed-subnet contract.
//!
//! This value carries no consent or network-observation authority. A search
//! needs a [`super::SubnetSearchConsent`] for exactly this scope, admitted
//! against a healthy observation generation.

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;

use ipnet::Ipv4Net;
use thiserror::Error;

/// A canonical RFC 1918 IPv4 scope, validated without granting permission to send.
///
/// A search must separately bind fresh per-search consent and a healthy
/// observation generation to this exact scope and policy. Remembering its text
/// is a preference only. Construction performs no I/O or address resolution.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TypedSubnetScope {
    network: Ipv4Net,
}

impl TypedSubnetScope {
    /// Distinguishes typed-scope budgets from route-derived or exact-target authority.
    pub const POLICY_ID: &str = "typed-subnet-rfc1918-v1";
    /// Longest canonical IPv4 CIDR text, including a two-digit prefix length.
    pub const MAX_TEXT_BYTES: usize = 18;
    /// The approved /23 usable-host ceiling; network/broadcast endpoints are excluded.
    pub const MAX_CANDIDATES: usize = 510;
    /// Initial request plus one retry; both consume the outbound attempt budget.
    pub const ATTEMPTS_PER_CANDIDATE: usize = 2;
    /// Admission ceiling for concurrent targeted probes in the single subnet lane.
    pub const MAX_IN_FLIGHT: usize = 16;
    /// Every outbound attempt, including a retry, must obey this minimum spacing.
    pub const MIN_SEND_INTERVAL: Duration = Duration::from_nanos(15_625_000);
    /// Nonnegative jitter may add up to 25% of the minimum spacing.
    pub const MAX_SEND_JITTER: Duration = Duration::from_nanos(3_906_250);
    /// Per-attempt reply window, also bounded by the overall scan deadline.
    pub const REPLY_WINDOW: Duration = Duration::from_millis(200);
    /// Total receive budget for one candidate across its attempts.
    pub const MAX_RECEIVED_PER_CANDIDATE: usize = 16;
    /// A candidate may contribute no more than one accepted device identity.
    pub const MAX_IDENTITIES_PER_CANDIDATE: usize = 1;
    /// Reaching this distinct-device limit must report incomplete work.
    pub const MAX_DEVICES: usize = 64;
    /// UDP scan deadline including pacing and replies, separate from enrichment.
    pub const DEADLINE: Duration = Duration::from_secs(30);

    /// Return the exact canonical network entered by the user.
    #[must_use]
    pub const fn network(self) -> Ipv4Net {
        self.network
    }

    /// Enumerate usable hosts; /31 has both addresses and /32 has its one address.
    pub fn candidates(self) -> impl Iterator<Item = Ipv4Addr> {
        self.network.hosts()
    }

    /// Derive the preview count from the same host rule used by enumeration.
    #[must_use]
    pub fn candidate_count(self) -> usize {
        self.candidates().count()
    }

    /// Bound Balun's outbound attempts, not downstream deliveries or recipients.
    #[must_use]
    pub fn maximum_request_attempts(self) -> usize {
        self.candidate_count() * Self::ATTEMPTS_PER_CANDIDATE
    }
}

/// Closed validation reasons never contain the rejected input or an endpoint.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidTypedSubnetScope {
    #[error("enter one canonical IPv4 CIDR subnet")]
    InvalidSyntax,
    #[error("enter the network address without host bits or alternate spelling")]
    Noncanonical,
    #[error("enter a subnet from /23 through /32")]
    TooWide,
    #[error("enter a subnet wholly within RFC 1918 private address space")]
    NotPrivate,
}

impl FromStr for TypedSubnetScope {
    type Err = InvalidTypedSubnetScope;

    /// Reject scope changes rather than trimming, widening, or masking entered text.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.len() > Self::MAX_TEXT_BYTES {
            return Err(InvalidTypedSubnetScope::InvalidSyntax);
        }
        let network = input
            .parse::<Ipv4Net>()
            .map_err(|_| InvalidTypedSubnetScope::InvalidSyntax)?;
        if network != network.trunc() || input != network.to_string() {
            return Err(InvalidTypedSubnetScope::Noncanonical);
        }
        if network.prefix_len() < 23 {
            return Err(InvalidTypedSubnetScope::TooWide);
        }
        if !network.network().is_private() || !network.broadcast().is_private() {
            return Err(InvalidTypedSubnetScope::NotPrivate);
        }
        Ok(Self { network })
    }
}

impl fmt::Display for TypedSubnetScope {
    /// Canonical text is available for the explicit scope preview and preference.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.network.fmt(formatter)
    }
}

impl fmt::Debug for TypedSubnetScope {
    /// Accidental diagnostic formatting does not disclose a private network prefix.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TypedSubnetScope([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Approved endpoint rules drive both actual candidates and preview budgets.
    #[test]
    fn exact_scope_and_preview_match_the_approved_prefix_boundaries() {
        for (text, count, requests, first, last) in [
            ("192.168.2.0/23", 510, 1020, "192.168.2.1", "192.168.3.254"),
            (
                "10.255.255.0/24",
                254,
                508,
                "10.255.255.1",
                "10.255.255.254",
            ),
            (
                "172.31.255.252/30",
                2,
                4,
                "172.31.255.253",
                "172.31.255.254",
            ),
            ("172.16.0.0/31", 2, 4, "172.16.0.0", "172.16.0.1"),
            (
                "192.168.255.255/32",
                1,
                2,
                "192.168.255.255",
                "192.168.255.255",
            ),
        ] {
            let scope: TypedSubnetScope = text.parse().unwrap();
            let candidates = scope.candidates().collect::<Vec<_>>();
            assert_eq!(scope.to_string(), text);
            assert_eq!(scope.candidate_count(), count);
            assert_eq!(scope.maximum_request_attempts(), requests);
            assert_eq!(candidates.len(), count);
            assert_eq!(candidates[0].to_string(), first);
            assert_eq!(candidates.last().unwrap().to_string(), last);
            assert!(
                candidates
                    .windows(2)
                    .all(|pair| u32::from(pair[1]) == u32::from(pair[0]) + 1)
            );
        }
    }

    /// Caller text cannot be normalized into a different or nonprivate scan scope.
    #[test]
    fn malformed_noncanonical_and_disallowed_scopes_reject() {
        for input in [
            "",
            "10.0.0.1",
            "10.0.0.0/33",
            "10.0.0.0/-1",
            "10.0.0.0/+23",
            "10.0.0.0/023",
            "010.0.0.0/23",
            "10.0.0.1/23",
            "10.0.1.0/23",
            "10.0.0.0/23 ",
            " 10.0.0.0/23",
            "10.0.0.0/23\n",
            "10.0.0.0/23\0",
            "10.0.0.0/23,10.0.2.0/23",
            "10.0.0.0/22",
            "0.0.0.0/0",
            "172.15.254.0/23",
            "172.32.0.0/23",
            "192.167.254.0/23",
            "192.169.0.0/23",
            "127.0.0.0/24",
            "169.254.0.0/23",
            "224.0.0.0/23",
            "100.64.0.0/23",
            "192.0.2.0/24",
            "255.255.255.255/32",
            "::1/128",
            "fd00::/120",
            "::ffff:10.0.0.0/120",
            "https://private.invalid/23",
            "tuner.local/23",
        ] {
            assert!(input.parse::<TypedSubnetScope>().is_err(), "{input:?}");
        }
        assert_eq!(
            "10.0.0.1/24".parse::<TypedSubnetScope>(),
            Err(InvalidTypedSubnetScope::Noncanonical)
        );
        assert_eq!(
            "10.0.0.0/22".parse::<TypedSubnetScope>(),
            Err(InvalidTypedSubnetScope::TooWide)
        );
        assert_eq!(
            "192.0.2.0/24".parse::<TypedSubnetScope>(),
            Err(InvalidTypedSubnetScope::NotPrivate)
        );
    }

    /// Every accepted prefix stays bounded at both ends of each private block.
    #[test]
    fn each_private_edge_has_exact_contained_bounded_candidates() {
        for address in [
            "10.0.0.0",
            "10.255.255.255",
            "172.16.0.0",
            "172.31.255.255",
            "192.168.0.0",
            "192.168.255.255",
        ] {
            let address = address.parse::<Ipv4Addr>().unwrap();
            for prefix in 23..=32 {
                let network = Ipv4Net::new(address, prefix).unwrap().trunc();
                let scope: TypedSubnetScope = network.to_string().parse().unwrap();
                let candidates = scope.candidates().collect::<Vec<_>>();
                assert!(
                    !candidates.is_empty() && candidates.len() <= TypedSubnetScope::MAX_CANDIDATES
                );
                assert_eq!(scope.network(), network);
                assert!(
                    candidates
                        .iter()
                        .all(|ip| ip.is_private() && network.contains(ip))
                );
                assert!(scope.maximum_request_attempts() <= 1020);
            }
        }
    }

    /// The fixed maximum pacing still leaves room for replies within the deadline.
    #[test]
    fn typed_budget_fits_the_approved_deadline() {
        let scope: TypedSubnetScope = "10.0.0.0/23".parse().unwrap();
        let attempts = u32::try_from(scope.maximum_request_attempts()).unwrap();
        let worst_spacing = TypedSubnetScope::MIN_SEND_INTERVAL + TypedSubnetScope::MAX_SEND_JITTER;
        assert_eq!(
            TypedSubnetScope::MIN_SEND_INTERVAL * 64,
            Duration::from_secs(1)
        );
        assert_eq!(
            TypedSubnetScope::MAX_SEND_JITTER * 4,
            TypedSubnetScope::MIN_SEND_INTERVAL
        );
        assert!(
            worst_spacing * attempts + TypedSubnetScope::REPLY_WINDOW < TypedSubnetScope::DEADLINE
        );
        assert_eq!(TypedSubnetScope::MAX_IN_FLIGHT, 16);
        assert_eq!(TypedSubnetScope::MAX_RECEIVED_PER_CANDIDATE, 16);
        assert_eq!(TypedSubnetScope::MAX_IDENTITIES_PER_CANDIDATE, 1);
        assert_eq!(TypedSubnetScope::MAX_DEVICES, 64);
    }

    /// Failed input and incidental Debug formatting remain free of entered values.
    #[test]
    fn diagnostic_formatting_cannot_echo_scope_or_rejected_input() {
        let input = "secret-shaped-marker";
        let error = input.parse::<TypedSubnetScope>().unwrap_err();
        assert!(!format!("{error}: {error:?}").contains(input));
        let scope: TypedSubnetScope = "10.73.82.0/23".parse().unwrap();
        assert_eq!(format!("{scope:?}"), "TypedSubnetScope([redacted])");
        assert_eq!(scope.to_string(), "10.73.82.0/23");
    }
}
