//! Paced, bounded search of one explicitly confirmed typed subnet (V2.4).
//!
//! Authority comes in three steps. A [`TypedSubnetScope`] validates what the
//! user typed and grants nothing. A [`SubnetSearchConsent`] confirms exactly
//! that scope and its outbound request budget against the network-observation
//! generation that was healthy when the confirmation was shown. Admitting the
//! consent consumes it against the live observation and yields one
//! [`SubnetScanPermit`], which [`discover_typed_subnet`] spends on one search.
//!
//! The search enforces the approved contract itself:
//!
//! - the candidates are the scope's usable hosts, at most 510, each probed
//!   with at most two requests to the fixed discovery port; every attempt,
//!   including a retry, spends the scope's request budget of at most 1,020;
//! - attempts are paced at the actual send boundary: one lane lock is held
//!   across the pacing wait, write readiness, the authority re-check, and the
//!   nonblocking send, and the next attempt may start no earlier than
//!   15.625 ms plus up to 25% nonnegative jitter after that send returned.
//!   The boundary belongs to the process-wide lane, so a later search cannot
//!   start with a burst;
//! - at most 16 probes are in flight, and one search runs per process;
//! - each attempt waits at most 200 ms for replies; a candidate reads at most
//!   16 datagrams and accepts one identity, after which it sends no retry;
//! - reaching 64 distinct devices, the 30-second deadline, cancellation, or a
//!   network change stops the search, joins every probe, and reports the
//!   search incomplete, never as a completed empty one;
//! - after write readiness and immediately before every nonblocking send, the
//!   search re-checks cancellation, the live observation generation, the
//!   deadline, the remaining budget, and that the destination is a usable
//!   host of the scope;
//! - sockets keep broadcasting disabled, and a send the operating system
//!   refuses, as it refuses a broadcast destination, is never retried.
//!
//! Only validated responders reach the report, so HTTP metadata enrichment
//! stays a separate, responder-only step.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use ipnet::Ipv4Net;
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::task::{JoinError, JoinSet};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use super::client::{DiscoveryReport, ProbeFailureClass, ProbeIssue, validated_observation};
use super::{
    DiscoveryMethod, ObservationGeneration, ObservationState, ObservationWatch, ProbeEndpoint,
    TypedSubnetScope,
};
use crate::domain::DeviceId;
use crate::hdhr::protocol::{DISCOVERY_UDP_PORT, MAX_PACKET_SIZE, encode_tuner_discover_request};

/// Why a displayed confirmation cannot become consent.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubnetConsentError {
    #[error("the confirmed candidate count or request budget does not match the subnet")]
    BudgetMismatch,
}

/// Why consent could not be admitted. Neither reason sends anything.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubnetAdmissionError {
    #[error("network changes are not being observed, so subnet search is unavailable")]
    ObservationUnavailable,
    #[error("the network changed after the subnet search was confirmed")]
    Stale,
}

/// Why a subnet search could not start at all. No request was sent.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubnetScanError {
    #[error("another subnet search is already running in this process")]
    Busy,
    #[error("the discovery request could not be encoded")]
    Request,
}

/// One confirmation of an exact typed scope and its outbound request budget,
/// bound to the network-observation generation that was healthy when the
/// confirmation was shown.
///
/// It is neither `Clone` nor `Copy`: admitting it consumes it, so one
/// confirmation authorizes at most one search. Its type is separate from
/// every exact-address approval, and on its own it grants nothing.
pub struct SubnetSearchConsent {
    scope: TypedSubnetScope,
    generation: ObservationGeneration,
}

impl SubnetSearchConsent {
    /// Confirm `scope` exactly as it was displayed: the candidate count and
    /// outbound request budget shown must be the ones this policy derives
    /// from the same scope.
    pub fn confirm(
        scope: TypedSubnetScope,
        displayed_candidates: usize,
        displayed_request_budget: usize,
        generation: ObservationGeneration,
    ) -> Result<Self, SubnetConsentError> {
        if displayed_candidates != scope.candidate_count()
            || displayed_request_budget != scope.maximum_request_attempts()
        {
            return Err(SubnetConsentError::BudgetMismatch);
        }
        Ok(Self { scope, generation })
    }

    /// The confirmed scope.
    #[must_use]
    pub const fn scope(&self) -> TypedSubnetScope {
        self.scope
    }

    /// The observation generation the confirmation was shown under.
    #[must_use]
    pub const fn generation(&self) -> ObservationGeneration {
        self.generation
    }

    /// Consume this consent against the live observation. It is admitted
    /// only while observation is healthy in exactly the confirmed generation;
    /// a later generation never matches, even if the network looks the same.
    pub fn admit(
        self,
        observation: &ObservationWatch,
    ) -> Result<SubnetScanPermit, SubnetAdmissionError> {
        match observation.current() {
            ObservationState::Ready(generation) if generation == self.generation => {
                Ok(SubnetScanPermit {
                    scope: self.scope,
                    authority: Authority {
                        observation: observation.clone(),
                        generation,
                    },
                })
            }
            ObservationState::Ready(_) => Err(SubnetAdmissionError::Stale),
            ObservationState::Unavailable => Err(SubnetAdmissionError::ObservationUnavailable),
        }
    }
}

impl fmt::Debug for SubnetSearchConsent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubnetSearchConsent")
            .field("scope", &self.scope)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Admitted authority for one search: its scope and the observation
/// generation it stays bound to. A detected change revokes it at once.
pub struct SubnetScanPermit {
    scope: TypedSubnetScope,
    authority: Authority,
}

impl SubnetScanPermit {
    /// The confirmed scope this permit searches.
    #[must_use]
    pub const fn scope(&self) -> TypedSubnetScope {
        self.scope
    }

    /// The observation generation this permit is bound to.
    #[must_use]
    pub const fn generation(&self) -> ObservationGeneration {
        self.authority.generation
    }

    /// Whether observation is still healthy in the permit's generation.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.authority.is_live()
    }
}

impl fmt::Debug for SubnetScanPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubnetScanPermit")
            .field("scope", &self.scope)
            .field("generation", &self.authority.generation)
            .finish()
    }
}

/// How a subnet search ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubnetScanOutcome {
    /// Every candidate was probed within every limit.
    Complete,
    /// The search stopped early; its report covers only part of the scope.
    Incomplete(SubnetScanIncomplete),
}

/// Why a subnet search stopped before probing every candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubnetScanIncomplete {
    /// The 30-second UDP deadline expired first.
    Deadline,
    /// The search reached its limit of distinct devices.
    DeviceLimit,
    /// A detected network change or loss of observation revoked the permit.
    NetworkChanged,
    /// The search was cancelled.
    Cancelled,
    /// The outbound request budget was spent before every candidate was
    /// probed. Each candidate's attempts fit the budget, so this is a guard.
    RequestBudget,
}

/// The result of one subnet search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubnetScanReport {
    /// Validated responders, per-candidate issues, and datagram statistics.
    pub report: DiscoveryReport,
    /// Whether every candidate was probed.
    pub outcome: SubnetScanOutcome,
    /// Outbound request attempts, including retries and refused sends.
    pub requests_attempted: usize,
    /// Sends the operating system refused, as it refuses a broadcast
    /// destination while broadcasting is disabled. None was retried.
    pub refused_sends: usize,
}

/// Search the permit's scope from this process's single subnet lane.
///
/// Returns [`SubnetScanError::Busy`] without sending if another search is
/// already running in this process. Cancellation, a network change, the
/// deadline, and the device limit end the search with an incomplete report.
pub async fn discover_typed_subnet(
    permit: SubnetScanPermit,
    cancellation: &CancellationToken,
) -> Result<SubnetScanReport, SubnetScanError> {
    scan(
        SubnetScanLane::process(),
        &SystemTransport,
        ScanPlan::typed(permit.scope),
        permit.authority,
        cancellation,
    )
    .await
}

/// The observation generation a search stays bound to.
#[derive(Clone)]
struct Authority {
    observation: ObservationWatch,
    generation: ObservationGeneration,
}

impl Authority {
    fn is_live(&self) -> bool {
        self.observation.is_ready_for(self.generation)
    }
}

/// The one subnet-search lane of a process and its shared pacing boundary.
struct SubnetScanLane {
    busy: AtomicBool,
    /// The earliest instant the next request may be sent, across searches.
    next_send: tokio::sync::Mutex<Option<Instant>>,
    entropy: Box<dyn Fn() -> u64 + Send + Sync>,
}

static PROCESS_LANE: LazyLock<Arc<SubnetScanLane>> =
    LazyLock::new(|| Arc::new(SubnetScanLane::new(system_entropy)));

/// OS entropy for jitter; without it, the maximum extra delay.
fn system_entropy() -> u64 {
    getrandom::u64().unwrap_or(u64::MAX)
}

impl SubnetScanLane {
    fn new(entropy: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            busy: AtomicBool::new(false),
            next_send: tokio::sync::Mutex::new(None),
            entropy: Box::new(entropy),
        }
    }

    fn process() -> Arc<Self> {
        Arc::clone(&PROCESS_LANE)
    }

    /// Claim the lane for one search, or `None` while another runs.
    fn begin(self: &Arc<Self>) -> Option<LaneClaim> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| LaneClaim {
                lane: Arc::clone(self),
            })
    }
}

/// Serializes tests that use the process-wide lane.
#[cfg(test)]
pub(crate) static PROCESS_LANE_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Hold the process-wide lane busy, as a running search would.
#[cfg(test)]
pub(crate) fn claim_process_lane() -> impl Drop {
    SubnetScanLane::process()
        .begin()
        .expect("the process lane is idle")
}

/// Releases the lane once a search has joined every probe.
struct LaneClaim {
    lane: Arc<SubnetScanLane>,
}

impl Drop for LaneClaim {
    fn drop(&mut self) {
        self.lane.busy.store(false, Ordering::Release);
    }
}

/// Nonnegative jitter of at most 25% of the minimum spacing.
fn send_jitter(entropy: u64) -> Duration {
    let maximum = TypedSubnetScope::MAX_SEND_JITTER.as_nanos();
    let extra = maximum * u128::from(entropy) / u128::from(u64::MAX);
    Duration::from_nanos(u64::try_from(extra).unwrap_or(u64::MAX))
}

/// Opens one socket per probed candidate.
trait SubnetTransport: Sync {
    type Socket: SubnetSocket;

    fn open(&self) -> io::Result<Self::Socket>;
}

/// The socket operations a probe needs: readiness, one nonblocking send, and
/// receiving replies.
trait SubnetSocket: Send + Sync + 'static {
    fn writable(&self) -> impl Future<Output = io::Result<()>> + Send;

    fn try_send_to(&self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize>;

    fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> impl Future<Output = io::Result<(usize, SocketAddr)>> + Send;
}

/// Ordinary unbound IPv4 UDP sockets; the system's routing picks the path.
struct SystemTransport;

impl SubnetTransport for SystemTransport {
    type Socket = UdpSocket;

    fn open(&self) -> io::Result<UdpSocket> {
        open_system_socket()
    }
}

/// Bind an ephemeral IPv4 socket with broadcasting disabled and confirmed so.
fn open_system_socket() -> io::Result<UdpSocket> {
    let socket = std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?;
    socket.set_broadcast(false)?;
    if socket.broadcast()? {
        return Err(io::Error::other("the socket kept broadcasting enabled"));
    }
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket)
}

impl SubnetSocket for UdpSocket {
    fn writable(&self) -> impl Future<Output = io::Result<()>> + Send {
        Self::writable(self)
    }

    fn try_send_to(&self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        Self::try_send_to(self, datagram, destination)
    }

    fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> impl Future<Output = io::Result<(usize, SocketAddr)>> + Send {
        Self::recv_from(self, buffer)
    }
}

/// What one search probes: the entered network, its candidates in order,
/// and the destination port.
struct ScanPlan {
    network: Ipv4Net,
    candidates: Vec<Ipv4Addr>,
    port: u16,
}

impl ScanPlan {
    fn typed(scope: TypedSubnetScope) -> Self {
        Self {
            network: scope.network(),
            candidates: scope.candidates().collect(),
            port: DISCOVERY_UDP_PORT,
        }
    }

    /// Whether `destination` is a usable host of the entered network on the
    /// planned port.
    fn admits(&self, destination: SocketAddr) -> bool {
        let SocketAddr::V4(destination) = destination else {
            return false;
        };
        let address = *destination.ip();
        destination.port() == self.port
            && self.network.contains(&address)
            && !address.is_broadcast()
            && !address.is_multicast()
            && !address.is_unspecified()
            && (self.network.prefix_len() >= 31
                || (address != self.network.network() && address != self.network.broadcast()))
    }
}

/// State shared by one search and its probes.
struct ScanContext {
    lane: Arc<SubnetScanLane>,
    plan: ScanPlan,
    authority: Authority,
    cancellation: CancellationToken,
    /// Cancelled once the search stops for any reason; every probe watches it.
    stop: CancellationToken,
    reason: Mutex<Option<SubnetScanIncomplete>>,
    deadline: Instant,
    budget: AtomicUsize,
    attempted: AtomicUsize,
    refused: AtomicUsize,
    request: Vec<u8>,
}

/// How one send attempt ended.
enum SendOutcome {
    /// The request left through the socket when the send returned.
    Sent(Instant),
    /// The search stopped; nothing was sent.
    Halted,
    /// The destination is not a usable host of the scope; nothing was sent.
    OutOfScope,
    /// The operating system refused the send; it is never retried.
    Refused(io::ErrorKind),
    /// The socket failed.
    Failed(io::ErrorKind),
}

impl ScanContext {
    /// Stop the search, keeping the first reason.
    fn halt(&self, reason: SubnetScanIncomplete) {
        self.reason
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(reason);
        self.stop.cancel();
    }

    fn reason(&self) -> Option<SubnetScanIncomplete> {
        *self.reason.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Why no request may be sent now, if anything.
    fn refusal(&self) -> Option<SubnetScanIncomplete> {
        if self.stop.is_cancelled() {
            return Some(self.reason().unwrap_or(SubnetScanIncomplete::Cancelled));
        }
        if self.cancellation.is_cancelled() {
            Some(SubnetScanIncomplete::Cancelled)
        } else if !self.authority.is_live() {
            Some(SubnetScanIncomplete::NetworkChanged)
        } else if Instant::now() >= self.deadline {
            Some(SubnetScanIncomplete::Deadline)
        } else if self.budget.load(Ordering::Acquire) == 0 {
            Some(SubnetScanIncomplete::RequestBudget)
        } else {
            None
        }
    }

    /// Send one request to `destination` at the lane's pacing boundary.
    ///
    /// The lane lock is held from the pacing wait through the send, so no
    /// other attempt in this process can come between them. Every condition
    /// is checked again after write readiness and immediately before the
    /// nonblocking send.
    async fn send<S: SubnetSocket>(&self, socket: &S, destination: SocketAddr) -> SendOutcome {
        if !self.plan.admits(destination) {
            return SendOutcome::OutOfScope;
        }
        let mut next_send = tokio::select! {
            biased;
            () = self.stop.cancelled() => return SendOutcome::Halted,
            next_send = self.lane.next_send.lock() => next_send,
        };
        if let Some(at) = *next_send {
            tokio::select! {
                biased;
                () = self.stop.cancelled() => return SendOutcome::Halted,
                () = sleep_until(at) => {}
            }
        }
        loop {
            tokio::select! {
                biased;
                () = self.stop.cancelled() => return SendOutcome::Halted,
                ready = socket.writable() => {
                    if let Err(error) = ready {
                        return SendOutcome::Failed(error.kind());
                    }
                }
            }
            if let Some(reason) = self.refusal() {
                self.halt(reason);
                return SendOutcome::Halted;
            }
            let result = socket.try_send_to(&self.request, destination);
            if matches!(&result, Err(error) if error.kind() == io::ErrorKind::WouldBlock) {
                continue;
            }
            let sent_at = Instant::now();
            // Only the lane holder sends, and `refusal` saw budget left.
            self.budget.fetch_sub(1, Ordering::AcqRel);
            self.attempted.fetch_add(1, Ordering::AcqRel);
            *next_send = Some(
                sent_at + TypedSubnetScope::MIN_SEND_INTERVAL + send_jitter((self.lane.entropy)()),
            );
            return match result {
                Ok(length) if length == self.request.len() => SendOutcome::Sent(sent_at),
                Ok(_) => SendOutcome::Failed(io::ErrorKind::WriteZero),
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    self.refused.fetch_add(1, Ordering::AcqRel);
                    SendOutcome::Refused(error.kind())
                }
                Err(error) => SendOutcome::Failed(error.kind()),
            };
        }
    }
}

/// One candidate's report and, when it failed, the reason.
struct CandidateResult {
    candidate: Ipv4Addr,
    report: DiscoveryReport,
    issue: Option<String>,
}

fn subnet_endpoint(destination: SocketAddr) -> ProbeEndpoint {
    ProbeEndpoint {
        bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        destination,
        method: DiscoveryMethod::TypedSubnet,
        interface: None,
        accepted_source_network: None,
    }
}

/// Probe one candidate: at most two attempts, each followed by one reply
/// window, until one identity is accepted or the receive budget is spent.
async fn probe<S: SubnetSocket>(
    context: Arc<ScanContext>,
    socket: S,
    candidate: Ipv4Addr,
) -> CandidateResult {
    let destination = SocketAddr::V4(SocketAddrV4::new(candidate, context.plan.port));
    let endpoint = subnet_endpoint(destination);
    let mut result = CandidateResult {
        candidate,
        report: DiscoveryReport::default(),
        issue: None,
    };
    result.report.stats.probes_started = 1;
    let mut buffer = [0_u8; MAX_PACKET_SIZE + 1];
    for _ in 0..TypedSubnetScope::ATTEMPTS_PER_CANDIDATE {
        let sent_at = match context.send(&socket, destination).await {
            SendOutcome::Sent(sent_at) => sent_at,
            SendOutcome::Halted => return result,
            SendOutcome::OutOfScope => {
                result.issue =
                    Some("the destination is not a usable host of the confirmed subnet".into());
                return result;
            }
            SendOutcome::Refused(kind) => {
                result.issue = Some(format!(
                    "the operating system refused the send ({kind:?}); it was not retried"
                ));
                return result;
            }
            SendOutcome::Failed(kind) => {
                result.issue = Some(format!("the discovery request failed ({kind:?})"));
                return result;
            }
        };
        result.report.stats.datagrams_sent += 1;
        let window = (sent_at + TypedSubnetScope::REPLY_WINDOW).min(context.deadline);
        loop {
            if result.report.stats.datagrams_received
                >= TypedSubnetScope::MAX_RECEIVED_PER_CANDIDATE
            {
                result.report.stats.receive_limit_reached = true;
                return result;
            }
            let received = tokio::select! {
                biased;
                () = context.stop.cancelled() => return result,
                () = sleep_until(window) => break,
                received = socket.recv_from(&mut buffer) => received,
            };
            let (length, source) = match received {
                Ok(received) => received,
                // The host answered that nothing listens on the discovery
                // port: it is not a tuner, and there is nothing to retry.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    return result;
                }
                Err(error) => {
                    result.issue = Some(format!("receiving a reply failed ({:?})", error.kind()));
                    return result;
                }
            };
            result.report.stats.datagrams_received += 1;
            match validated_observation(&endpoint, source, &buffer[..length], None) {
                Some(observation) => {
                    result.report.stats.datagrams_accepted += 1;
                    result.report.observations.push(observation);
                    // One accepted identity per candidate; a retry is moot.
                    return result;
                }
                None => result.report.stats.datagrams_rejected += 1,
            }
        }
    }
    result
}

/// Accumulates candidate results under the distinct-device limit.
#[derive(Default)]
struct Aggregate {
    report: DiscoveryReport,
    devices: BTreeSet<DeviceId>,
}

impl Aggregate {
    fn issue(&mut self, candidate: Ipv4Addr, class: ProbeFailureClass, message: String) {
        self.report.issues.push(ProbeIssue {
            endpoint: subnet_endpoint(SocketAddr::from((candidate, DISCOVERY_UDP_PORT))),
            class,
            message,
        });
    }

    fn merge(&mut self, joined: Result<CandidateResult, JoinError>, context: &ScanContext) {
        let result = match joined {
            Ok(result) => result,
            Err(_) => {
                self.report.issues.push(ProbeIssue {
                    endpoint: subnet_endpoint(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))),
                    class: ProbeFailureClass::Task,
                    message: "a subnet probe task failed".into(),
                });
                return;
            }
        };
        let mut report = result.report;
        let mut dropped = false;
        report.observations.retain(|observation| {
            if self.devices.contains(&observation.device_id) {
                return true;
            }
            if self.devices.len() >= TypedSubnetScope::MAX_DEVICES {
                dropped = true;
                return false;
            }
            self.devices.insert(observation.device_id);
            true
        });
        report.stats.device_limit_reached |= dropped;
        self.report.merge(report);
        if let Some(message) = result.issue {
            let destination = SocketAddr::from((result.candidate, context.plan.port));
            self.report.issues.push(ProbeIssue {
                endpoint: subnet_endpoint(destination),
                class: ProbeFailureClass::Network,
                message,
            });
        }
        if self.devices.len() >= TypedSubnetScope::MAX_DEVICES {
            self.report.stats.device_limit_reached = true;
            context.halt(SubnetScanIncomplete::DeviceLimit);
        }
    }
}

/// Run one search on `lane` through `transport`.
async fn scan<T: SubnetTransport>(
    lane: Arc<SubnetScanLane>,
    transport: &T,
    plan: ScanPlan,
    authority: Authority,
    cancellation: &CancellationToken,
) -> Result<SubnetScanReport, SubnetScanError> {
    let _claim = lane.begin().ok_or(SubnetScanError::Busy)?;
    let request = encode_tuner_discover_request(None).map_err(|_| SubnetScanError::Request)?;
    let candidates = plan.candidates.clone();
    let budget = candidates.len() * TypedSubnetScope::ATTEMPTS_PER_CANDIDATE;
    let mut observation = authority.observation.clone();
    observation.mark_seen();
    let generation = authority.generation;
    let context = Arc::new(ScanContext {
        lane,
        plan,
        authority,
        cancellation: cancellation.clone(),
        // Independent of the caller's token, so every stop records a reason.
        stop: CancellationToken::new(),
        reason: Mutex::new(None),
        deadline: Instant::now() + TypedSubnetScope::DEADLINE,
        budget: AtomicUsize::new(budget),
        attempted: AtomicUsize::new(0),
        refused: AtomicUsize::new(0),
        request,
    });
    let mut pending = candidates.into_iter();
    let mut tasks = JoinSet::new();
    let mut aggregate = Aggregate::default();
    loop {
        while !context.stop.is_cancelled() && tasks.len() < TypedSubnetScope::MAX_IN_FLIGHT {
            let Some(candidate) = pending.next() else {
                break;
            };
            // No probe is admitted without live authority and budget left.
            if let Some(reason) = context.refusal() {
                context.halt(reason);
                break;
            }
            match transport.open() {
                Ok(socket) => {
                    tasks.spawn(probe(Arc::clone(&context), socket, candidate));
                }
                Err(error) => aggregate.issue(
                    candidate,
                    ProbeFailureClass::Network,
                    format!("a probe socket could not be opened ({:?})", error.kind()),
                ),
            }
        }
        if context.stop.is_cancelled() || tasks.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => context.halt(SubnetScanIncomplete::Cancelled),
            state = observation.changed() => {
                if !state.is_ready_for(generation) {
                    context.halt(SubnetScanIncomplete::NetworkChanged);
                }
            }
            () = sleep_until(context.deadline) => context.halt(SubnetScanIncomplete::Deadline),
            Some(joined) = tasks.join_next() => aggregate.merge(joined, &context),
        }
    }
    // Cancel and join every admitted probe before reporting.
    context.stop.cancel();
    while let Some(joined) = tasks.join_next().await {
        aggregate.merge(joined, &context);
    }
    Ok(SubnetScanReport {
        report: aggregate.report,
        outcome: context
            .reason()
            .map_or(SubnetScanOutcome::Complete, SubnetScanOutcome::Incomplete),
        requests_attempted: context.attempted.load(Ordering::Acquire),
        refused_sends: context.refused.load(Ordering::Acquire),
    })
}

#[cfg(test)]
mod tests;
