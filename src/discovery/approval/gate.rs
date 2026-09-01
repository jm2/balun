//! Fresh-snapshot revalidation for route-derived scan authority.
//!
//! This module is deliberately packet-free. It consumes a committed permit,
//! rebuilds the complete route-derived proposal from a caller-supplied fresh
//! snapshot, and replaces transient interface identities only after every
//! stable field still matches. The resulting capability is not a scanner and
//! cannot be converted back into the generic approved-target set.

use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

use thiserror::Error;

use super::{
    ResolvedRouteCandidate, ResolvedTunnelOrigin, RouteFingerprint, RouteFingerprintKey,
    RoutedPolicyTime, RoutedRunId, RoutedScanPermit, RoutedScanProposal, fingerprint,
};
use crate::discovery::{
    InterfaceId, ProbeConfig, RouteCandidateOrigin, RouteSnapshot, RoutedScanConfig,
    select_route_candidates,
};

/// One freshly resolved target and the exact tunnel interface it must use.
///
/// The later operating-system egress layer may use these explicit getters to
/// pin a socket. Default debug output remains topology-redacted.
pub(crate) struct RevalidatedRoutedTarget {
    address: Ipv4Addr,
    interface_id: InterfaceId,
    interface_name: String,
}

impl RevalidatedRoutedTarget {
    #[must_use]
    pub(crate) const fn address(&self) -> Ipv4Addr {
        self.address
    }

    #[must_use]
    pub(crate) const fn interface_id(&self) -> InterfaceId {
        self.interface_id
    }

    #[must_use]
    pub(crate) fn interface_name(&self) -> &str {
        &self.interface_name
    }
}

impl fmt::Debug for RevalidatedRoutedTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevalidatedRoutedTarget")
            .field("address", &"<redacted>")
            .field("interface_id", &"<redacted>")
            .field("interface_name", &"<redacted>")
            .finish()
    }
}

/// Single-use, freshly revalidated route-derived scan authority.
///
/// This value deliberately does not implement `Clone`. It contains no socket
/// or network entry point; a later platform layer must consume it, pin egress
/// to every target's fresh interface, and enforce both the effective deadline
/// and cancellation on subsequent network change or revocation. It must turn
/// `reservation_expires_at` into a monotonic deadline immediately before
/// starting; queueing delay after this snapshot must never extend the lease.
pub(crate) struct RevalidatedRoutedScan {
    fingerprint: RouteFingerprint,
    run_id: RoutedRunId,
    validated_at: RoutedPolicyTime,
    reservation_expires_at: RoutedPolicyTime,
    probe_config: ProbeConfig,
    effective_scan_config: RoutedScanConfig,
    targets: Vec<RevalidatedRoutedTarget>,
}

impl RevalidatedRoutedScan {
    #[must_use]
    pub(crate) const fn fingerprint(&self) -> RouteFingerprint {
        self.fingerprint
    }

    #[must_use]
    pub(crate) const fn run_id(&self) -> RoutedRunId {
        self.run_id
    }

    #[must_use]
    pub(crate) const fn validated_at(&self) -> RoutedPolicyTime {
        self.validated_at
    }

    #[must_use]
    pub(crate) const fn reservation_expires_at(&self) -> RoutedPolicyTime {
        self.reservation_expires_at
    }

    #[must_use]
    pub(crate) const fn probe_config(&self) -> ProbeConfig {
        self.probe_config
    }

    /// Return the approved scan policy with its deadline already capped to
    /// the remaining reservation lease at `validated_at`.
    ///
    /// The original uncapped configuration is deliberately unavailable from
    /// this capability, so a future runner cannot select the wrong budget. A
    /// delayed runner must still shorten this value again or reject the
    /// expired absolute reservation before opening a socket.
    #[must_use]
    pub(crate) const fn scan_config(&self) -> RoutedScanConfig {
        self.effective_scan_config
    }

    #[must_use]
    pub(crate) fn targets(&self) -> &[RevalidatedRoutedTarget] {
        &self.targets
    }
}

impl fmt::Debug for RevalidatedRoutedScan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevalidatedRoutedScan")
            .field("fingerprint", &self.fingerprint)
            .field("run_id", &self.run_id)
            .field("validated_at", &self.validated_at)
            .field("reservation_expires_at", &self.reservation_expires_at)
            .field(
                "effective_deadline",
                &self.effective_scan_config.overall_deadline(),
            )
            .field("target_count", &self.targets.len())
            .finish()
    }
}

/// Topology-redacted reason a committed permit could not be freshly validated.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum RoutedRevalidationError {
    #[error("the routed scan reservation has expired")]
    Expired,

    #[error("the routed scan permit is internally inconsistent")]
    InvalidPermit,

    #[error("fresh route-derived candidate selection was rejected")]
    FreshCandidateSelectionRejected,

    #[error("the fresh route-derived proposal was rejected")]
    FreshProposalRejected,

    #[error("the fresh route-derived plan no longer matches the approved plan")]
    PlanChanged,

    #[error("the route-derived approval fingerprint did not match")]
    FingerprintMismatch,
}

/// Consume a committed permit and revalidate it against one fresh snapshot.
///
/// Candidate selection and proposal construction are repeated with no explicit
/// ranges and with exactly the configs retained by the permit. Stable plan
/// equality ignores only the transient interface identifier. The output then
/// uses the fresh identifier, so an old ifindex/LUID can never reach a future
/// socket-pinning layer.
pub(crate) fn revalidate_routed_scan(
    permit: RoutedScanPermit,
    snapshot: &RouteSnapshot,
    key: &RouteFingerprintKey,
    now: RoutedPolicyTime,
) -> Result<RevalidatedRoutedScan, RoutedRevalidationError> {
    if now >= permit.expires_at {
        return Err(RoutedRevalidationError::Expired);
    }

    validate_permit_plan(&permit)?;
    let expected_fingerprint = fingerprint(
        key,
        &permit.resolved_candidates,
        permit.probe_config,
        permit.scan_config,
    );
    if expected_fingerprint != permit.fingerprint {
        return Err(RoutedRevalidationError::FingerprintMismatch);
    }

    let candidates = select_route_candidates(snapshot, &[])
        .map_err(|_| RoutedRevalidationError::FreshCandidateSelectionRejected)?;
    let fresh = RoutedScanProposal::from_route_candidates(
        snapshot,
        &candidates,
        key,
        permit.probe_config,
        permit.scan_config,
    )
    .map_err(|_| RoutedRevalidationError::FreshProposalRejected)?;

    if !stable_plans_match(&permit.resolved_candidates, &fresh.resolved_candidates)
        || !target_sets_match(&permit, &fresh)
    {
        return Err(RoutedRevalidationError::PlanChanged);
    }
    if fresh.fingerprint != permit.fingerprint {
        return Err(RoutedRevalidationError::FingerprintMismatch);
    }

    let remaining_lease = Duration::from_secs(
        permit
            .expires_at
            .as_seconds()
            .saturating_sub(now.as_seconds()),
    );
    let effective_deadline = permit.scan_config.overall_deadline().min(remaining_lease);
    let effective_scan_config = RoutedScanConfig::new_with_overall_deadline(
        permit.scan_config.wire_datagrams_per_second(),
        permit.scan_config.max_in_flight(),
        effective_deadline,
    )
    .map_err(|_| RoutedRevalidationError::InvalidPermit)?;
    let targets = fresh_targets(fresh.resolved_candidates)?;

    Ok(RevalidatedRoutedScan {
        fingerprint: permit.fingerprint,
        run_id: permit.run_id,
        validated_at: now,
        reservation_expires_at: permit.expires_at,
        probe_config: permit.probe_config,
        effective_scan_config,
        targets,
    })
}

fn validate_permit_plan(permit: &RoutedScanPermit) -> Result<(), RoutedRevalidationError> {
    if permit.targets.len() != permit.resolved_candidates.len()
        || !permit.targets.candidates().eq(permit
            .resolved_candidates
            .iter()
            .map(|candidate| candidate.address))
        || !plan_has_one_tunnel_origin_per_target(&permit.resolved_candidates)
    {
        return Err(RoutedRevalidationError::InvalidPermit);
    }
    Ok(())
}

fn target_sets_match(permit: &RoutedScanPermit, fresh: &RoutedScanProposal) -> bool {
    permit.targets.len() == fresh.targets.len()
        && permit.targets.candidates().eq(fresh.targets.candidates())
        && fresh.targets.candidates().eq(fresh
            .resolved_candidates
            .iter()
            .map(|candidate| candidate.address))
}

fn plan_has_one_tunnel_origin_per_target(candidates: &[ResolvedRouteCandidate]) -> bool {
    !candidates.is_empty()
        && candidates.iter().all(|candidate| {
            let [origin] = candidate.origins.as_slice() else {
                return false;
            };
            matches!(
                origin.original,
                RouteCandidateOrigin::TunnelRoute { network, .. }
                    if network.contains(&candidate.address)
            ) && !origin.interface_name.is_empty()
                && !origin.scopes.is_empty()
        })
}

fn stable_plans_match(
    approved: &[ResolvedRouteCandidate],
    fresh: &[ResolvedRouteCandidate],
) -> bool {
    plan_has_one_tunnel_origin_per_target(approved)
        && plan_has_one_tunnel_origin_per_target(fresh)
        && approved.len() == fresh.len()
        && approved.iter().zip(fresh).all(|(approved, fresh)| {
            approved.address == fresh.address
                && stable_origins_match(&approved.origins[0], &fresh.origins[0])
        })
}

fn stable_origins_match(approved: &ResolvedTunnelOrigin, fresh: &ResolvedTunnelOrigin) -> bool {
    let (
        RouteCandidateOrigin::TunnelRoute {
            network: approved_network,
            ..
        },
        RouteCandidateOrigin::TunnelRoute {
            network: fresh_network,
            ..
        },
    ) = (approved.original, fresh.original)
    else {
        return false;
    };

    approved_network == fresh_network
        && approved.interface_name == fresh.interface_name
        && approved.assigned_prefixes == fresh.assigned_prefixes
        && approved.scopes == fresh.scopes
}

fn fresh_targets(
    candidates: Vec<ResolvedRouteCandidate>,
) -> Result<Vec<RevalidatedRoutedTarget>, RoutedRevalidationError> {
    candidates
        .into_iter()
        .map(|candidate| {
            let [origin]: [ResolvedTunnelOrigin; 1] = candidate
                .origins
                .try_into()
                .map_err(|_| RoutedRevalidationError::FreshProposalRejected)?;
            let RouteCandidateOrigin::TunnelRoute { interface, .. } = origin.original else {
                return Err(RoutedRevalidationError::FreshProposalRejected);
            };
            if interface.get() == 0 {
                return Err(RoutedRevalidationError::FreshProposalRejected);
            }

            Ok(RevalidatedRoutedTarget {
                address: candidate.address,
                interface_id: interface,
                interface_name: origin.interface_name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use ipnet::IpNet;

    use super::super::{
        RoutedApprovalState, RoutedBeginDecision, RoutedPendingReservation, RoutedScanTrigger,
    };
    use super::*;
    use crate::discovery::{InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteScope};

    const START: RoutedPolicyTime = RoutedPolicyTime::from_seconds(100);

    trait AmbiguousIfClone<Marker> {
        fn marker() {}
    }

    impl<T: ?Sized> AmbiguousIfClone<()> for T {}

    struct ImplementsClone;

    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}

    #[test]
    fn routed_authority_capabilities_do_not_implement_clone() {
        let _ = <RevalidatedRoutedScan as AmbiguousIfClone<_>>::marker;
        let _ = <RoutedPendingReservation as AmbiguousIfClone<_>>::marker;
        let _ = <RoutedScanPermit as AmbiguousIfClone<_>>::marker;
    }

    fn ipnet(value: &str) -> IpNet {
        value.parse().expect("valid test network")
    }

    fn interface(
        id: u64,
        name: &str,
        kind: InterfaceKind,
        is_up: bool,
        addresses: &[&str],
    ) -> NetworkInterface {
        NetworkInterface::new(
            InterfaceId::new(id),
            name,
            kind,
            is_up,
            addresses.iter().copied().map(ipnet),
        )
    }

    fn route(id: u64, destination: &str, scope: RouteScope) -> NetworkRoute {
        NetworkRoute::effective(
            ipnet(destination),
            Some(InterfaceId::new(id)),
            RouteKind::Unicast,
            scope,
        )
    }

    fn blocker(destination: &str) -> NetworkRoute {
        NetworkRoute::effective(
            ipnet(destination),
            None,
            RouteKind::Other,
            RouteScope::Other,
        )
    }

    fn snapshot(
        id: u64,
        name: &str,
        assigned: &[&str],
        destination: &str,
        scope: RouteScope,
    ) -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![interface(id, name, InterfaceKind::Tunnel, true, assigned)],
            vec![route(id, destination, scope)],
        )
    }

    fn baseline_snapshot() -> RouteSnapshot {
        snapshot(
            7,
            "synthetic-tunnel-a",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        )
    }

    fn key(byte: u8) -> RouteFingerprintKey {
        RouteFingerprintKey::from_bytes([byte; 32])
    }

    fn permit_for(
        snapshot: &RouteSnapshot,
        key: &RouteFingerprintKey,
        probe_config: ProbeConfig,
        scan_config: RoutedScanConfig,
    ) -> RoutedScanPermit {
        let candidates = select_route_candidates(snapshot, &[]).expect("candidate selection");
        let proposal = RoutedScanProposal::from_route_candidates(
            snapshot,
            &candidates,
            key,
            probe_config,
            scan_config,
        )
        .expect("valid proposal");
        let approval = RoutedApprovalState::from_user_approval(&proposal, START, None);
        let pending = match approval.plan_begin(
            proposal,
            RoutedScanTrigger::ExplicitRefresh,
            START,
            RoutedRunId::from_counter(1),
        ) {
            RoutedBeginDecision::Pending(pending) => pending,
            other => panic!("expected pending reservation, got {other:?}"),
        };
        pending.into_test_persisted_parts().1
    }

    fn default_permit(snapshot: &RouteSnapshot, key: &RouteFingerprintKey) -> RoutedScanPermit {
        permit_for(
            snapshot,
            key,
            ProbeConfig::default(),
            RoutedScanConfig::default(),
        )
    }

    #[test]
    fn unchanged_snapshot_succeeds_with_exact_bound_plan_and_budget() {
        let snapshot = baseline_snapshot();
        let key = key(0x5a);
        let permit = default_permit(&snapshot, &key);
        let fingerprint = permit.fingerprint();
        let run_id = permit.run_id();

        let validated = revalidate_routed_scan(permit, &snapshot, &key, START).unwrap();

        assert_eq!(validated.fingerprint(), fingerprint);
        assert_eq!(validated.run_id(), run_id);
        assert_eq!(validated.validated_at(), START);
        assert_eq!(
            validated.reservation_expires_at(),
            RoutedPolicyTime::from_seconds(160)
        );
        assert_eq!(validated.probe_config(), ProbeConfig::default());
        assert_eq!(validated.scan_config(), RoutedScanConfig::default());
        assert_eq!(validated.targets().len(), 2);
        assert!(
            validated
                .targets()
                .iter()
                .all(|target| target.interface_id() == InterfaceId::new(7)
                    && target.interface_name() == "synthetic-tunnel-a")
        );
    }

    #[test]
    fn transient_interface_id_change_succeeds_and_uses_only_the_fresh_id() {
        let original = baseline_snapshot();
        let fresh = snapshot(
            77,
            "synthetic-tunnel-a",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        );
        let key = key(0x5a);
        let permit = default_permit(&original, &key);

        let validated = revalidate_routed_scan(permit, &fresh, &key, START).unwrap();

        assert!(
            validated
                .targets()
                .iter()
                .all(|target| target.interface_id() == InterfaceId::new(77))
        );
    }

    #[test]
    fn remaining_lease_caps_the_effective_deadline() {
        let snapshot = baseline_snapshot();
        let key = key(0x5a);
        let permit = default_permit(&snapshot, &key);

        let validated =
            revalidate_routed_scan(permit, &snapshot, &key, RoutedPolicyTime::from_seconds(159))
                .unwrap();

        assert_eq!(
            validated.scan_config().overall_deadline(),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn unspecified_fresh_interface_id_is_rejected_before_capability_output() {
        let original = baseline_snapshot();
        let fresh = snapshot(
            0,
            "synthetic-tunnel-a",
            &["10.250.0.2/32", "fd00:1234::2/128"],
            "172.31.90.8/30",
            RouteScope::OnLink,
        );
        let key = key(0x5a);

        assert_eq!(
            revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START)
                .unwrap_err(),
            RoutedRevalidationError::FreshProposalRejected
        );
    }

    #[test]
    fn target_name_prefix_and_scope_changes_all_fail_closed() {
        let original = baseline_snapshot();
        let changed = [
            snapshot(
                7,
                "synthetic-tunnel-renamed",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::OnLink,
            ),
            snapshot(
                7,
                "synthetic-tunnel-a",
                &["10.250.0.3/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::OnLink,
            ),
            snapshot(
                7,
                "synthetic-tunnel-a",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.90.8/30",
                RouteScope::ViaGateway,
            ),
            snapshot(
                7,
                "synthetic-tunnel-a",
                &["10.250.0.2/32", "fd00:1234::2/128"],
                "172.31.91.8/30",
                RouteScope::OnLink,
            ),
        ];
        let key = key(0x5a);

        for fresh in changed {
            assert!(
                revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START)
                    .is_err()
            );
        }
    }

    #[test]
    fn internally_changed_configs_cannot_reuse_the_original_fingerprint() {
        let snapshot = baseline_snapshot();
        let key = key(0x5a);
        let mut changed_probe = default_permit(&snapshot, &key);
        changed_probe.probe_config =
            ProbeConfig::new(1, Duration::from_millis(200), 256, 64).unwrap();
        let mut changed_scan = default_permit(&snapshot, &key);
        changed_scan.scan_config =
            RoutedScanConfig::new_with_overall_deadline(32, 8, Duration::from_secs(14)).unwrap();

        for permit in [changed_probe, changed_scan] {
            assert_eq!(
                revalidate_routed_scan(permit, &snapshot, &key, START).unwrap_err(),
                RoutedRevalidationError::FingerprintMismatch
            );
        }
    }

    #[test]
    fn route_moving_from_tunnel_a_to_tunnel_b_fails_closed() {
        let original = baseline_snapshot();
        let fresh = RouteSnapshot::from_effective_routes(
            vec![
                interface(
                    7,
                    "synthetic-tunnel-a",
                    InterfaceKind::Tunnel,
                    true,
                    &["10.250.0.2/32"],
                ),
                interface(
                    8,
                    "synthetic-tunnel-b",
                    InterfaceKind::Tunnel,
                    true,
                    &["10.251.0.2/32"],
                ),
            ],
            vec![route(8, "172.31.90.8/30", RouteScope::OnLink)],
        );
        let key = key(0x5a);

        assert!(
            revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START).is_err()
        );
    }

    #[test]
    fn tunnel_becoming_lan_down_or_another_kind_fails_closed() {
        let original = baseline_snapshot();
        let changed_interfaces = [
            interface(
                7,
                "synthetic-tunnel-a",
                InterfaceKind::Other,
                true,
                &["10.250.0.2/32"],
            ),
            interface(
                7,
                "synthetic-tunnel-a",
                InterfaceKind::Tunnel,
                false,
                &["10.250.0.2/32"],
            ),
            interface(
                7,
                "synthetic-tunnel-a",
                InterfaceKind::Loopback,
                true,
                &["10.250.0.2/32"],
            ),
        ];
        let key = key(0x5a);

        for changed_interface in changed_interfaces {
            let fresh = RouteSnapshot::from_effective_routes(
                vec![changed_interface],
                vec![route(7, "172.31.90.8/30", RouteScope::OnLink)],
            );
            assert!(
                revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START)
                    .is_err()
            );
        }
    }

    #[test]
    fn a_new_more_specific_blocker_fails_closed() {
        let original = baseline_snapshot();
        let fresh = RouteSnapshot::from_effective_routes(
            vec![interface(
                7,
                "synthetic-tunnel-a",
                InterfaceKind::Tunnel,
                true,
                &["10.250.0.2/32", "fd00:1234::2/128"],
            )],
            vec![
                route(7, "172.31.90.8/30", RouteScope::OnLink),
                blocker("172.31.90.9/32"),
            ],
        );
        let key = key(0x5a);

        assert!(
            revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START).is_err()
        );
    }

    #[test]
    fn fresh_ecmp_is_rejected_instead_of_selecting_one_origin() {
        let original = baseline_snapshot();
        let fresh = RouteSnapshot::from_effective_routes(
            vec![
                interface(
                    7,
                    "synthetic-tunnel-a",
                    InterfaceKind::Tunnel,
                    true,
                    &["10.250.0.2/32"],
                ),
                interface(
                    8,
                    "synthetic-tunnel-b",
                    InterfaceKind::Tunnel,
                    true,
                    &["10.251.0.2/32"],
                ),
            ],
            vec![
                route(7, "172.31.90.8/30", RouteScope::OnLink),
                route(8, "172.31.90.8/30", RouteScope::OnLink),
            ],
        );
        let key = key(0x5a);

        assert_eq!(
            revalidate_routed_scan(default_permit(&original, &key), &fresh, &key, START)
                .unwrap_err(),
            RoutedRevalidationError::FreshProposalRejected
        );
    }

    #[test]
    fn added_and_missing_targets_both_fail_closed() {
        let narrow = snapshot(
            7,
            "synthetic-tunnel-a",
            &["10.250.0.2/32"],
            "172.31.90.9/32",
            RouteScope::OnLink,
        );
        let broad = baseline_snapshot();
        let key = key(0x5a);

        assert!(
            revalidate_routed_scan(default_permit(&narrow, &key), &broad, &key, START).is_err()
        );
        assert!(
            revalidate_routed_scan(default_permit(&broad, &key), &narrow, &key, START).is_err()
        );
    }

    #[test]
    fn wrong_installation_key_and_expired_permits_fail_closed() {
        let snapshot = baseline_snapshot();
        let approved_key = key(0x5a);
        let wrong_key = key(0xa5);

        assert_eq!(
            revalidate_routed_scan(
                default_permit(&snapshot, &approved_key),
                &snapshot,
                &wrong_key,
                START,
            )
            .unwrap_err(),
            RoutedRevalidationError::FingerprintMismatch
        );
        for now in [160, 161] {
            assert_eq!(
                revalidate_routed_scan(
                    default_permit(&snapshot, &approved_key),
                    &snapshot,
                    &approved_key,
                    RoutedPolicyTime::from_seconds(now),
                )
                .unwrap_err(),
                RoutedRevalidationError::Expired
            );
        }
    }

    #[test]
    fn debug_output_redacts_fresh_targets_and_bindings() {
        let snapshot = baseline_snapshot();
        let key = key(0x5a);
        let validated =
            revalidate_routed_scan(default_permit(&snapshot, &key), &snapshot, &key, START)
                .unwrap();

        let outputs = [
            format!("{validated:?}"),
            format!("{:?}", validated.targets()),
            format!("{:?}", RoutedRevalidationError::PlanChanged),
        ];
        for output in outputs {
            assert!(!output.contains("synthetic-tunnel-a"));
            assert!(!output.contains("172.31.90"));
            assert!(!output.contains("10.250"));
            assert!(!output.contains("fd00:1234"));
        }
    }

    #[test]
    fn target_getter_returns_the_expected_synthetic_addresses() {
        let snapshot = baseline_snapshot();
        let key = key(0x5a);
        let validated =
            revalidate_routed_scan(default_permit(&snapshot, &key), &snapshot, &key, START)
                .unwrap();

        assert_eq!(
            validated
                .targets()
                .iter()
                .map(RevalidatedRoutedTarget::address)
                .collect::<Vec<_>>(),
            vec![
                Ipv4Addr::new(172, 31, 90, 9),
                Ipv4Addr::new(172, 31, 90, 10),
            ]
        );
    }
}
