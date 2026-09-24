# ADR-0003: Retire route-table-derived tunnel discovery

- Status: Accepted
- Date: 2026-09-24
- Ledger: V2.9 ([#181](https://github.com/jm2/balun/issues/181))

## Context

[ADR-0002](adr-0002-scope-and-diagnostics.md) kept Balun's routed-discovery
authority and completed it for Linux in P2. v0.1.0 and v0.1.1 shipped it:
rtnetlink route snapshots, candidates from active private tunnel routes, a
keyed, topology-redacted approval store in the private profile, a fresh-route
gate, interface-pinned sockets, and observers that revoked authority when routes
changed. P2.5 validated it across the owner's two sites.

It was also Balun's largest subsystem, about 25,000 lines of Rust, nearly half
of them tests, and it stayed Linux-only: macOS and Windows never gained a safe
route-table provider. Keeping it correct needed a steady run of fixes to
authority observers, IPv6 address refreshes, and revocation during a run.

The maintainer-approved [typed-subnet contract](../typed-subnet-discovery-proposal.md)
of 2026-09-18 (V2.4) serves the same case on every platform. The user types one
private prefix, confirms its request budget before each search, and a live
network-change source guards the search. It needs no route-table inference, no
remembered authority, and no per-platform route provider. Keeping both would
leave two scan-authority contracts, two consent models, and a Linux-only
desktop path.

## Decision

1. **Remove route-table-derived tunnel discovery on every platform.** This covers
   route snapshot providers and candidate selection, the approval policy and
   its durable store, the fresh-route gate and pinned routed runner, the
   controller's routed lane, the sidebar's **Search routes behind your tunnel**
   and **Forget routed approvals** controls, and `balun-discover --providers`.
2. **Typed-subnet search replaces it.** V2.4 is built as a desktop feature on
   every platform after H0.1 and V2.8, which adds native network-change
   observation on macOS and Windows.
3. **Until V2.4 lands,** remote tuners are added by exact IP address or
   hostname, which are remembered and probed again at launch, or found with
   `balun-discover --approved-range`, which is unchanged.
4. **Keep what V2.4 and network-change handling need.** The approved-range
   scanner stays in `src/discovery/routed.rs` under its existing names, which
   the unchanged typed-subnet validation imports. The Linux rtnetlink monitor
   that drives network-change reconciliation stays at
   `discovery::routes::linux::monitor` until V2.8 lands.
5. **Persisted data.** `settings.json` never held routed state, so its schema
   is unchanged. The retired `routed-approvals/` directory in the profile is
   left untouched; see the [settings boundary](../settings-file-trust.md).

## Consequences

- Linux loses one-click tunnel search until V2.4 lands. Remembered exact and
  hostname targets, already the macOS and Windows path, cover the owner's
  two-site deployment.
- Network-change reconciliation no longer cancels routed scans or expires
  routed evidence. Local and exact discovery behave as before.
- ADR-0001's route-derived discovery section and ADR-0002's decision 3 are
  superseded. The P2 ledger records and the P2.5 and P4.2 evidence remain as
  history.
- The route-budget and approval-sequence adversarial properties, and the
  removed files' coverage baselines, leave the ratchets with the code.
- A downgrade to v0.1.x finds its approval store intact.

## Rejected alternatives

- **Keeping routed discovery beside typed subnets** preserves two authority
  and consent models, with one of them Linux-only.
- **Porting route providers to macOS and Windows** needs a safe route-table
  wrapper, a provable routing domain, and a stable tunnel identity. The v0.1
  plan found none of them.
- **Deleting the orphan approval store at startup** adds a deletion path to the
  private profile for little benefit and breaks downgrades.
- **Hiding the code behind a feature flag** keeps its maintenance and coverage
  cost without a user.

## Revisit conditions

Revisit if typed-subnet search cannot serve tunnelled deployments, for example
because users cannot know the remote prefix, or if a platform gains a provable
routing-domain API that makes route-derived candidates cheap to verify.
