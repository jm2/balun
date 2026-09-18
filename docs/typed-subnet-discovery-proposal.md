# Typed subnet discovery: proposed contract

Status: awaiting maintainer approval for V2.4 and [issue #71]. This document
does not expand current scan authority. Implementation and completion stay
on hold until the traffic and consent decisions below are accepted.

The existing CLI accepts one explicitly approved private `/24` or narrower
range. The Linux route-derived proposal has a 256-candidate ceiling and a
15-second default deadline. Both remain the current implementation baseline.

## Assessment and proposed traffic limits

A `/23` contains 512 addresses, but the existing `Ipv4Net::hosts()` rule excludes
its network and broadcast addresses. The correct expanded maximum is therefore
**510 candidates and 1,020 discovery requests** at two attempts per candidate.
The issue's 512/1,024 values are upper ceilings, not the actual `/23` preview.
For `/31`, both addresses are candidates; a `/32` has one candidate.

The proposed typed-subnet contract is shared by the desktop preview, runner,
CLI `--approved-range`, persistence, and tests:

| Property | Proposed boundary |
| --- | --- |
| Scope | One canonical IPv4 CIDR, `/23` through `/32`, wholly inside RFC 1918 space |
| Candidates | Exact usable-host expansion, at most 510; no network/broadcast destinations for `/30` and wider |
| Requests | At most two targeted HDHomeRun UDP discovery requests per candidate, to the fixed discovery port |
| Request budget | Exact candidate count times two, at most 1,020; retries consume this same budget |
| Wire pacing | At least 15.625 ms between request attempts, including retries; nonnegative jitter may add at most 25% |
| Concurrency | At most 16 targeted probes in flight; one typed or route-derived subnet scan per application process |
| Reply window | At most 200 ms per request attempt |
| Receive budget | At most 16 received datagrams and one accepted device identity per candidate |
| Result budget | At most 64 distinct accepted devices; reaching the limit reports an incomplete result |
| Scan deadline | 30 seconds for the UDP scan, including pacing and reply waits; no new request after expiry |
| Cancellation | Cancel on the user's request, close, or a detected network change; cancel and join all admitted probes |

Reject host bits in a typed network rather than silently changing its scope.
Reject public, loopback, link-local, multicast, IPv6, and wider ranges before
any socket or persistence work. The preview derives its exact candidate and
request counts from the same validated policy value used by the runner.

At maximum jitter, pacing 1,020 requests needs less than 20 seconds, leaving
room inside the 30-second deadline for the last reply window and scheduling.
This is a budget calculation, not a promise that every host will answer or that
a stalled OS operation can be interrupted. Deadline expiry reports incomplete
work; it must never be represented as a completed empty search.

The current runner spaces candidate starts using attempts per target. That is a
nominal request rate, not an independently enforced wire limit for every retry.
The new lane must enforce the proposed pacing at the actual send boundary and
recheck cancellation, scope, remaining budget, and deadline after readiness and
before each nonblocking send attempt. A successor cannot inherit an old permit.
Its pacer must not reset into a burst on repeated explicit searches.

Metadata enrichment is separate from UDP scanning and applies only to validated
responders. Retain its existing HTTP origin, address, parser, and concurrency
limits. Do not fetch HTTP metadata for nonresponders or infer a device from a
successful connection alone. Opening a stream or allocating a tuner is outside
subnet discovery.

## Consent decision

The proposed default remembers the typed prefix as a convenience but requires
confirmation of the exact scope and packet budget for **every** search. There
are no automatic scans on startup, timers, network changes, or profile loading.
The CLI's explicit `--approved-range` argument is that invocation's consent;
an ordinary launch never replays a previous command's authority.

The desktop confirmation identifies the scope as user-entered and explains that
the system's current network routing selects the path. It makes no assertion
that a tunnel terminates on this host or that the addresses identify the same
physical network as an earlier run. Cancel and incomplete-result status remain
visible throughout the scan.

Issue #71 originally proposed remembering approval once per prefix. A maintainer
may instead choose that behavior: persist authorization for this exact canonical
prefix and policy version, while still requiring an explicit Search action for
every run. That alternative intentionally carries the approval across restarts
and changes of the connected network. A policy/budget change or Forget action
invalidates it. A detected network change still cancels an active scan.
This broader remembered-authority boundary requires a separate explicit choice;
remembering the text alone does not grant it.

Store only user-entered prefix preferences and, if approved, the exact bounded
authorization in the accepted private profile. Use a separate typed-scope policy
identity so neither route-derived nor exact-address approvals can authorize it.
Unknown/newer state remains preserved and grants no new authority. Persistence
failure must not silently create remembered approval.

## Integration and acceptance

Keep Linux route-derived admission at its current limits and retain interface
pinning, topology fingerprints, durable reservation ownership, and the H0.1
post-readiness checks. Increasing a shared global constant would weaken that
separate contract and is not the proposed implementation.

The ordinary cross-platform socket path is appropriate only for the explicit
typed range. Results retain their typed-search origin, and the registry's device
identity and network-change reconciliation continue to apply. No hostname, URL,
route guess, or stored device endpoint can silently become subnet authority.

Required evidence before V2.4 can be completed:

- One policy value drives `/23`, `/24`, `/31`, and `/32` preview, CLI, requests,
  result accounting, consent identity, and persistence. Reject boundary-crossing,
  noncanonical, oversized, and nonprivate input before side effects.
- Deterministic scheduler/send fixtures prove exact wire budgets, retry pacing,
  jitter bounds, the 16-probe cap, deadline expiry, result limits, and cancellation
  after readiness. Partial results never become a completed empty run.
- Repeated activation, stale completion, revocation, close, policy changes,
  unavailable persistence, and network changes cannot authorize extra sends or
  leave unjoined workers. Repeated searches share the same pacing boundary.
- Run the native socket/cancellation fixtures on Linux, macOS, and Windows.
  Test fixtures use the already accepted library loopback boundary; shipped
  input validation continues to reject loopback scopes.
- Exercise preview, confirmation, progress, Cancel, Forget, and error focus with
  keyboard and translated accessibility copy as the UI integration lands.

Approval of this proposal permits implementation, not a completion checkbox.
The maintainer owns the scope and remembered-authority decision; revisit the
contract before beta or any increase in scope, rate, retries, or automatic work.

[issue #71]: https://github.com/jm2/balun/issues/71
