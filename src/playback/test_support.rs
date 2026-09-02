//! Loopback HTTP fixtures for transport tests.
//!
//! Every server binds `127.0.0.1:0`, serves exactly one client connection, and
//! records the raw request plus the instant the client disconnected. Nothing
//! here contacts an external network, resolves a name, or opens a tuner.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The checked-in, video-only synthetic MPEG-2 transport stream.
pub(super) const FIXTURE_BYTES: &[u8] = include_bytes!("../../tests/fixtures/synthetic-mpeg2.ts");

const ACCEPT_DEADLINE: Duration = Duration::from_secs(10);
const REQUEST_DEADLINE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(20);
const MAX_REQUEST_BYTES: usize = 16 * 1_024;

/// What the server does after writing its scripted response.
pub(super) enum StreamBehavior {
    /// Close the connection immediately, like a completed HTTP body.
    Close,
    /// Keep the connection open without sending more data until the client
    /// disconnects or the server stops.
    Hold,
    /// Keep writing the body until the client disconnects or the server stops.
    Repeat(Vec<u8>),
}

pub(super) struct FixtureStreamServer {
    address: SocketAddr,
    requests: Receiver<Vec<u8>>,
    disconnects: Receiver<Instant>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FixtureStreamServer {
    pub(super) fn start(response: Vec<u8>, behavior: StreamBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback stream server");
        listener
            .set_nonblocking(true)
            .expect("make loopback stream listener nonblocking");
        let address = listener.local_addr().expect("loopback stream address");
        let (request_sender, requests) = mpsc::channel();
        let (disconnect_sender, disconnects) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);

        let worker = thread::spawn(move || {
            let Some(mut stream) = accept_one(&listener, &worker_stop) else {
                return;
            };
            let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
            let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
            let _ = request_sender.send(read_request(&mut stream, &worker_stop));
            if write_fully(&mut stream, &response, &worker_stop).is_err() {
                let _ = disconnect_sender.send(Instant::now());
                return;
            }
            match behavior {
                StreamBehavior::Close => {
                    let _ = stream.shutdown(Shutdown::Both);
                }
                StreamBehavior::Hold => {
                    let mut scratch = [0_u8; 256];
                    while !worker_stop.load(Ordering::Acquire) {
                        match stream.read(&mut scratch) {
                            Ok(0) => {
                                let _ = disconnect_sender.send(Instant::now());
                                return;
                            }
                            Ok(_) => {}
                            Err(error) if is_timeout(&error) => {}
                            Err(_) => {
                                let _ = disconnect_sender.send(Instant::now());
                                return;
                            }
                        }
                    }
                }
                StreamBehavior::Repeat(body) => {
                    while !worker_stop.load(Ordering::Acquire) {
                        if write_fully(&mut stream, &body, &worker_stop).is_err() {
                            let _ = disconnect_sender.send(Instant::now());
                            return;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        });

        Self {
            address,
            requests,
            disconnects,
            stop,
            worker: Some(worker),
        }
    }

    /// A responder-shaped stream URL. The port is the loopback listener's, so
    /// this is only valid through the crate-private test handoff.
    pub(super) fn stream_url(&self) -> String {
        format!("http://{}/auto/v5.1", self.address)
    }

    pub(super) fn request(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.requests.recv_timeout(timeout).ok()
    }

    pub(super) fn client_disconnected_within(&self, timeout: Duration) -> bool {
        self.disconnects.recv_timeout(timeout).is_ok()
    }
}

impl Drop for FixtureStreamServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Accepts every connection, counts it, and closes it without reading.
pub(super) struct ConnectionTrap {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ConnectionTrap {
    pub(super) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback trap");
        listener
            .set_nonblocking(true)
            .expect("make loopback trap nonblocking");
        let address = listener.local_addr().expect("loopback trap address");
        let connections = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_connections = Arc::clone(&connections);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        worker_connections.fetch_add(1, Ordering::AcqRel);
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            address,
            connections,
            stop,
            worker: Some(worker),
        }
    }

    pub(super) const fn address(&self) -> SocketAddr {
        self.address
    }

    pub(super) fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }
}

impl Drop for ConnectionTrap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A loopback address whose listener has already been closed.
pub(super) fn closed_port_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback placeholder");
    let address = listener.local_addr().expect("loopback placeholder address");
    drop(listener);
    address
}

pub(super) fn http_response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body);
    response
}

/// A complete `200 OK` response carrying the checked-in fixture.
pub(super) fn fixture_response() -> Vec<u8> {
    http_response(
        "200 OK",
        &[
            ("Content-Type", "video/mpeg".to_owned()),
            ("Content-Length", FIXTURE_BYTES.len().to_string()),
        ],
        FIXTURE_BYTES,
    )
}

/// A `200 OK` head without a declared length, so the body ends at close.
pub(super) fn open_ended_response_head() -> Vec<u8> {
    http_response("200 OK", &[("Content-Type", "video/mpeg".to_owned())], b"")
}

fn accept_one(listener: &TcpListener, stop: &AtomicBool) -> Option<TcpStream> {
    let deadline = Instant::now() + ACCEPT_DEADLINE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => return None,
        }
    }
}

fn read_request(stream: &mut TcpStream, stop: &AtomicBool) -> Vec<u8> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
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

fn write_fully(stream: &mut TcpStream, bytes: &[u8], stop: &AtomicBool) -> std::io::Result<()> {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "server stopped",
            ));
        }
        match stream.write(remaining) {
            Ok(0) => return Err(std::io::Error::new(ErrorKind::WriteZero, "socket closed")),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if is_timeout(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
