//! Deterministic scheduler and send fixtures on a paused clock, plus native
//! loopback socket fixtures that run in every platform lane.
//!
//! The fake transport records every send attempt with its instant, counts
//! open sockets, and delivers scripted replies after scripted delays, so the
//! budget, pacing, concurrency, window, and limit assertions are exact.
//! Loopback fixtures use the library boundary directly; shipped input
//! validation still rejects loopback scopes.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;

use tokio::sync::mpsc;

use super::*;
use crate::discovery::ObservationGate;
use crate::hdhr::protocol::{
    DEVICE_TYPE_TUNER, TAG_DEVICE_ID, TAG_DEVICE_TYPE, TYPE_DISCOVER_REPLY,
};

const MIN: Duration = TypedSubnetScope::MIN_SEND_INTERVAL;
const MAX_JITTER: Duration = TypedSubnetScope::MAX_SEND_JITTER;
/// Tokio timers fire on whole milliseconds, so a paced wait can only end
/// later than requested, never earlier, and by less than this.
const TIMER_GRANULARITY: Duration = Duration::from_millis(1);

/// A valid tuner discovery reply for `device_id`.
fn reply_frame(device_id: u32) -> Vec<u8> {
    let mut payload = vec![TAG_DEVICE_TYPE, 4];
    payload.extend(DEVICE_TYPE_TUNER.to_be_bytes());
    payload.extend([TAG_DEVICE_ID, 4]);
    payload.extend(device_id.to_be_bytes());
    let mut frame = TYPE_DISCOVER_REPLY.to_be_bytes().to_vec();
    frame.extend(u16::try_from(payload.len()).unwrap().to_be_bytes());
    frame.extend(payload);
    let crc = crc32fast::hash(&frame);
    frame.extend(crc.to_le_bytes());
    frame
}

/// `count` distinct valid DeviceIDs.
fn device_ids(count: usize) -> Vec<u32> {
    (0x1050_0000_u32..)
        .filter(|value| DeviceId::new(*value).is_ok())
        .take(count)
        .collect()
}

fn scope(text: &str) -> TypedSubnetScope {
    text.parse().unwrap()
}

fn ready() -> (ObservationGate, Authority) {
    let gate = ObservationGate::new();
    gate.establish();
    let authority = Authority {
        observation: gate.watch(),
        generation: gate.state().generation().unwrap(),
    };
    (gate, authority)
}

fn lane(entropy: u64) -> Arc<SubnetScanLane> {
    Arc::new(SubnetScanLane::new(move || entropy))
}

fn at(address: Ipv4Addr) -> SocketAddr {
    SocketAddr::from((address, DISCOVERY_UDP_PORT))
}

/// One scripted event delivered to the socket that sent a request.
enum Reply {
    Datagram {
        delay: Duration,
        source: SocketAddr,
        bytes: Vec<u8>,
    },
    Error {
        delay: Duration,
        kind: io::ErrorKind,
    },
}

type Responder = dyn Fn(SocketAddr, usize) -> Vec<Reply> + Send + Sync;

/// A packet-free network: records sends and replays scripted replies.
struct FakeNetwork {
    sends: Mutex<Vec<(Instant, SocketAddr)>>,
    requests: Mutex<BTreeMap<SocketAddr, usize>>,
    open: AtomicUsize,
    max_open: AtomicUsize,
    readiness: AtomicUsize,
    closed: Mutex<Vec<(Instant, Option<SocketAddr>)>>,
    ready_delay: Duration,
    responder: Box<Responder>,
    on_ready: Box<dyn Fn(usize) + Send + Sync>,
    send_error: Box<dyn Fn(SocketAddr) -> Option<io::ErrorKind> + Send + Sync>,
}

impl FakeNetwork {
    fn silent() -> Self {
        Self::replying(|_, _| Vec::new())
    }

    fn replying(
        responder: impl Fn(SocketAddr, usize) -> Vec<Reply> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sends: Mutex::new(Vec::new()),
            requests: Mutex::new(BTreeMap::new()),
            open: AtomicUsize::new(0),
            max_open: AtomicUsize::new(0),
            readiness: AtomicUsize::new(0),
            closed: Mutex::new(Vec::new()),
            ready_delay: Duration::ZERO,
            responder: Box::new(responder),
            on_ready: Box::new(|_| {}),
            send_error: Box::new(|_| None),
        }
    }

    fn sends(&self) -> Vec<(Instant, SocketAddr)> {
        self.sends.lock().unwrap().clone()
    }

    fn sends_to(&self, destination: SocketAddr) -> Vec<Instant> {
        self.sends()
            .into_iter()
            .filter(|(_, sent)| *sent == destination)
            .map(|(instant, _)| instant)
            .collect()
    }

    fn closed_at(&self, destination: SocketAddr) -> Instant {
        self.closed
            .lock()
            .unwrap()
            .iter()
            .find(|(_, sent)| *sent == Some(destination))
            .map(|(instant, _)| *instant)
            .unwrap()
    }
}

struct FakeTransport(Arc<FakeNetwork>);

type Inbox = mpsc::UnboundedReceiver<io::Result<(Vec<u8>, SocketAddr)>>;

struct FakeSocket {
    network: Arc<FakeNetwork>,
    replies: mpsc::UnboundedSender<io::Result<(Vec<u8>, SocketAddr)>>,
    inbox: tokio::sync::Mutex<Inbox>,
    destination: Mutex<Option<SocketAddr>>,
}

impl SubnetTransport for FakeTransport {
    type Socket = FakeSocket;

    fn open(&self) -> io::Result<FakeSocket> {
        let open = self.0.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.max_open.fetch_max(open, Ordering::SeqCst);
        let (replies, inbox) = mpsc::unbounded_channel();
        Ok(FakeSocket {
            network: Arc::clone(&self.0),
            replies,
            inbox: tokio::sync::Mutex::new(inbox),
            destination: Mutex::new(None),
        })
    }
}

impl Drop for FakeSocket {
    fn drop(&mut self) {
        self.network.open.fetch_sub(1, Ordering::SeqCst);
        let destination = *self.destination.lock().unwrap();
        self.network
            .closed
            .lock()
            .unwrap()
            .push((Instant::now(), destination));
    }
}

impl SubnetSocket for FakeSocket {
    async fn writable(&self) -> io::Result<()> {
        if !self.network.ready_delay.is_zero() {
            tokio::time::sleep(self.network.ready_delay).await;
        }
        let readiness = self.network.readiness.fetch_add(1, Ordering::SeqCst) + 1;
        (self.network.on_ready)(readiness);
        Ok(())
    }

    fn try_send_to(&self, _datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        self.destination.lock().unwrap().get_or_insert(destination);
        self.network
            .sends
            .lock()
            .unwrap()
            .push((Instant::now(), destination));
        if let Some(kind) = (self.network.send_error)(destination) {
            return Err(kind.into());
        }
        let index = {
            let mut requests = self.network.requests.lock().unwrap();
            let count = requests.entry(destination).or_default();
            *count += 1;
            *count - 1
        };
        for reply in (self.network.responder)(destination, index) {
            let replies = self.replies.clone();
            let (delay, item) = match reply {
                Reply::Datagram {
                    delay,
                    source,
                    bytes,
                } => (delay, Ok((bytes, source))),
                Reply::Error { delay, kind } => (delay, Err(io::Error::from(kind))),
            };
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = replies.send(item);
            });
        }
        Ok(_datagram.len())
    }

    async fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut inbox = self.inbox.lock().await;
        match inbox.recv().await {
            Some(Ok((bytes, source))) => {
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok((bytes.len(), source))
            }
            Some(Err(error)) => Err(error),
            None => std::future::pending().await,
        }
    }
}

async fn run(
    network: &Arc<FakeNetwork>,
    lane: &Arc<SubnetScanLane>,
    plan: ScanPlan,
    authority: Authority,
    cancellation: &CancellationToken,
) -> SubnetScanReport {
    scan(
        Arc::clone(lane),
        &FakeTransport(Arc::clone(network)),
        plan,
        authority,
        cancellation,
    )
    .await
    .unwrap()
}

fn gaps(sends: &[(Instant, SocketAddr)]) -> Vec<Duration> {
    sends.windows(2).map(|pair| pair[1].0 - pair[0].0).collect()
}

#[test]
fn jitter_is_nonnegative_and_at_most_a_quarter_of_the_spacing() {
    assert_eq!(send_jitter(0), Duration::ZERO);
    assert_eq!(send_jitter(u64::MAX), MAX_JITTER);
    assert_eq!(MAX_JITTER * 4, MIN);
    let mut previous = Duration::ZERO;
    for entropy in [1, u64::MAX / 7, u64::MAX / 4, u64::MAX / 2, u64::MAX - 1] {
        let jitter = send_jitter(entropy);
        assert!(jitter >= previous && jitter <= MAX_JITTER);
        previous = jitter;
    }
    // OS entropy is read without failing; without it the maximum is used.
    system_entropy();
}

#[tokio::test(start_paused = true)]
async fn a_slash_23_spends_exactly_its_budget_paced_at_the_send_boundary() {
    let typed = scope("10.0.0.0/23");
    for (entropy, minimum_gap) in [(0, MIN), (u64::MAX, MIN + MAX_JITTER)] {
        let network = Arc::new(FakeNetwork::silent());
        let (_gate, authority) = ready();
        let started = Instant::now();
        let report = run(
            &network,
            &lane(entropy),
            ScanPlan::typed(typed),
            authority,
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(report.outcome, SubnetScanOutcome::Complete);
        assert_eq!(report.requests_attempted, 1_020);
        assert_eq!(report.requests_attempted, typed.maximum_request_attempts());
        assert_eq!(report.report.stats.datagrams_sent, 1_020);
        assert_eq!(report.report.stats.probes_started, 510);
        assert_eq!(report.refused_sends, 0);
        let sends = network.sends();
        assert_eq!(sends.len(), 1_020);
        let mut per_destination = BTreeMap::<SocketAddr, usize>::new();
        for (_, destination) in &sends {
            *per_destination.entry(*destination).or_default() += 1;
        }
        assert_eq!(
            per_destination.keys().copied().collect::<Vec<_>>(),
            typed.candidates().map(at).collect::<Vec<_>>()
        );
        assert!(per_destination.values().all(|count| *count == 2));

        // Every attempt, first or retry, respects the spacing, and pacing is
        // what binds: nearly every gap is the paced minimum, rounded up only
        // to the timer's granularity.
        let gaps = gaps(&sends);
        assert!(gaps.iter().all(|gap| *gap >= minimum_gap), "{entropy}");
        assert!(
            gaps.iter()
                .filter(|gap| **gap < minimum_gap + TIMER_GRANULARITY)
                .count()
                > 1_000
        );
        assert_eq!(network.max_open.load(Ordering::SeqCst), 16);
        assert_eq!(network.open.load(Ordering::SeqCst), 0);
        // Even at maximum jitter the whole budget fits the deadline.
        assert!(sends.last().unwrap().0 - started < TypedSubnetScope::DEADLINE);
    }
}

#[tokio::test(start_paused = true)]
async fn each_attempt_waits_one_reply_window_and_a_reply_ends_its_candidate() {
    let answering = Ipv4Addr::new(10, 0, 0, 1);
    let silent = Ipv4Addr::new(10, 0, 0, 2);
    let device = device_ids(1)[0];
    let network = Arc::new(FakeNetwork::replying(move |destination, _| {
        if destination == at(answering) {
            vec![Reply::Datagram {
                delay: Duration::from_millis(199),
                source: destination,
                bytes: reply_frame(device),
            }]
        } else {
            Vec::new()
        }
    }));
    let (_gate, authority) = ready();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.0.0.0/30")),
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    let answered = network.sends_to(at(answering));
    assert_eq!(answered.len(), 1, "an accepted identity needs no retry");
    assert_eq!(
        network.closed_at(at(answering)) - answered[0],
        Duration::from_millis(199)
    );
    let unanswered = network.sends_to(at(silent));
    assert_eq!(unanswered.len(), 2);
    assert!(unanswered[1] - unanswered[0] >= TypedSubnetScope::REPLY_WINDOW);
    assert_eq!(
        network.closed_at(at(silent)) - unanswered[1],
        TypedSubnetScope::REPLY_WINDOW
    );
    assert_eq!(report.report.observations.len(), 1);
    let observation = &report.report.observations[0];
    assert_eq!(observation.device_id.get(), device);
    assert_eq!(observation.source, at(answering));
    assert_eq!(observation.method, DiscoveryMethod::TypedSubnet);
    assert_eq!(observation.interface, None);
    assert_eq!(report.requests_attempted, 3);
}

#[tokio::test(start_paused = true)]
async fn a_candidate_reads_sixteen_datagrams_and_accepts_one_identity() {
    let flooded = Ipv4Addr::new(172, 16, 0, 0);
    let answering = Ipv4Addr::new(172, 16, 0, 1);
    let ids = device_ids(2);
    let replies = ids.clone();
    let network = Arc::new(FakeNetwork::replying(move |destination, _| {
        let ids = &replies;
        if destination == at(flooded) {
            (1..=20)
                .map(|index| Reply::Datagram {
                    delay: Duration::from_millis(index),
                    // Replies from any other address are rejected but read.
                    source: at(Ipv4Addr::new(10, 9, 9, 9)),
                    bytes: reply_frame(ids[0]),
                })
                .collect()
        } else {
            ids.iter()
                .zip(1..)
                .map(|(id, delay)| Reply::Datagram {
                    delay: Duration::from_millis(delay),
                    source: destination,
                    bytes: reply_frame(*id),
                })
                .collect()
        }
    }));
    let (_gate, authority) = ready();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("172.16.0.0/31")),
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    assert_eq!(
        network.sends_to(at(flooded)).len(),
        1,
        "no retry after the receive limit"
    );
    assert_eq!(network.sends_to(at(answering)).len(), 1);
    let stats = report.report.stats;
    assert!(stats.receive_limit_reached);
    assert_eq!(stats.datagrams_received, 16 + 1);
    assert_eq!(stats.datagrams_rejected, 16);
    assert_eq!(stats.datagrams_accepted, 1);
    assert_eq!(report.report.observations.len(), 1);
    assert_eq!(report.report.observations[0].device_id.get(), ids[0]);
    assert_eq!(report.report.observations[0].source, at(answering));
}

#[tokio::test(start_paused = true)]
async fn reaching_the_device_limit_reports_incomplete_and_sends_no_more() {
    let ids = device_ids(254);
    let network = Arc::new(FakeNetwork::replying(move |destination, _| {
        let SocketAddr::V4(address) = destination else {
            unreachable!()
        };
        let host = usize::from(address.ip().octets()[3]);
        vec![Reply::Datagram {
            delay: Duration::from_millis(1),
            source: destination,
            bytes: reply_frame(ids[host - 1]),
        }]
    }));
    let (_gate, authority) = ready();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.1.1.0/24")),
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::DeviceLimit)
    );
    assert_eq!(
        report.report.observations.len(),
        TypedSubnetScope::MAX_DEVICES
    );
    assert!(report.report.stats.device_limit_reached);
    assert_eq!(network.sends().len(), TypedSubnetScope::MAX_DEVICES);
    assert_eq!(report.requests_attempted, TypedSubnetScope::MAX_DEVICES);
    assert_eq!(network.open.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn devices_beyond_the_limit_from_late_probes_are_dropped() {
    let (_gate, authority) = ready();
    let context = test_context(authority, ScanPlan::typed(scope("10.0.0.0/23")), 4);
    let mut aggregate = Aggregate::default();
    for (index, id) in device_ids(TypedSubnetScope::MAX_DEVICES + 2)
        .into_iter()
        .enumerate()
    {
        let candidate = Ipv4Addr::new(10, 0, 0, u8::try_from(index + 1).unwrap());
        let endpoint = subnet_endpoint(at(candidate));
        let observation = validated_observation(&endpoint, at(candidate), &reply_frame(id), None);
        let mut report = DiscoveryReport::default();
        report.observations.extend(observation);
        aggregate.merge(
            Ok(CandidateResult {
                candidate,
                report,
                issue: None,
            }),
            &context,
        );
    }
    assert_eq!(aggregate.devices.len(), TypedSubnetScope::MAX_DEVICES);
    assert_eq!(
        aggregate.report.observations.len(),
        TypedSubnetScope::MAX_DEVICES
    );
    assert!(aggregate.report.stats.device_limit_reached);
    assert_eq!(context.reason(), Some(SubnetScanIncomplete::DeviceLimit));
}

#[tokio::test(start_paused = true)]
async fn deadline_expiry_is_incomplete_and_nothing_is_sent_after_it() {
    let mut network = FakeNetwork::silent();
    // Slow readiness stretches the paced budget past the deadline.
    network.ready_delay = Duration::from_millis(30);
    let network = Arc::new(network);
    let (_gate, authority) = ready();
    let started = Instant::now();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.0.0.0/23")),
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::Deadline)
    );
    assert!(report.report.observations.is_empty());
    assert_ne!(
        report.outcome,
        SubnetScanOutcome::Complete,
        "never a complete empty search"
    );
    let sends = network.sends();
    assert!(!sends.is_empty() && sends.len() < 1_020);
    assert!(
        sends
            .iter()
            .all(|(instant, _)| *instant - started < TypedSubnetScope::DEADLINE)
    );
    assert_eq!(report.requests_attempted, sends.len());
    assert_eq!(network.open.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn revocation_after_readiness_prevents_every_send_including_a_retry() {
    // (scope, the readiness at which authority is revoked). The /32 case
    // revokes the candidate's retry; the others revoke first attempts.
    for (text, revoked_at) in [
        ("10.2.0.0/24", 1),
        ("10.2.0.0/24", 2),
        ("10.2.0.0/24", 3),
        ("10.2.0.0/24", 17),
        ("10.2.0.0/24", 40),
        ("10.2.0.7/32", 2),
    ] {
        for network_change in [false, true] {
            let (gate, authority) = ready();
            let cancellation = CancellationToken::new();
            let mut network = FakeNetwork::silent();
            let revoking_gate = gate.clone();
            let revoking_token = cancellation.clone();
            network.on_ready = Box::new(move |readiness| {
                if readiness == revoked_at {
                    if network_change {
                        revoking_gate.invalidate();
                    } else {
                        revoking_token.cancel();
                    }
                }
            });
            let network = Arc::new(network);
            let report = run(
                &network,
                &lane(0),
                ScanPlan::typed(scope(text)),
                authority,
                &cancellation,
            )
            .await;

            let expected = if network_change {
                SubnetScanIncomplete::NetworkChanged
            } else {
                SubnetScanIncomplete::Cancelled
            };
            assert_eq!(
                report.outcome,
                SubnetScanOutcome::Incomplete(expected),
                "{text} {revoked_at}"
            );
            assert_eq!(network.sends().len(), revoked_at - 1, "{text} {revoked_at}");
            assert_eq!(report.requests_attempted, revoked_at - 1);
            assert_eq!(network.open.load(Ordering::SeqCst), 0, "every probe joined");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_network_change_mid_scan_stops_joins_and_never_revives_the_permit() {
    let network = Arc::new(FakeNetwork::silent());
    let (gate, authority) = ready();
    let stale = authority.clone();
    let started = Instant::now();
    let changer = {
        let gate = gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            gate.invalidate();
        })
    };
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.3.0.0/24")),
        authority,
        &CancellationToken::new(),
    )
    .await;
    changer.await.unwrap();

    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::NetworkChanged)
    );
    let sends = network.sends();
    assert!(!sends.is_empty());
    assert!(
        sends
            .iter()
            .all(|(instant, _)| *instant - started < Duration::from_secs(1))
    );
    assert_eq!(network.open.load(Ordering::SeqCst), 0);

    // The topology returns as a new generation; the old authority stays dead.
    gate.establish();
    let quiet = Arc::new(FakeNetwork::silent());
    let report = run(
        &quiet,
        &lane(0),
        ScanPlan::typed(scope("10.3.0.0/24")),
        stale,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::NetworkChanged)
    );
    assert!(quiet.sends().is_empty());
    assert_eq!(
        quiet.max_open.load(Ordering::SeqCst),
        0,
        "no probe was admitted"
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_searches_share_one_pacing_boundary_and_one_lane() {
    let shared = lane(u64::MAX);
    let network = Arc::new(FakeNetwork::silent());
    let (_gate, authority) = ready();
    let cancellation = CancellationToken::new();
    let canceller = {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            cancellation.cancel();
        })
    };
    let busy = async {
        tokio::task::yield_now().await;
        scan(
            Arc::clone(&shared),
            &FakeTransport(Arc::new(FakeNetwork::silent())),
            ScanPlan::typed(scope("10.4.0.9/32")),
            authority.clone(),
            &CancellationToken::new(),
        )
        .await
    };
    let (first, second) = tokio::join!(
        run(
            &network,
            &shared,
            ScanPlan::typed(scope("10.4.0.9/32")),
            authority.clone(),
            &cancellation,
        ),
        busy
    );
    canceller.await.unwrap();
    assert_eq!(
        second.unwrap_err(),
        SubnetScanError::Busy,
        "one search per lane"
    );
    assert_eq!(
        first.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::Cancelled)
    );
    let previous = network.sends()[0].0;

    // The successor starts at once, but its first send waits for the
    // boundary its predecessor left behind rather than bursting.
    let successor = Arc::new(FakeNetwork::silent());
    let report = run(
        &successor,
        &shared,
        ScanPlan::typed(scope("10.4.0.9/32")),
        authority,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    let gap = successor.sends()[0].0 - previous;
    assert!(gap >= MIN + MAX_JITTER && gap < MIN + MAX_JITTER + TIMER_GRANULARITY);
}

#[tokio::test(start_paused = true)]
async fn refused_sends_are_never_retried_and_refusing_hosts_end_quietly() {
    let refused = Ipv4Addr::new(10, 5, 0, 1);
    let resetting = Ipv4Addr::new(10, 5, 0, 2);
    let mut network = FakeNetwork::replying(move |destination, _| {
        if destination == at(resetting) {
            vec![Reply::Error {
                delay: Duration::from_millis(1),
                kind: io::ErrorKind::ConnectionReset,
            }]
        } else {
            Vec::new()
        }
    });
    network.send_error = Box::new(move |destination| {
        (destination == at(refused)).then_some(io::ErrorKind::PermissionDenied)
    });
    let network = Arc::new(network);
    let (_gate, authority) = ready();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.5.0.0/30")),
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    assert_eq!(network.sends_to(at(refused)).len(), 1);
    assert_eq!(network.sends_to(at(resetting)).len(), 1);
    assert_eq!(report.refused_sends, 1);
    assert_eq!(
        report.requests_attempted, 2,
        "a refused send still spent budget"
    );
    assert_eq!(report.report.stats.datagrams_sent, 1);
    assert_eq!(report.report.issues.len(), 1);
    let issue = &report.report.issues[0];
    assert_eq!(issue.endpoint.destination, at(refused));
    assert_eq!(issue.endpoint.method, DiscoveryMethod::TypedSubnet);
    assert_eq!(issue.class, ProbeFailureClass::Network);
    assert!(issue.message.contains("not retried"));
}

#[tokio::test(start_paused = true)]
async fn a_local_directed_broadcast_is_refused_before_the_socket_on_every_platform() {
    let local = Ipv4Addr::new(10, 8, 0, 255);
    let network = Arc::new(FakeNetwork::silent());
    let (_gate, authority) = ready();
    let plan = ScanPlan::typed(scope("10.8.0.0/23")).with_local_broadcasts([local].into());
    let report = run(
        &network,
        &lane(0),
        plan,
        authority,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    assert!(
        network.sends_to(at(local)).is_empty(),
        "never handed to the socket"
    );
    assert_eq!(report.refused_sends, 1);
    assert_eq!(
        report.requests_attempted,
        1_020 - 1,
        "refused once, never retried"
    );
    assert_eq!(network.sends().len(), 1_020 - 2);
    assert_eq!(report.report.issues.len(), 1);
    assert_eq!(report.report.issues[0].endpoint.destination, at(local));
}

#[tokio::test(start_paused = true)]
async fn failed_sockets_are_issues_and_other_candidates_continue() {
    struct Failing;
    impl SubnetTransport for Failing {
        type Socket = FakeSocket;
        fn open(&self) -> io::Result<FakeSocket> {
            Err(io::ErrorKind::AddrNotAvailable.into())
        }
    }
    let (_gate, authority) = ready();
    let report = scan(
        lane(0),
        &Failing,
        ScanPlan::typed(scope("10.6.0.0/30")),
        authority.clone(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, SubnetScanOutcome::Complete);
    assert_eq!(report.requests_attempted, 0);
    assert_eq!(report.report.issues.len(), 2);

    let mut network = FakeNetwork::silent();
    network.send_error = Box::new(|_| Some(io::ErrorKind::HostUnreachable));
    let network = Arc::new(network);
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.6.0.0/30")),
        authority,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(report.refused_sends, 0);
    assert_eq!(report.requests_attempted, 2);
    assert!(
        report
            .report
            .issues
            .iter()
            .all(|issue| issue.message.contains("HostUnreachable"))
    );

    let network = Arc::new(FakeNetwork::replying(|_, _| {
        vec![Reply::Error {
            delay: Duration::from_millis(1),
            kind: io::ErrorKind::Other,
        }]
    }));
    let (_gate, authority) = ready();
    let report = run(
        &network,
        &lane(0),
        ScanPlan::typed(scope("10.6.0.4/32")),
        authority,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(report.report.issues.len(), 1);
    assert!(report.report.issues[0].message.contains("receiving"));
}

fn test_context(authority: Authority, plan: ScanPlan, budget: usize) -> ScanContext {
    ScanContext {
        lane: lane(0),
        plan,
        authority,
        stop: CancellationToken::new(),
        cancellation: CancellationToken::new(),
        reason: Mutex::new(None),
        deadline: Instant::now() + TypedSubnetScope::DEADLINE,
        budget: AtomicUsize::new(budget),
        attempted: AtomicUsize::new(0),
        refused: AtomicUsize::new(0),
        request: encode_tuner_discover_request(None).unwrap(),
    }
}

#[tokio::test(start_paused = true)]
async fn every_send_rechecks_scope_budget_and_each_authority_condition() {
    let (gate, authority) = ready();
    let plan = ScanPlan::typed(scope("10.7.0.0/24"));
    let network = Arc::new(FakeNetwork::silent());
    let socket = FakeTransport(Arc::clone(&network)).open().unwrap();

    // Outside the scope, the network or broadcast address, or another port.
    let context = test_context(authority.clone(), plan, 2);
    for destination in [
        at(Ipv4Addr::new(10, 7, 1, 1)),
        at(Ipv4Addr::new(10, 7, 0, 0)),
        at(Ipv4Addr::new(10, 7, 0, 255)),
        SocketAddr::from((Ipv4Addr::new(10, 7, 0, 1), 9)),
        "[fd00::1]:65001".parse().unwrap(),
    ] {
        assert!(matches!(
            context.send(&socket, destination).await,
            SendOutcome::OutOfScope
        ));
    }
    assert!(network.sends().is_empty());
    assert!(matches!(
        context.send(&socket, at(Ipv4Addr::new(10, 7, 0, 1))).await,
        SendOutcome::Sent(_)
    ));

    // A spent budget refuses the send at the boundary.
    let spent = test_context(authority.clone(), ScanPlan::typed(scope("10.7.0.0/24")), 0);
    assert_eq!(spent.refusal(), Some(SubnetScanIncomplete::RequestBudget));
    assert!(matches!(
        spent.send(&socket, at(Ipv4Addr::new(10, 7, 0, 2))).await,
        SendOutcome::Halted
    ));
    assert_eq!(spent.reason(), Some(SubnetScanIncomplete::RequestBudget));

    let late = test_context(authority.clone(), ScanPlan::typed(scope("10.7.0.0/24")), 2);
    tokio::time::advance(TypedSubnetScope::DEADLINE).await;
    assert_eq!(late.refusal(), Some(SubnetScanIncomplete::Deadline));

    let stopped = test_context(authority.clone(), ScanPlan::typed(scope("10.7.0.0/24")), 2);
    stopped.halt(SubnetScanIncomplete::DeviceLimit);
    stopped.halt(SubnetScanIncomplete::Deadline);
    assert_eq!(stopped.refusal(), Some(SubnetScanIncomplete::DeviceLimit));

    let cancelled = test_context(authority.clone(), ScanPlan::typed(scope("10.7.0.0/24")), 2);
    cancelled.cancellation.cancel();
    assert_eq!(cancelled.refusal(), Some(SubnetScanIncomplete::Cancelled));
    assert_eq!(
        cancelled
            .reason()
            .unwrap_or(SubnetScanIncomplete::Cancelled),
        SubnetScanIncomplete::Cancelled
    );

    gate.invalidate();
    let revoked = test_context(authority, ScanPlan::typed(scope("10.7.0.0/24")), 2);
    assert_eq!(
        revoked.refusal(),
        Some(SubnetScanIncomplete::NetworkChanged)
    );
    assert_eq!(network.sends().len(), 1);
}

#[test]
fn usable_hosts_follow_the_entered_prefix() {
    let point_to_point = ScanPlan::typed(scope("172.16.0.0/31"));
    assert!(point_to_point.admits(at(Ipv4Addr::new(172, 16, 0, 0))));
    assert!(point_to_point.admits(at(Ipv4Addr::new(172, 16, 0, 1))));
    let one = ScanPlan::typed(scope("192.168.9.9/32"));
    assert!(one.admits(at(Ipv4Addr::new(192, 168, 9, 9))));
    assert_eq!(one.candidates, [Ipv4Addr::new(192, 168, 9, 9)]);
    let limited = ScanPlan {
        network: "255.255.255.255/32".parse().unwrap(),
        candidates: vec![Ipv4Addr::BROADCAST],
        port: DISCOVERY_UDP_PORT,
        local_broadcasts: BTreeSet::new(),
    };
    assert!(
        !limited.admits(at(Ipv4Addr::BROADCAST)),
        "never a limited broadcast"
    );
}

#[test]
fn consent_binds_the_displayed_budget_and_its_generation() {
    let typed = scope("192.168.2.0/23");
    let gate = ObservationGate::new();
    let watch = gate.watch();
    gate.establish();
    let generation = gate.state().generation().unwrap();

    for (candidates, budget) in [(510, 1_021), (509, 1_020), (0, 0)] {
        assert_eq!(
            SubnetSearchConsent::confirm(typed, candidates, budget, generation).unwrap_err(),
            SubnetConsentError::BudgetMismatch
        );
    }
    let consent = SubnetSearchConsent::confirm(typed, 510, 1_020, generation).unwrap();
    assert_eq!(consent.scope(), typed);
    assert_eq!(consent.generation(), generation);
    assert!(!format!("{consent:?}").contains("192.168"));
    let permit = consent.admit(&watch).unwrap();
    assert!(permit.is_live());
    assert_eq!(permit.scope(), typed);
    assert_eq!(permit.generation(), generation);
    assert!(!format!("{permit:?}").contains("192.168"));

    // A change revokes the permit; the network returning does not restore it.
    gate.invalidate();
    assert!(!permit.is_live());
    let unavailable = SubnetSearchConsent::confirm(typed, 510, 1_020, generation).unwrap();
    assert_eq!(
        unavailable.admit(&watch).unwrap_err(),
        SubnetAdmissionError::ObservationUnavailable
    );
    gate.establish();
    assert!(!permit.is_live());
    let stale = SubnetSearchConsent::confirm(typed, 510, 1_020, generation).unwrap();
    assert_eq!(
        stale.admit(&watch).unwrap_err(),
        SubnetAdmissionError::Stale
    );
}

#[tokio::test]
async fn the_production_entry_sends_nothing_without_live_uncancelled_authority() {
    let _serial = PROCESS_LANE_TESTS.lock().await;
    let typed = scope("192.168.254.0/24");
    let gate = ObservationGate::new();
    gate.establish();
    let generation = gate.state().generation().unwrap();
    let admit = || {
        SubnetSearchConsent::confirm(typed, 254, 508, generation)
            .unwrap()
            .admit(&gate.watch())
            .unwrap()
    };

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let report = discover_typed_subnet(admit(), &cancelled).await.unwrap();
    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::Cancelled)
    );
    assert_eq!(report.requests_attempted, 0);
    assert_eq!(report.report.stats.probes_started, 0);

    let permit = admit();
    gate.invalidate();
    let report = discover_typed_subnet(permit, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        report.outcome,
        SubnetScanOutcome::Incomplete(SubnetScanIncomplete::NetworkChanged)
    );
    assert_eq!(report.requests_attempted, 0);
}

/// Native loopback fixtures: a real responder socket and the production
/// socket settings, with an optional hook at each write readiness.
mod native {
    use std::net::UdpSocket as StdUdpSocket;
    use std::sync::mpsc as std_mpsc;
    use std::thread;

    use super::*;

    /// A loopback responder that reports every request it receives and, when
    /// `device` is set, answers each with a valid reply.
    struct Responder {
        address: SocketAddr,
        requests: std_mpsc::Receiver<()>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Responder {
        fn start(device: Option<u32>) -> Self {
            let socket = StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_millis(20)))
                .unwrap();
            let address = socket.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let (report, requests) = std_mpsc::channel();
            let running = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                let mut buffer = [0_u8; 64];
                while !running.load(Ordering::SeqCst) {
                    let Ok((_, client)) = socket.recv_from(&mut buffer) else {
                        continue;
                    };
                    let _ = report.send(());
                    if let Some(device) = device {
                        let _ = socket.send_to(&reply_frame(device), client);
                    }
                }
            });
            Self {
                address,
                requests,
                stop,
                thread: Some(thread),
            }
        }

        fn plan(&self) -> ScanPlan {
            let SocketAddr::V4(address) = self.address else {
                unreachable!()
            };
            ScanPlan {
                network: Ipv4Net::new(*address.ip(), 32).unwrap(),
                candidates: vec![*address.ip()],
                port: address.port(),
                local_broadcasts: BTreeSet::new(),
            }
        }

        /// Requests received so far, waiting briefly for stragglers.
        fn received(&self) -> usize {
            let mut count = 0;
            while self
                .requests
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_ok()
            {
                count += 1;
            }
            count
        }
    }

    impl Drop for Responder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Production sockets whose readiness runs a hook and whose sends are
    /// timed.
    struct Hooked {
        readiness: Arc<AtomicUsize>,
        on_ready: Arc<dyn Fn(usize) + Send + Sync>,
        sends: Arc<Mutex<Vec<Instant>>>,
    }

    struct HookedSocket {
        inner: UdpSocket,
        readiness: Arc<AtomicUsize>,
        on_ready: Arc<dyn Fn(usize) + Send + Sync>,
        sends: Arc<Mutex<Vec<Instant>>>,
    }

    impl Hooked {
        fn new(on_ready: impl Fn(usize) + Send + Sync + 'static) -> Self {
            Self {
                readiness: Arc::new(AtomicUsize::new(0)),
                on_ready: Arc::new(on_ready),
                sends: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl SubnetTransport for Hooked {
        type Socket = HookedSocket;

        fn open(&self) -> io::Result<HookedSocket> {
            let inner = open_system_socket()?;
            assert!(!socket2_broadcast(&inner), "broadcast stays disabled");
            Ok(HookedSocket {
                inner,
                readiness: Arc::clone(&self.readiness),
                on_ready: Arc::clone(&self.on_ready),
                sends: Arc::clone(&self.sends),
            })
        }
    }

    fn socket2_broadcast(socket: &UdpSocket) -> bool {
        socket.broadcast().unwrap()
    }

    impl SubnetSocket for HookedSocket {
        async fn writable(&self) -> io::Result<()> {
            self.inner.writable().await?;
            let readiness = self.readiness.fetch_add(1, Ordering::SeqCst) + 1;
            (self.on_ready)(readiness);
            Ok(())
        }

        fn try_send_to(&self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
            let sent = self.inner.try_send_to(datagram, destination);
            self.sends.lock().unwrap().push(Instant::now());
            sent
        }

        fn recv_from(
            &self,
            buffer: &mut [u8],
        ) -> impl Future<Output = io::Result<(usize, SocketAddr)>> + Send {
            self.inner.recv_from(buffer)
        }
    }

    #[tokio::test]
    async fn a_native_reply_is_accepted_once_without_a_retry() {
        let device = device_ids(1)[0];
        let responder = Responder::start(Some(device));
        let (_gate, authority) = ready();
        let report = scan(
            lane(0),
            &SystemTransport,
            responder.plan(),
            authority,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, SubnetScanOutcome::Complete);
        assert_eq!(report.requests_attempted, 1);
        assert_eq!(report.report.observations.len(), 1);
        let observation = &report.report.observations[0];
        assert_eq!(observation.device_id.get(), device);
        assert_eq!(observation.source, responder.address);
        assert_eq!(observation.method, DiscoveryMethod::TypedSubnet);
        assert_eq!(responder.received(), 1);
    }

    #[tokio::test]
    async fn a_native_silent_host_gets_one_paced_retry() {
        let responder = Responder::start(None);
        let (_gate, authority) = ready();
        let hooked = Hooked::new(|_| {});
        let report = scan(
            lane(0),
            &hooked,
            responder.plan(),
            authority,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(report.outcome, SubnetScanOutcome::Complete);
        assert_eq!(report.requests_attempted, 2);
        let sends = hooked.sends.lock().unwrap().clone();
        assert_eq!(sends.len(), 2);
        assert!(sends[1] - sends[0] >= TypedSubnetScope::REPLY_WINDOW);
        assert_eq!(responder.received(), 2);
    }

    #[tokio::test]
    async fn native_revocation_after_readiness_sends_nothing_more() {
        for network_change in [false, true] {
            let responder = Responder::start(None);
            let (gate, authority) = ready();
            let cancellation = CancellationToken::new();
            let revoking = cancellation.clone();
            // The retry's readiness completes, then authority is revoked.
            let hooked = Hooked::new(move |readiness| {
                if readiness == 2 {
                    if network_change {
                        gate.invalidate();
                    } else {
                        revoking.cancel();
                    }
                }
            });
            let report = scan(lane(0), &hooked, responder.plan(), authority, &cancellation)
                .await
                .unwrap();

            let expected = if network_change {
                SubnetScanIncomplete::NetworkChanged
            } else {
                SubnetScanIncomplete::Cancelled
            };
            assert_eq!(report.outcome, SubnetScanOutcome::Incomplete(expected));
            assert_eq!(report.requests_attempted, 1);
            assert_eq!(hooked.sends.lock().unwrap().len(), 1);
            assert_eq!(responder.received(), 1);
        }
    }

    /// A local interface's directed broadcast, the kind of interior address
    /// a typed subnet can contain, as the first `(interface network, address)`
    /// found. `None` where no interface has an IPv4 broadcast address.
    fn local_directed_broadcast() -> Option<Ipv4Addr> {
        if_addrs::get_if_addrs()
            .ok()?
            .into_iter()
            .find_map(|interface| match interface.addr {
                if_addrs::IfAddr::V4(address)
                    if interface.is_oper_up()
                        && !address.ip.is_loopback()
                        && address.prefixlen <= 30 =>
                {
                    address.broadcast
                }
                _ => None,
            })
    }

    /// The operating system refuses a broadcast destination on a socket that
    /// never enabled broadcasting, and the refusal widens nothing. The
    /// limited broadcast is refused everywhere (macOS reports it
    /// unreachable). Windows sends a directed broadcast without the option,
    /// so the runner refuses a local one itself: offered as an ordinary
    /// candidate, it is refused once, never reaches the socket, and is never
    /// retried, on every platform.
    #[tokio::test]
    async fn native_broadcast_sends_are_refused_with_broadcast_disabled() {
        let socket = open_system_socket().unwrap();
        assert!(!socket.broadcast().unwrap());
        let error = socket
            .send_to(b"balun", SocketAddr::from((Ipv4Addr::BROADCAST, 9)))
            .await
            .expect_err("a broadcast send needs broadcasting enabled");
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::HostUnreachable
            ),
            "{error:?}"
        );
        assert!(!socket.broadcast().unwrap(), "no permission was widened");

        // A sandbox without a broadcast-capable interface has nothing to test.
        let Some(broadcast) = local_directed_broadcast() else {
            return;
        };
        // Linux and macOS refuse it themselves; Windows would send it.
        #[cfg(not(windows))]
        {
            let error = socket
                .send_to(b"balun", SocketAddr::from((broadcast, 9)))
                .await
                .expect_err("a directed broadcast needs broadcasting enabled");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error:?}");
        }
        let plan = ScanPlan {
            // Any network that holds the address as an interior host.
            network: Ipv4Net::new(broadcast, 1).unwrap().trunc(),
            candidates: vec![broadcast],
            port: 9,
            local_broadcasts: local_directed_broadcasts(),
        };
        assert!(plan.admits(SocketAddr::from((broadcast, 9))));
        let (_gate, authority) = ready();
        let hooked = Hooked::new(|_| {});
        let report = scan(lane(0), &hooked, plan, authority, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.outcome, SubnetScanOutcome::Complete);
        assert_eq!(report.requests_attempted, 1, "refused once, never retried");
        assert_eq!(report.refused_sends, 1, "{:?}", report.report.issues);
        assert_eq!(report.report.stats.datagrams_sent, 0);
        assert!(
            hooked.sends.lock().unwrap().is_empty(),
            "it never reached the socket"
        );
        assert!(report.report.issues[0].message.contains("not retried"));
    }
}
