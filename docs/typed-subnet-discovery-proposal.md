# Typed subnet discovery: approved contract

Status: approved by jm2 on 2026-09-18 for V2.4 and [issue #71], selecting
the `/23` traffic limits (1A) and confirmation before every search (2A).
The maintainer also accepted the [outbound-request boundary](#approved-delivery-boundary)
(option A) on 2026-09-18. V2.4 implements it on Linux, macOS, and Windows, in the
desktop and in `balun-discover --approved-range`; [Implementation](#implementation-v24)
maps each limit to its enforcement and tests.

The [authoritative plan](plan-v0.1.md#5-discovery-policy) and
[ADR-0001 amendment](architecture/adr-0001-discovery-playback.md#typed-subnet-amendment--2026-09-18)
adopt this separate typed-scope policy. Their former route-derived treatment of
user-entered ranges is superseded. Route-derived discovery itself was retired on
2026-09-24 ([ADR-0003](architecture/adr-0003-retire-route-derived-discovery.md)).

The side-effect-free `TypedSubnetScope` value (`src/discovery/typed_subnet.rs`) is
the one policy value: it accepts only canonical private `/23`–`/32` text and supplies
usable-host enumeration and exact candidate/outbound-attempt counts to the preview,
confirmation, consent, runner, CLI, and persisted preference alike. It carries no
consent or observation authority; diagnostic formatting redacts it, and explicit
display formatting retains canonical text for the preview and the preference.

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

`default_network_change_source()` in `src/controller/runtime.rs` starts a
native source on Linux (rtnetlink), macOS (a `PF_ROUTE` routing socket), and
Windows (IP Helper interface, unicast-address, and route notifications). Each
subscribes before reading its baseline, reports one change after resubscribing,
gives up after bounded consecutive failures, and publishes the readiness
[Implementation](#implementation-v24) describes beside its debounced changes.

## Integration and acceptance

Linux route-derived admission, with its interface pinning, topology
fingerprints, and durable reservations, was retired on 2026-09-24 (V2.9).
`--approved-range` now follows this contract, with the same limits as the desktop.

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

## Implementation (V2.4)

**Readiness.** `ObservationGate` (`src/discovery/changes/observation.rs`) publishes
`Ready(generation)` or `Unavailable`; every return to readiness is a new generation.
The shared watcher (`changes/watch.rs`) declares readiness only after the baseline is
reconciled with no notification queued, revokes it where a link or route notification
is recorded (inside the Linux monitor, the macOS reader, or the Windows callback) and,
for address notifications, as soon as an immediate inventory re-read differs, so the
debounce never delays revocation. An ended attempt, the resubscription gap, a
poisoned or overflowing monitor, a source that gave up, and a dropped source are all
unavailable. `NetworkChangeSource::subscribe` returns the change stream and this
`ObservationWatch` together (`src/controller/network.rs`).

**Consent.** `SubnetSearchConsent` (`src/discovery/subnet.rs`) confirms exactly the
displayed candidate count and request budget for one scope under one generation, is
neither `Clone` nor `Copy`, and is its own type, separate from exact-address
approval. `admit` consumes it against the live watch, refusing an unavailable or
different generation, including one that returns after a change.

**Controller.** `ControllerHandle::try_search_subnet` queues the consent; the actor
supersedes and joins any discovery, then admits against the live watch and publishes
`SubnetUnavailable` or `SubnetConfirmationStale` without sending. Readiness changes
are handled before network changes and commands: a subnet search whose generation
ended is cancelled, joined, and reported `NetworkChanged` with its replies discarded,
and a delivered change cancels one even if readiness was never revoked. Snapshots
carry the observation state, and the desktop offers subnet search only while it is
ready.

**Runner.** `discover_typed_subnet` searches from one process-wide lane: a second
search is refused, and the pacing boundary survives between searches. Each attempt
holds the lane lock across the pacing wait, write readiness, a re-check of
cancellation, the live generation, the deadline, the remaining budget, and the
destination's scope, and the nonblocking send; the next attempt may start 15.625 ms
plus 0–25% jitter after that send returned. Timers round waits up to whole
milliseconds, never down. At most 16 probes run, each attempt waits at most 200 ms,
a candidate reads at most 16 datagrams and stops after one accepted identity (no
retry), the 64th distinct device, the 30-second deadline, cancellation, or a change
stops the search and joins every probe, and the report says `Complete` or
`Incomplete` with the reason. Sockets bind `0.0.0.0:0` with broadcast disabled and
confirmed; a permission-refused send is counted and never retried, and a refusing
host (ICMP port unreachable) ends its candidate quietly.

**Results.** Replies carry `DiscoveryMethod::TypedSubnet`. The controller keeps the
latest result for each of at most four subnets, validates that every reply comes from
a usable host of its subnet on the discovery port, and replays them through the
registry's identity rules. Deadline and device-limit results keep their devices and
show *incomplete*; a complete search that found nothing retires that subnet's
devices. Because a typed reply names no interface, a network change that removes an
interface or an IPv4 address anywhere expires typed-subnet evidence; IPv6-only
changes leave it, as they leave other IPv4 evidence. HTTP metadata is fetched only for
a selected device, never for nonresponders.

**Desktop.** **Search a subnet** (`src/ui/subnet_search_dialog.rs`) validates the
entry, previews the exact budget, and opens a plain-text confirmation, Cancel by
default, bound to the generation in the current snapshot. A snapshot in any other
state closes it and invalidates a queued Search response. Only the entered text is
remembered (`subnet_prefix`, settings schema 3, written only while a prefix is
remembered); Forget clears it, and it never authorizes a search.

**CLI.** `--approved-range` parses with `TypedSubnetScope`, waits up to ten seconds
for the native source's first healthy baseline, prints the scope and budget, consumes
its consent once, runs one search, and exits with an error if the search was
incomplete; a changed network is never retried.

**Tests.** Paused-clock fixtures with a scripted transport prove the `/23` budget of
exactly 1,020 attempts, spacing at the send boundary with minimum and maximum jitter,
the 16-probe cap, the reply window, receive, identity, and device limits, deadline
expiry as incomplete with no later send, revocation after readiness before first
attempts and retries, a mid-search change that never revives, the shared pacing
boundary, and one search per lane. Native loopback fixtures on every platform lane
send real requests, prove one paced retry, stop after readiness on cancellation or a
change, and show the operating system refusing a limited-broadcast send while
broadcast stays disabled. Controller, CLI, settings, localization, and desktop
projection tests cover the rest; the dialog and sidebar display tests run in the
Linux desktop lifecycle job.

**Limits of the evidence.** The native broadcast fixture exercises the limited
broadcast address; a directed broadcast inside a typed subnet needs a matching local
interface, so the refusal and no-retry path for it is proven with the scripted
transport. Address-only changes are detected by an immediate inventory re-read rather
than from the notification itself. Real-network behaviour on macOS and Windows, such
as how often their sources report changes, awaits owner confirmation.

[issue #71]: https://github.com/jm2/balun/issues/71
