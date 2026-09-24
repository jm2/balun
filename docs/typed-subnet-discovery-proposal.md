# Typed subnet discovery: approved contract

Status: approved by jm2 on 2026-09-18 for V2.4 and [issue #71], selecting
the `/23` traffic limits (1A) and confirmation before every search (2A).
The maintainer also accepted the [outbound-request boundary](#approved-delivery-boundary)
(option A) on 2026-09-18. Implementation may proceed under this contract.
This documentation change does not expand current scan behavior or complete V2.4.

The [authoritative plan](plan-v0.1.md#5-discovery-policy) and
[ADR-0001 amendment](architecture/adr-0001-discovery-playback.md#typed-subnet-amendment--2026-09-18)
adopt this separate typed-scope policy. Their former route-derived treatment of
user-entered ranges is superseded. Route-derived discovery itself was retired on
2026-09-24 ([ADR-0003](architecture/adr-0003-retire-route-derived-discovery.md)).

The existing CLI accepts one explicitly approved private `/24` or narrower
range, with a 256-candidate ceiling and a 15-second default deadline. That
remains the current implementation baseline.

The first implementation slice is the side-effect-free `TypedSubnetScope` library
value. It accepts only canonical private `/23`–`/32` text, supplies usable-host
enumeration and exact candidate/outbound-attempt counts, and names the fixed
typed-policy limits below. It carries no consent, observation generation, or
route-derived authority. Neither shipped application admits scans with this value
yet; socket execution, observer health, consent, persistence, pacing, and UI/CLI
integration remain pending. Diagnostic formatting redacts the scope; explicit
display formatting retains canonical text for a future preview or preference.

Portable fixtures cover each prefix at both ends of all three RFC 1918 blocks,
`/23`, `/24`, `/30`, `/31`, and `/32` preview counts, noncanonical/invalid/private
boundaries, diagnostic redaction, and the separate unchanged approved-range limits.
The maximum-jitter budget fits the approved deadline arithmetically; it does not
establish actual send pacing or completion timing, which require runner evidence.

## Assessment and approved traffic limits

A `/23` contains 512 addresses, but the existing `Ipv4Net::hosts()` rule excludes
its network and broadcast addresses. The correct expanded maximum is therefore
**510 candidates and 1,020 discovery requests** at two attempts per candidate.
The network and broadcast exclusions account for the difference.
For `/31`, both addresses are candidates; a `/32` has one candidate.

The approved typed-subnet contract must be shared by the desktop preview, runner,
CLI `--approved-range`, persistence, and tests:

| Property | Approved boundary |
| --- | --- |
| Scope | One canonical IPv4 CIDR, `/23` through `/32`, wholly inside RFC 1918 space |
| Candidates | Exact usable-host expansion, at most 510; exclude the entered CIDR's network/broadcast endpoints for `/30` and wider |
| Requests | At most two outbound HDHomeRun UDP discovery requests per candidate, to the fixed discovery port |
| Request budget | Exact candidate count times two, at most 1,020 outbound attempts; retries consume this same budget |
| Outbound pacing | At least 15.625 ms between request attempts, including retries; nonnegative jitter may add at most 25% |
| Concurrency | At most 16 targeted probes in flight; one subnet scan per application process |
| Reply window | At most 200 ms per request attempt |
| Receive budget | At most 16 received datagrams and one accepted device identity per candidate |
| Result budget | At most 64 distinct accepted devices; reaching the limit reports an incomplete result |
| Scan deadline | 30 seconds for the UDP scan, including pacing and reply waits; no new request after expiry |
| Network observation | A healthy, live network-change source is required before admission and throughout the scan |
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
The new lane must enforce the approved pacing at the actual send boundary and
recheck cancellation, scope, remaining budget, and deadline after readiness and
before each nonblocking send attempt. A successor cannot inherit an old permit.
Its pacer must not reset into a burst on repeated explicit searches.

Metadata enrichment is separate from UDP scanning and applies only to validated
responders. Retain its existing HTTP origin, address, parser, and concurrency
limits. Do not fetch HTTP metadata for nonresponders or infer a device from a
successful connection alone. Opening a stream or allocating a tuner is outside
subnet discovery.

## Approved delivery boundary

Review identified a distinction between an address inside the entered CIDR
and a unicast destination under the actual downstream subnetting. An interior
address can be a directed-broadcast address for a narrower downstream subnet.
`Ipv4Net::hosts()` excludes the entered CIDR's endpoints; it cannot establish
how another router will deliver every remaining address.
[RFC 2644](https://www.rfc-editor.org/rfc/rfc2644.html) requires directed-broadcast
receipt and forwarding to be disabled by default, but allows operators to enable
them. Balun cannot infer that configuration from the typed prefix.

On 2026-09-18, jm2 accepted **option A: bound application requests and trust
downstream broadcast blocking**. Keep the approved scope and numeric limits,
with the request budget explicitly counting Balun's outbound attempts. Keep
socket broadcast disabled and reject destinations the local OS classifies as
broadcasts; never retry a denied send with broadcast enabled.

The supported deployment requires downstream routers to keep directed-broadcast
forwarding disabled. Balun cannot verify that remote setting and cannot promise
single-host delivery if a router is configured otherwise. The preview must
describe an outbound request budget, not a bound on downstream deliveries or
recipients. The same limit applies to CLI and desktop searches.

Policy owner: jm2. Revisit before beta, changes to routing assumptions, or
evidence of unexpected broadcast delivery. Per-search confirmation remains
required; this disposition grants no automatic or remembered scan authority.

## Approved consent decision

The application remembers the typed prefix as a convenience but requires
confirmation of the exact scope and packet budget for **every** search. There
are no automatic scans on startup, timers, network changes, or profile loading.
The CLI's explicit `--approved-range` argument is that invocation's consent;
an ordinary launch never replays a previous command's authority.

The desktop confirmation identifies the scope as user-entered and explains that
the system's current network routing selects the path. It makes no assertion
that a tunnel terminates on this host or that the addresses identify the same
physical network as an earlier run. Cancel and incomplete-result status remain
visible throughout the scan.

Each confirmation authorizes only that search. Remembering approval once per
prefix, as originally proposed in issue #71, was not selected. A later search
requires fresh confirmation, including after restarts or network changes.
A detected network change still cancels an active scan. Forget removes the
stored prefix preference; remembering or restoring that text grants no authority.

Store only user-entered prefix preferences in the accepted private profile;
do not persist typed-subnet authorization. Use a separate typed-scope policy
identity so exact-address approvals cannot authorize it.
Unknown/newer state remains preserved and grants no new authority. Persistence
failure must not bypass confirmation for the current or a later search.

## Network-change admission

Every typed-scan entry point, including the CLI, must establish a working
network-change source before accepting consent or admitting probes. The source must observe relevant
adapter, address, and route changes and expose readiness and loss of observation.
An unavailable source, a failed subscription, or an unready observer makes the
typed search unavailable and permits no sends. A channel handle alone does not
prove that observation has started or remains healthy.

For the desktop, establish observation and its current baseline **before showing
the confirmation**. Subscribe before taking the baseline and reconcile intervening
events before declaring it ready. Bind the displayed scope and request budget
to that observation generation, and keep observation active throughout the dialog,
consent acceptance, admission, and scan. A generation change or observation loss
while the dialog is open, or between acceptance and admission, invalidates that
confirmation and permits no sends. Present a fresh confirmation only after a new
healthy baseline is ready; never attach an old confirmation to a new generation,
even if the topology later appears unchanged. Changing the scope or budget also
invalidates the displayed confirmation.

The CLI first validates its explicit argument without granting scan authority,
then consumes that invocation's consent once against its initial healthy observed
baseline. Carry the same generation through admission and every send. Any later
generation change or observation loss ends that invocation's scan authority;
retrying requires a new explicit invocation, not automatic replay of the argument.

A detected change or loss of observation invalidates the scan's authority,
cancels and joins admitted probes, and reports any partial work as incomplete.
Loss includes a closed stream, observation errors or overflow, and gaps while
resubscribing. Restoration never resumes an old scan: another search requires
fresh confirmation. Check the live observation generation after write readiness
and immediately before every nonblocking send attempt, including retries.
Revoke on detection; UI notification debounce must not delay that revocation.

`default_network_change_source()` in `src/controller/runtime.rs` now starts a
native source on Linux (rtnetlink), macOS (a `PF_ROUTE` routing socket), and
Windows (IP Helper interface, unicast-address, and route notifications). Each
subscribes before reading its baseline, reports one change after resubscribing,
and gives up after bounded consecutive failures. Typed scans still need
cancellation evidence on every platform, and these sources and their debounced
controller stream still need the readiness, health, and immediate-invalidation
evidence above; their mere presence is not sufficient admission proof.

## Integration and acceptance

Linux route-derived admission, with its interface pinning, topology
fingerprints, and durable reservations, was retired on 2026-09-24 (V2.9). Until
V2.4 moves the CLI onto this contract, `--approved-range` keeps its current
`/24`, 256-candidate, and 15-second limits.

The ordinary cross-platform socket path is appropriate only for the explicit
typed range. Results retain their typed-search origin, and the registry's device
identity and network-change reconciliation continue to apply. No hostname, URL,
route guess, or stored device endpoint can silently become subnet authority.

Required evidence before V2.4 can be completed:

- One policy value drives `/23`, `/24`, `/31`, and `/32` preview, CLI, requests,
  result accounting, consent identity, and persistence. Reject boundary-crossing,
  noncanonical, oversized, and nonprivate input before side effects.
- Deterministic scheduler/send fixtures prove exact outbound budgets, retry pacing,
  jitter bounds, the 16-probe cap, deadline expiry, result limits, and cancellation
  after readiness. Partial results never become a completed empty run.
- Repeated activation, stale completion, revocation, close, policy changes,
  unavailable persistence, and network changes cannot authorize extra sends or
  leave unjoined workers. Repeated searches share the same pacing boundary.
- Missing, unready, failed, or interrupted network observation prevents admission
  or cancels an active scan. Prove revocation before pending sends resume, even
  while presentation notifications are debounced, and require fresh confirmation
  after observation is restored.
- Change the network while confirmation is open and between acceptance and
  admission; prove old dialogs and queued acceptance callbacks cannot authorize
  sends, including after the original topology returns. Reject changed scope or
  budget under an old confirmation. Prove CLI consent is consumed only once and
  cannot be replayed after its observed generation changes.
- Run the native socket/cancellation fixtures on Linux, macOS, and Windows.
  Prove that socket broadcast stays disabled and OS-classified broadcast sends
  are rejected without retrying with broader socket permissions.
  Test fixtures use the already accepted library loopback boundary; shipped
  input validation continues to reject loopback scopes.
- Exercise preview, confirmation, progress, Cancel, Forget, and error focus with
  keyboard and translated accessibility copy as the UI integration lands.

This approval permits implementation, not a completion checkbox. The maintainer,
jm2, owns the scope, consent, and delivery-boundary decisions; revisit the contract
before beta or any increase in scope, rate, retries, or automatic work.

[issue #71]: https://github.com/jm2/balun/issues/71
