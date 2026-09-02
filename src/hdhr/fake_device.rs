//! Loopback fake HDHomeRun device for end-to-end tests.
//!
//! One fake owns the fixed discovery responder port (65_001), the fixed
//! MPEG-TS stream port (5_004), and one ephemeral metadata port, so at most
//! one [`FakeHdhrDevice`] can exist per process at a time. Test runs in
//! separate processes must be serialized by the caller; `cargo test` runs
//! every test in one process, where the in-file port lock is sufficient.
//! Nothing here contacts an external network, resolves a name, or opens a
//! tuner.

use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::http::MetadataPortOverride;
use super::protocol::{
    DEVICE_TYPE_TUNER, FRAME_OVERHEAD, MAX_PACKET_SIZE, TAG_BASE_URL, TAG_DEVICE_ID,
    TAG_DEVICE_TYPE, TAG_LINEUP_URL, TAG_TUNER_COUNT, TYPE_DISCOVER_REPLY, TYPE_DISCOVER_REQUEST,
};
use super::test_support::response;
use crate::domain::DeviceId;

/// Serializes possession of the fixed discovery (65_001) and stream (5_004)
/// ports across every test in this process for each fake device's lifetime.
/// Cross-process test runs must be serialized by the caller; `cargo test`
/// executes every test in one process, where this lock suffices.
static FAKE_DEVICE_PORTS: Mutex<()> = Mutex::new(());

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const DISCOVERY_PORT: u16 = 65_001;
const STREAM_PORT: u16 = 5_004;
const DISCOVERY_TARGET: SocketAddr = SocketAddr::new(LOOPBACK, DISCOVERY_PORT);
const STREAM_TARGET: SocketAddr = SocketAddr::new(LOOPBACK, STREAM_PORT);

const UDP_READ_TIMEOUT: Duration = Duration::from_millis(50);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const METADATA_REQUEST_DEADLINE: Duration = Duration::from_secs(2);
const STREAM_REQUEST_DEADLINE: Duration = Duration::from_secs(3);
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const STREAM_WRITE_CHUNK: usize = 16 * 1_024;
const STREAM_READ_TIMEOUT: Duration = Duration::from_millis(100);
const CLIENT_CLOSE_CAP: Duration = Duration::from_secs(10);
const REPEAT_PACE: Duration = Duration::from_millis(5);
const MAX_REQUEST_BYTES: usize = 16 * 1_024;
const MAX_CONCURRENT_STREAM_HANDLERS: usize = 8;

/// The checked-in, video-only synthetic MPEG-2 transport stream.
const FIXTURE_BYTES: &[u8] = include_bytes!("../../tests/fixtures/synthetic-mpeg2.ts");

/// Checksum-valid synthetic HDHomeRun identity matching the golden fixtures.
pub(crate) const FAKE_DEVICE_ID: u32 = 0x105A_1232;

/// Return the fake device identity every harness instance advertises.
pub(crate) fn fake_device_id() -> DeviceId {
    DeviceId::new(FAKE_DEVICE_ID).expect("the fake device ID is checksum-valid")
}

/// Hold the process-wide fake-device port lock for the caller's lifetime. A
/// test that panicked while holding it owns no ports anymore, so a poisoned
/// lock carries no stale state. Sibling test-only port-policy checks also use
/// this lock so they never race a concurrently bound fake device.
pub(crate) fn hold_fake_device_ports() -> MutexGuard<'static, ()> {
    FAKE_DEVICE_PORTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How the fake stream server shapes one channel's HTTP body.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FakeStreamBody {
    /// Serve the checked-in synthetic MPEG-TS fixture once, then close the
    /// body like a completed transfer.
    FixtureOnce,
    /// Serve fixture bytes in an open-ended loop until the client disconnects.
    FixtureRepeat,
}

/// One channel row served through `/lineup.json` and `/auto/v<guide_number>`.
#[derive(Clone, Copy)]
pub(crate) struct FakeChannelSpec {
    /// Guide number, e.g. `"5.1"`, matched through `/auto/v<guide_number>`.
    pub(crate) guide_number: &'static str,
    /// Guide name served in the `/lineup.json` row.
    pub(crate) guide_name: &'static str,
    /// Whether the row advertises `DRM: 1`.
    pub(crate) drm: bool,
    /// Body shape this channel serves.
    pub(crate) body: FakeStreamBody,
}

/// One recorded stream-server lifecycle transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StreamEventKind {
    /// The handler parsed a stream request path.
    Connected,
    /// The handler finished serving the connection.
    Closed,
}

/// One recorded stream request or teardown.
#[derive(Clone, Debug)]
pub(crate) struct StreamEvent {
    /// Request path, e.g. `"/auto/v5.1"`.
    pub(crate) path: String,
    /// Recorded lifecycle transition.
    pub(crate) kind: StreamEventKind,
    /// When the event was recorded.
    pub(crate) at: Instant,
}

/// A loopback fake HDHomeRun device: a UDP discovery responder on
/// `127.0.0.1:65_001`, an HTTP metadata server on an ephemeral loopback port,
/// and an HTTP MPEG-TS stream server on `127.0.0.1:5_004`.
pub(crate) struct FakeHdhrDevice {
    device_id: DeviceId,
    discovery_target: SocketAddr,
    stop: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<StreamEvent>>>,
    metadata_paths: Arc<Mutex<Vec<String>>>,
    udp_worker: Option<JoinHandle<()>>,
    metadata_worker: Option<JoinHandle<()>>,
    stream_accept_worker: Option<JoinHandle<()>>,
    stream_workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    _port_override: MetadataPortOverride,
    _port_lock: MutexGuard<'static, ()>,
}

impl FakeHdhrDevice {
    /// Bind the fixed discovery (65_001) and stream (5_004) ports plus one
    /// ephemeral metadata port, and install the test-only metadata-port
    /// override for the device's lifetime. Panics when either fixed port is
    /// already held inside this process; the in-file lock serializes
    /// concurrent tests, and cross-process runs must be serialized by the
    /// caller.
    pub(crate) fn start(tuner_count: u8, channels: &[FakeChannelSpec]) -> Self {
        let port_lock = hold_fake_device_ports();

        let discovery_socket =
            UdpSocket::bind(DISCOVERY_TARGET).expect("bind the fake discovery responder");
        let metadata_listener =
            TcpListener::bind((LOOPBACK, 0)).expect("bind the fake metadata server");
        metadata_listener
            .set_nonblocking(true)
            .expect("make the fake metadata listener nonblocking");
        let metadata_port = metadata_listener
            .local_addr()
            .expect("read the fake metadata address")
            .port();
        let stream_listener =
            TcpListener::bind(STREAM_TARGET).expect("bind the fake stream server");
        stream_listener
            .set_nonblocking(true)
            .expect("make the fake stream listener nonblocking");

        let port_override = MetadataPortOverride::install(metadata_port);
        let reply = encode_discover_reply(
            FAKE_DEVICE_ID,
            tuner_count,
            &format!("http://127.0.0.1:{metadata_port}"),
            &format!("http://127.0.0.1:{metadata_port}/lineup.json"),
        );

        let stop = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let metadata_paths = Arc::new(Mutex::new(Vec::new()));
        let stream_workers = Arc::new(Mutex::new(Vec::new()));

        let udp_worker = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || answer_discovery(discovery_socket, &reply, &stop))
        };
        let metadata_worker = {
            let stop = Arc::clone(&stop);
            let paths = Arc::clone(&metadata_paths);
            let channels = channels.to_vec();
            thread::spawn(move || {
                serve_metadata_connections(
                    &metadata_listener,
                    tuner_count,
                    &channels,
                    metadata_port,
                    &paths,
                    &stop,
                )
            })
        };
        let stream_accept_worker = {
            let stop = Arc::clone(&stop);
            let connection_events = Arc::clone(&events);
            let workers = Arc::clone(&stream_workers);
            let channels = channels.to_vec();
            thread::spawn(move || {
                accept_stream_connections(
                    &stream_listener,
                    &channels,
                    &connection_events,
                    &workers,
                    &stop,
                )
            })
        };

        Self {
            device_id: fake_device_id(),
            discovery_target: DISCOVERY_TARGET,
            stop,
            events,
            metadata_paths,
            udp_worker: Some(udp_worker),
            metadata_worker: Some(metadata_worker),
            stream_accept_worker: Some(stream_accept_worker),
            stream_workers,
            _port_override: port_override,
            _port_lock: port_lock,
        }
    }

    /// Return the fake device identity.
    pub(crate) fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Always `127.0.0.1:65_001`; pass to `DiscoveryClient::discover_target`.
    pub(crate) fn discovery_target(&self) -> SocketAddr {
        self.discovery_target
    }

    /// Snapshot of recorded metadata request paths in order, e.g.
    /// `["/discover.json", "/lineup.json"]`.
    pub(crate) fn metadata_paths(&self) -> Vec<String> {
        self.metadata_paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Snapshot of recorded stream events in order.
    pub(crate) fn stream_events(&self) -> Vec<StreamEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Poll until `predicate` holds over the current stream events or
    /// `timeout` expires; return whether it held. Polls every ~10ms and never
    /// panics on its own.
    pub(crate) fn wait_for_stream_events(
        &self,
        timeout: Duration,
        predicate: impl Fn(&[StreamEvent]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate(&self.stream_events()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(WAIT_POLL_INTERVAL);
        }
    }
}

impl Drop for FakeHdhrDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.udp_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.metadata_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.stream_accept_worker.take() {
            let _ = worker.join();
        }
        let mut workers = self
            .stream_workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn answer_discovery(socket: UdpSocket, reply: &[u8], stop: &AtomicBool) {
    let _ = socket.set_read_timeout(Some(UDP_READ_TIMEOUT));
    let mut buffer = [0_u8; MAX_PACKET_SIZE];
    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((received, peer)) => {
                if received >= 2
                    && u16::from_be_bytes([buffer[0], buffer[1]]) == TYPE_DISCOVER_REQUEST
                {
                    let _ = socket.send_to(reply, peer);
                }
            }
            Err(error) if is_timeout(&error) => {}
            Err(_) => return,
        }
    }
}

fn serve_metadata_connections(
    listener: &TcpListener,
    tuner_count: u8,
    channels: &[FakeChannelSpec],
    metadata_port: u16,
    paths: &Mutex<Vec<String>>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                serve_metadata_request(
                    &mut stream,
                    tuner_count,
                    channels,
                    metadata_port,
                    paths,
                    stop,
                );
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => return,
        }
    }
}

fn serve_metadata_request(
    stream: &mut TcpStream,
    tuner_count: u8,
    channels: &[FakeChannelSpec],
    metadata_port: u16,
    paths: &Mutex<Vec<String>>,
    stop: &Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(METADATA_REQUEST_DEADLINE));
    let request = read_request(stream, METADATA_REQUEST_DEADLINE, stop);
    let Some(path) = request_path(&request) else {
        return;
    };
    paths
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(path.to_owned());

    let bytes = match path {
        "/discover.json" => json_response(&discover_json(tuner_count, metadata_port)),
        "/lineup.json" => json_response(&lineup_json(channels)),
        _ => response("404 Not Found", &[("Content-Length", "0".to_owned())], b""),
    };
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
}

fn accept_stream_connections(
    listener: &TcpListener,
    channels: &[FakeChannelSpec],
    events: &Arc<Mutex<Vec<StreamEvent>>>,
    workers: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    stop: &Arc<AtomicBool>,
) {
    let live_handlers = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if live_handlers.load(Ordering::Acquire) >= MAX_CONCURRENT_STREAM_HANDLERS {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                live_handlers.fetch_add(1, Ordering::AcqRel);
                let handler_stop = Arc::clone(stop);
                let handler_events = Arc::clone(events);
                let handler_live = Arc::clone(&live_handlers);
                let handler_channels = channels.to_vec();
                let handler = thread::spawn(move || {
                    serve_stream_connection(
                        stream,
                        &handler_channels,
                        &handler_events,
                        &handler_stop,
                    );
                    handler_live.fetch_sub(1, Ordering::AcqRel);
                });
                workers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(handler);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => return,
        }
    }
}

fn serve_stream_connection(
    mut stream: TcpStream,
    channels: &[FakeChannelSpec],
    events: &Mutex<Vec<StreamEvent>>,
    stop: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(STREAM_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(STREAM_WRITE_TIMEOUT));
    let request = read_request(&mut stream, STREAM_REQUEST_DEADLINE, stop);
    let Some(path) = request_path(&request).map(str::to_owned) else {
        return;
    };
    record_event(events, &path, StreamEventKind::Connected);

    if let Some(spec) = channels
        .iter()
        .find(|spec| format!("/auto/v{}", spec.guide_number) == path)
    {
        match spec.body {
            FakeStreamBody::FixtureOnce => serve_fixture_once(&mut stream, stop),
            FakeStreamBody::FixtureRepeat => serve_fixture_repeat(&mut stream, stop),
        }
    } else {
        let bytes = response("404 Not Found", &[("Content-Length", "0".to_owned())], b"");
        let _ = stream.write_all(&bytes);
        let _ = stream.flush();
    }

    record_event(events, &path, StreamEventKind::Closed);
}

fn serve_fixture_once(stream: &mut TcpStream, stop: &AtomicBool) {
    let mut bytes = response(
        "200 OK",
        &[
            ("Content-Type", "video/mpeg".to_owned()),
            ("Content-Length", FIXTURE_BYTES.len().to_string()),
        ],
        b"",
    );
    bytes.extend_from_slice(FIXTURE_BYTES);
    if stream.write_all(&bytes).is_err() || stream.flush().is_err() {
        return;
    }
    let _ = stream.shutdown(Shutdown::Write);

    let deadline = Instant::now() + CLIENT_CLOSE_CAP;
    let mut scratch = [0_u8; 256];
    while Instant::now() < deadline && !stop.load(Ordering::Acquire) {
        match stream.read(&mut scratch) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if is_timeout(&error) => {}
            Err(_) => return,
        }
    }
}

fn serve_fixture_repeat(stream: &mut TcpStream, stop: &AtomicBool) {
    let head = response("200 OK", &[("Content-Type", "video/mpeg".to_owned())], b"");
    if stream.write_all(&head).is_err() || stream.flush().is_err() {
        return;
    }

    let mut scratch = [0_u8; 256];
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut scratch) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if is_timeout(&error) => {}
            Err(_) => return,
        }
        let mut written = 0;
        while written < FIXTURE_BYTES.len() {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let end = (written + STREAM_WRITE_CHUNK).min(FIXTURE_BYTES.len());
            match stream.write(&FIXTURE_BYTES[written..end]) {
                Ok(0) => return,
                Ok(count) => written += count,
                Err(error) if is_timeout(&error) => {}
                Err(_) => return,
            }
        }
        thread::sleep(REPEAT_PACE);
    }
}

fn read_request(stream: &mut TcpStream, deadline: Duration, stop: &AtomicBool) -> Vec<u8> {
    let deadline = Instant::now() + deadline;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1_024];
    while request.len() <= MAX_REQUEST_BYTES
        && !stop.load(Ordering::Acquire)
        && Instant::now() < deadline
    {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error) if is_timeout(&error) => {}
            Err(_) => break,
        }
    }
    request
}

fn request_path(request: &[u8]) -> Option<&str> {
    let request = std::str::from_utf8(request).ok()?;
    let line = request.lines().next()?;
    let after_method = line.strip_prefix("GET ")?;
    let path = after_method.split(' ').next()?;
    (!path.is_empty()).then_some(path)
}

fn record_event(events: &Mutex<Vec<StreamEvent>>, path: &str, kind: StreamEventKind) {
    events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(StreamEvent {
            path: path.to_owned(),
            kind,
            at: Instant::now(),
        });
}

fn json_response(body: &str) -> Vec<u8> {
    response(
        "200 OK",
        &[
            ("Content-Type", "application/json".to_owned()),
            ("Content-Length", body.len().to_string()),
        ],
        body.as_bytes(),
    )
}

fn discover_json(tuner_count: u8, metadata_port: u16) -> String {
    serde_json::json!({
        "DeviceID": format!("{FAKE_DEVICE_ID:08X}"),
        "FriendlyName": "Loopback Fake Tuner",
        "ModelNumber": "HDHR4-FAKE",
        "FirmwareName": "fakefw",
        "FirmwareVersion": "20260902",
        "TunerCount": tuner_count,
        "LineupURL": format!("http://127.0.0.1:{metadata_port}/lineup.json"),
    })
    .to_string()
}

fn lineup_json(channels: &[FakeChannelSpec]) -> String {
    let rows = channels
        .iter()
        .map(|spec| {
            let mut row = serde_json::json!({
                "GuideNumber": spec.guide_number,
                "GuideName": spec.guide_name,
                "URL": format!("http://127.0.0.1:{STREAM_PORT}/auto/v{}", spec.guide_number),
            });
            if spec.drm {
                row["DRM"] = serde_json::json!(1);
            }
            row
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(rows).to_string()
}

fn encode_discover_reply(
    device_id: u32,
    tuner_count: u8,
    base_url: &str,
    lineup_url: &str,
) -> Vec<u8> {
    let mut payload = Vec::new();
    push_u32_tlv(&mut payload, TAG_DEVICE_TYPE, DEVICE_TYPE_TUNER);
    push_u32_tlv(&mut payload, TAG_DEVICE_ID, device_id);
    push_tlv(&mut payload, TAG_TUNER_COUNT, &[tuner_count]);
    push_tlv(&mut payload, TAG_BASE_URL, base_url.as_bytes());
    push_tlv(&mut payload, TAG_LINEUP_URL, lineup_url.as_bytes());

    let payload_length = u16::try_from(payload.len())
        .expect("the fake discovery payload stays far below the u16 maximum");
    let mut frame = Vec::with_capacity(payload.len() + FRAME_OVERHEAD);
    frame.extend_from_slice(&TYPE_DISCOVER_REPLY.to_be_bytes());
    frame.extend_from_slice(&payload_length.to_be_bytes());
    frame.extend_from_slice(&payload);
    let crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());
    frame
}

fn push_u32_tlv(payload: &mut Vec<u8>, tag: u8, value: u32) {
    push_tlv(payload, tag, &value.to_be_bytes());
}

fn push_tlv(payload: &mut Vec<u8>, tag: u8, value: &[u8]) {
    payload.push(tag);
    if value.len() < 0x80 {
        payload.push(value.len() as u8);
    } else {
        payload.push(0x80 | (value.len() & 0x7F) as u8);
        payload.push((value.len() >> 7) as u8);
    }
    payload.extend_from_slice(value);
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DeviceRegistry, DiscoveryClient, ProbeConfig, RegistryInstant};
    use crate::hdhr::protocol::parse_tuner_discover_response;
    use crate::hdhr::{DeviceEndpoint, DeviceHttpClient};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn encode_discover_reply_matches_the_wire_format() {
        let base_url = "http://127.0.0.1:8080";
        let lineup_url = "http://127.0.0.1:8080/lineup.json";
        let frame = encode_discover_reply(FAKE_DEVICE_ID, 3, base_url, lineup_url);

        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x00, 0x03, 0x00, 0x49]);
        expected.extend_from_slice(&[0x01, 0x04, 0x00, 0x00, 0x00, 0x01]);
        expected.extend_from_slice(&[0x02, 0x04]);
        expected.extend_from_slice(&FAKE_DEVICE_ID.to_be_bytes());
        expected.extend_from_slice(&[0x10, 0x01, 0x03]);
        expected.extend_from_slice(&[0x2A, 0x15]);
        expected.extend_from_slice(base_url.as_bytes());
        expected.extend_from_slice(&[0x27, 0x21]);
        expected.extend_from_slice(lineup_url.as_bytes());
        let crc = crc32fast::hash(&expected);
        expected.extend_from_slice(&crc.to_le_bytes());

        assert_eq!(frame, expected);
        assert_eq!(frame.len(), 81);

        let parsed = parse_tuner_discover_response(&frame).expect("the fake reply should parse");
        assert_eq!(parsed.device_id, FAKE_DEVICE_ID);
        assert_eq!(parsed.tuner_count, Some(3));
        assert_eq!(parsed.base_url.as_deref(), Some(base_url));
        assert_eq!(parsed.lineup_url.as_deref(), Some(lineup_url));
        assert!(parsed.device_types.contains(&DEVICE_TYPE_TUNER));

        let long_base = format!("http://127.0.0.1:8080/{}", "x".repeat(178));
        assert_eq!(long_base.len(), 200);
        let frame = encode_discover_reply(FAKE_DEVICE_ID, 1, &long_base, lineup_url);

        assert_eq!(frame[19], TAG_BASE_URL);
        assert_eq!(frame[20], 0x80 | (200 & 0x7F) as u8);
        assert_eq!(frame[21], (200 >> 7) as u8);
        assert_eq!(&frame[22..222], long_base.as_bytes());

        let parsed =
            parse_tuner_discover_response(&frame).expect("the two-byte-length reply should parse");
        assert_eq!(parsed.base_url.as_deref(), Some(long_base.as_str()));
    }

    #[tokio::test]
    async fn fake_device_answers_targeted_discovery_and_metadata() {
        let device = FakeHdhrDevice::start(
            2,
            &[
                FakeChannelSpec {
                    guide_number: "5.1",
                    guide_name: "FAKE FIVE",
                    drm: false,
                    body: FakeStreamBody::FixtureOnce,
                },
                FakeChannelSpec {
                    guide_number: "7.1",
                    guide_name: "FAKE DRM",
                    drm: true,
                    body: FakeStreamBody::FixtureOnce,
                },
            ],
        );

        let probe = DiscoveryClient::new(
            ProbeConfig::new(1, Duration::from_millis(200), 16, 4)
                .expect("the fixed probe budget is valid"),
        );
        let report = probe
            .discover_target(device.discovery_target(), None, &CancellationToken::new())
            .await
            .expect("the fake responder should answer targeted discovery");
        assert_eq!(report.observations.len(), 1);
        let observation = report.observations[0].clone();

        assert_eq!(observation.device_id, fake_device_id());
        assert_eq!(observation.source, device.discovery_target());
        assert_eq!(observation.tuner_count, Some(2));
        let base_url = observation
            .advertised_base_url
            .as_deref()
            .expect("the fake responder advertises a base URL");
        let lineup_url = observation
            .advertised_lineup_url
            .as_deref()
            .expect("the fake responder advertises a lineup URL");
        assert_eq!(lineup_url, format!("{base_url}/lineup.json"));

        let endpoint = DeviceEndpoint::from_discovery(
            device.discovery_target(),
            Some(base_url),
            Some(lineup_url),
        )
        .expect("the advertised URLs satisfy the endpoint policy");
        let cancellation = CancellationToken::new();
        let http = DeviceHttpClient::default();
        let info = http
            .fetch_device_info(&endpoint, device.device_id(), &cancellation)
            .await
            .expect("identity-checked discover.json should parse");
        assert_eq!(info.device_id(), device.device_id());
        assert_eq!(info.tuner_count(), Some(2));
        assert_eq!(info.model_number(), Some("HDHR4-FAKE"));

        let lineup = http
            .get_lineup_json(&endpoint, &cancellation)
            .await
            .expect("lineup.json should parse");
        let lineup: serde_json::Value =
            serde_json::from_slice(&lineup).expect("lineup.json is valid JSON");
        let rows = lineup.as_array().expect("lineup.json is an array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["GuideNumber"], "5.1");
        assert_eq!(rows[0]["URL"], "http://127.0.0.1:5004/auto/v5.1");
        assert_eq!(rows[0].get("DRM"), None);
        assert_eq!(rows[1]["GuideNumber"], "7.1");
        assert_eq!(rows[1]["DRM"], 1);

        let mut registry = DeviceRegistry::default();
        registry
            .observe(observation, RegistryInstant::from_duration(Duration::ZERO))
            .expect("a fresh observation registers");
        let claim = registry
            .get(device.device_id())
            .and_then(|registered| registered.preferred_locator())
            .expect("the observation installs a locator");
        assert!(DeviceEndpoint::from_locator(claim).is_ok());

        assert_eq!(
            device.metadata_paths(),
            vec!["/discover.json".to_owned(), "/lineup.json".to_owned()]
        );
    }

    #[test]
    fn fixture_once_stream_serves_the_body_and_records_close() {
        let device = FakeHdhrDevice::start(
            1,
            &[FakeChannelSpec {
                guide_number: "5.1",
                guide_name: "FAKE FIVE",
                drm: false,
                body: FakeStreamBody::FixtureOnce,
            }],
        );

        let mut stream =
            TcpStream::connect(STREAM_TARGET).expect("connect to the fake stream server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bound the test client read");
        stream
            .write_all(b"GET /auto/v5.1 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\n\r\n")
            .expect("send the stream request");

        let head = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mpeg\r\nContent-Length: {}\r\n\r\n",
            FIXTURE_BYTES.len()
        );
        let mut expected = head.into_bytes();
        expected.extend_from_slice(FIXTURE_BYTES);
        let received = read_until(&mut stream, expected.len(), Duration::from_secs(10));
        assert_eq!(received, expected);
        drop(stream);

        let observed = device.wait_for_stream_events(Duration::from_secs(5), |events| {
            let connected = events.iter().position(|event| {
                event.path == "/auto/v5.1" && event.kind == StreamEventKind::Connected
            });
            let closed = events.iter().position(|event| {
                event.path == "/auto/v5.1" && event.kind == StreamEventKind::Closed
            });
            connected.is_some_and(|connected| closed.is_some_and(|closed| closed > connected))
        });
        assert!(observed);
        let events = device.stream_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].path, "/auto/v5.1");
        assert_eq!(events[0].kind, StreamEventKind::Connected);
        assert_eq!(events[1].path, "/auto/v5.1");
        assert_eq!(events[1].kind, StreamEventKind::Closed);
    }

    #[test]
    fn fixture_repeat_stream_serves_until_the_client_disconnects() {
        let device = FakeHdhrDevice::start(
            1,
            &[FakeChannelSpec {
                guide_number: "7.1",
                guide_name: "FAKE REPEAT",
                drm: false,
                body: FakeStreamBody::FixtureRepeat,
            }],
        );

        let mut stream =
            TcpStream::connect(STREAM_TARGET).expect("connect to the fake stream server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bound the test client read");
        stream
            .write_all(b"GET /auto/v7.1 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\n\r\n")
            .expect("send the stream request");

        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mpeg\r\n\r\n";
        let head_length = head.len();
        let wanted = head_length + 2 * FIXTURE_BYTES.len();
        let received = read_until(&mut stream, wanted, Duration::from_secs(10));
        assert!(received.len() >= wanted);
        assert!(received.starts_with(head.as_bytes()));
        assert_eq!(
            &received[head_length..head_length + FIXTURE_BYTES.len()],
            FIXTURE_BYTES
        );

        drop(stream);
        let observed = device.wait_for_stream_events(Duration::from_secs(5), |events| {
            events
                .iter()
                .any(|event| event.path == "/auto/v7.1" && event.kind == StreamEventKind::Closed)
        });
        assert!(observed);
    }

    fn read_until(stream: &mut TcpStream, wanted: usize, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut received = Vec::with_capacity(wanted);
        let mut buffer = [0_u8; 4_096];
        while received.len() < wanted && Instant::now() < deadline {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => received.extend_from_slice(&buffer[..count]),
                Err(error) if is_timeout(&error) => {}
                Err(_) => break,
            }
        }
        received
    }
}
