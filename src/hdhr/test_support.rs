use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(crate) struct ScriptedResponse {
    pub(crate) bytes: Vec<u8>,
    pub(crate) delay: Duration,
}

impl ScriptedResponse {
    pub(crate) fn immediate(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            delay: Duration::ZERO,
        }
    }

    pub(crate) fn delayed(bytes: impl Into<Vec<u8>>, delay: Duration) -> Self {
        Self {
            bytes: bytes.into(),
            delay,
        }
    }
}

pub(crate) struct ScriptedHttpServer {
    address: SocketAddr,
    expected_requests: usize,
    requests: Receiver<Vec<u8>>,
    worker: Option<JoinHandle<()>>,
}

impl ScriptedHttpServer {
    /// Starts a loopback HTTP server that responds sequentially to requests.
    pub(crate) fn start(responses: Vec<ScriptedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP server");
        listener
            .set_nonblocking(true)
            .expect("make test HTTP listener nonblocking");
        let address = listener.local_addr().expect("test HTTP address");
        let expected_requests = responses.len();
        let (request_sender, requests) = mpsc::channel();

        let worker = thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => return,
                    }
                };
                // On Windows, accepted sockets inherit non-blocking status from the listener.
                if stream.set_nonblocking(false).is_err()
                    || stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .is_err()
                    || stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .is_err()
                {
                    return;
                }

                let mut request = Vec::new();
                let mut buffer = [0_u8; 1_024];
                let read_deadline = Instant::now() + Duration::from_secs(2);
                while request.len() < 16 * 1_024 && Instant::now() < read_deadline {
                    let read_len = (16 * 1_024 - request.len()).min(buffer.len());
                    match stream.read(&mut buffer[..read_len]) {
                        Ok(0) => break,
                        Ok(count) => {
                            request.extend_from_slice(&buffer[..count]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                ErrorKind::WouldBlock | ErrorKind::TimedOut
                            ) =>
                        {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
                let _ = request_sender.send(request);

                thread::sleep(response.delay);
                let _ = stream.write_all(&response.bytes);
                let _ = stream.flush();
                let _ = stream.shutdown(Shutdown::Both);
            }
        });

        Self {
            address,
            expected_requests,
            requests,
            worker: Some(worker),
        }
    }

    pub(crate) const fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn base_url(&self) -> String {
        format!("http://{}/", self.address)
    }

    pub(crate) fn finish(mut self) -> Vec<Vec<u8>> {
        let mut requests = Vec::with_capacity(self.expected_requests);
        for _ in 0..self.expected_requests {
            if let Ok(request) = self.requests.recv_timeout(Duration::from_secs(3)) {
                requests.push(request);
            }
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("test HTTP worker should not panic");
        }
        requests
    }
}

pub(crate) fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body);
    response
}
