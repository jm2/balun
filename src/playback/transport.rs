//! Balun-owned direct HTTP transport for the exact `appsrc` element.
//!
//! GStreamer receives only the constant, endpoint-free [`PIPELINE_URI`]. The
//! responder-pinned stream URL moves into one reader thread that owns a private
//! `reqwest` client with automatic and explicit proxies, redirects, and Referer
//! disabled. A small bounded channel separates asynchronous body reads from a
//! dedicated blocking feeder, so neither the GTK main context nor the
//! controller runtime can block on GStreamer backpressure. HTTP status and
//! transport failures reduce immediately to fixed playback categories that are
//! posted as one field-bounded application message on the pipeline bus.

use std::net::IpAddr;
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use reqwest::{Url, redirect};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::pipeline_failure::PlaybackPipelineFailure;
use crate::controller::StreamHandoff;

/// The only URI Balun assigns to `playbin3`. It names no endpoint.
pub const PIPELINE_URI: &str = "appsrc://balun";

/// Largest single buffer pushed into `appsrc`; larger HTTP chunks are split.
pub(super) const MAX_PUSH_BYTES: usize = 64 * 1_024;
pub(super) const TRANSPORT_FAILURE_MESSAGE: &str = "balun-transport-failure";
pub(super) const TRANSPORT_FAILURE_FIELD: &str = "category";

const PUSH_BUFFER_SIGNAL: &str = "push-buffer";
const END_OF_STREAM_SIGNAL: &str = "end-of-stream";
const USER_AGENT: &str = concat!("Balun/", env!("CARGO_PKG_VERSION"));
const READER_THREAD_NAME: &str = "balun-stream-reader";
const FEEDER_THREAD_NAME: &str = "balun-stream-feeder";
const FEED_QUEUE_CAPACITY: usize = 8;
const RUNTIME_SHUTDOWN_BOUND: Duration = Duration::from_secs(1);
const FAILED_START_JOIN_BOUND: Duration = Duration::from_secs(1);

/// Bounded deadlines for one live stream request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TransportConfig {
    connect_timeout: Duration,
    response_timeout: Duration,
    read_timeout: Duration,
}

impl TransportConfig {
    /// Production deadlines: connect, response headers, and idle body reads.
    pub(super) const PRODUCTION: Self = Self {
        connect_timeout: Duration::from_secs(5),
        response_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(10),
    };

    #[cfg(test)]
    pub(super) const fn new(
        connect_timeout: Duration,
        response_timeout: Duration,
        read_timeout: Duration,
    ) -> Self {
        Self {
            connect_timeout,
            response_timeout,
            read_timeout,
        }
    }
}

/// Fixed, endpoint-free reason the transport could not start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransportStartError {
    /// The handoff URI was not a credential-free numeric-host HTTP URL.
    InvalidHandoff,
    /// The source did not expose the exact `appsrc` feed signals.
    SignalSchema,
    /// A worker thread could not be created.
    Thread,
}

/// The owned workers did not finish inside the teardown bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TransportJoinError;

enum FeedItem {
    Data(gst::Buffer),
    End,
}

enum ReaderStop {
    Cancelled,
    Failed(PlaybackPipelineFailure),
}

/// One live stream request bound to exactly one `appsrc` element.
///
/// Dropping the transport cancels the request. Ordinary teardown should call
/// [`Self::cancel`] before the pipeline leaves `PLAYING` and [`Self::join`]
/// after it reaches `NULL`, because only a flushing `appsrc` can unblock a
/// feeder waiting on the bounded byte limit.
pub(super) struct StreamTransport {
    cancellation: CancellationToken,
    reader: WorkerHandle,
    feeder: WorkerHandle,
}

impl StreamTransport {
    pub(super) fn start(
        handoff: StreamHandoff,
        source: gst::Element,
        pipeline: &gst::Pipeline,
        config: TransportConfig,
    ) -> Result<Self, TransportStartError> {
        validate_feed_signals(&source)?;
        // The handoff is consumed and zeroized here. Only the parsed URL lives
        // on, and only inside the reader thread's private state.
        let url = handoff
            .with_uri(parse_private_stream_url)
            .ok_or(TransportStartError::InvalidHandoff)?;
        let cancellation = CancellationToken::new();
        let (feed_sender, feed_receiver) = mpsc::channel(FEED_QUEUE_CAPACITY);
        let failure_sink = FailureSink {
            pipeline: pipeline.downgrade(),
        };
        let feeder = WorkerHandle::spawn(FEEDER_THREAD_NAME, move || {
            run_feeder(&source, feed_receiver);
        })?;
        let reader_cancellation = cancellation.clone();
        let reader = WorkerHandle::spawn(READER_THREAD_NAME, move || {
            run_reader(
                url,
                config,
                &reader_cancellation,
                feed_sender,
                &failure_sink,
            );
        });
        match reader {
            Ok(reader) => Ok(Self {
                cancellation,
                reader,
                feeder,
            }),
            Err(error) => {
                // The unspawned closure dropped the feed sender, so the feeder
                // observes a closed channel and exits without emitting EOS.
                let mut feeder = feeder;
                cancellation.cancel();
                let _ = feeder.join_until(Instant::now() + FAILED_START_JOIN_BOUND);
                Err(error)
            }
        }
    }

    /// Stop reading and close the device connection as soon as possible.
    pub(super) fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Wait for both workers. Call after the pipeline reached `NULL`.
    pub(super) fn join(&mut self, deadline: Instant) -> Result<(), TransportJoinError> {
        self.cancellation.cancel();
        let reader = self.reader.join_until(deadline);
        let feeder = self.feeder.join_until(deadline);
        reader.and(feeder)
    }
}

impl Drop for StreamTransport {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct WorkerHandle {
    finished: std_mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    fn spawn(
        name: &str,
        work: impl FnOnce() + Send + 'static,
    ) -> Result<Self, TransportStartError> {
        let (finished_sender, finished) = std_mpsc::channel();
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _finished = finished_sender;
                work();
            })
            .map_err(|_| TransportStartError::Thread)?;
        Ok(Self {
            finished,
            thread: Some(thread),
        })
    }

    fn join_until(&mut self, deadline: Instant) -> Result<(), TransportJoinError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.finished.recv_timeout(remaining) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                thread.join().map_err(|_| TransportJoinError)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.thread = Some(thread);
                Err(TransportJoinError)
            }
        }
    }
}

struct FailureSink {
    pipeline: glib::WeakRef<gst::Pipeline>,
}

impl FailureSink {
    fn post(&self, failure: PlaybackPipelineFailure) {
        let Some(pipeline) = self.pipeline.upgrade() else {
            return;
        };
        let Some(bus) = pipeline.bus() else {
            return;
        };
        let structure = gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
            .field(TRANSPORT_FAILURE_FIELD, failure.code())
            .build();
        let message = gst::message::Application::builder(structure)
            .src(&pipeline)
            .build();
        let _ = bus.post(message);
    }
}

fn parse_private_stream_url(uri: &str) -> Option<Url> {
    let url = Url::parse(uri).ok()?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return None;
    }
    let host = url.host_str()?;
    let numeric = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    // A numeric host guarantees the request never resolves a name.
    numeric.parse::<IpAddr>().ok()?;
    Some(url)
}

fn validate_feed_signals(source: &gst::Element) -> Result<(), TransportStartError> {
    let element_type = source.type_();
    let push = glib::subclass::SignalId::lookup(PUSH_BUFFER_SIGNAL, element_type)
        .ok_or(TransportStartError::SignalSchema)?
        .query();
    let end = glib::subclass::SignalId::lookup(END_OF_STREAM_SIGNAL, element_type)
        .ok_or(TransportStartError::SignalSchema)?
        .query();
    let push_parameters = push.param_types();
    if push.signal_name() != PUSH_BUFFER_SIGNAL
        || !push.flags().contains(glib::SignalFlags::ACTION)
        || push.return_type() != gst::FlowReturn::static_type()
        || push_parameters.len() != 1
        || push_parameters[0] != gst::Buffer::static_type()
        || end.signal_name() != END_OF_STREAM_SIGNAL
        || !end.flags().contains(glib::SignalFlags::ACTION)
        || end.return_type() != gst::FlowReturn::static_type()
        || !end.param_types().is_empty()
    {
        return Err(TransportStartError::SignalSchema);
    }
    Ok(())
}

fn run_reader(
    url: Url,
    config: TransportConfig,
    cancellation: &CancellationToken,
    feed: mpsc::Sender<FeedItem>,
    failure_sink: &FailureSink,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build();
    let Ok(runtime) = runtime else {
        failure_sink.post(PlaybackPipelineFailure::Internal);
        return;
    };
    let outcome = runtime.block_on(stream_body(url, config, cancellation, &feed));
    // Drop the sender before the runtime so the feeder cannot outlive the
    // response socket when the reader stops for any reason.
    drop(feed);
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_BOUND);
    match outcome {
        Ok(()) | Err(ReaderStop::Cancelled) => {}
        Err(ReaderStop::Failed(failure)) => failure_sink.post(failure),
    }
}

async fn stream_body(
    url: Url,
    config: TransportConfig,
    cancellation: &CancellationToken,
    feed: &mpsc::Sender<FeedItem>,
) -> Result<(), ReaderStop> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(redirect::Policy::none())
        .referer(false)
        .user_agent(USER_AGENT)
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.read_timeout)
        .pool_max_idle_per_host(0)
        .http1_only()
        .build()
        .map_err(|_| ReaderStop::Failed(PlaybackPipelineFailure::Internal))?;
    let request = client.get(url).send();
    let mut response = tokio::select! {
        () = cancellation.cancelled() => return Err(ReaderStop::Cancelled),
        result = tokio::time::timeout(config.response_timeout, request) => match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) | Err(_) => {
                return Err(ReaderStop::Failed(PlaybackPipelineFailure::Offline));
            }
        },
    };
    // Only the numeric status is interpreted; reason phrases, headers, and
    // bodies of rejected responses are dropped with the response.
    match response.status().as_u16() {
        200 => {}
        503 => return Err(ReaderStop::Failed(PlaybackPipelineFailure::TunerBusy)),
        404 => return Err(ReaderStop::Failed(PlaybackPipelineFailure::ChannelMissing)),
        _ => return Err(ReaderStop::Failed(PlaybackPipelineFailure::HttpRejected)),
    }

    loop {
        let chunk = tokio::select! {
            () = cancellation.cancelled() => return Err(ReaderStop::Cancelled),
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(mut bytes)) => {
                while !bytes.is_empty() {
                    let piece = bytes.split_to(bytes.len().min(MAX_PUSH_BYTES));
                    send_feed(
                        feed,
                        cancellation,
                        FeedItem::Data(gst::Buffer::from_slice(piece)),
                    )
                    .await?;
                }
            }
            Ok(None) => {
                send_feed(feed, cancellation, FeedItem::End).await?;
                return Ok(());
            }
            Err(_) => return Err(ReaderStop::Failed(PlaybackPipelineFailure::Offline)),
        }
    }
}

async fn send_feed(
    feed: &mpsc::Sender<FeedItem>,
    cancellation: &CancellationToken,
    item: FeedItem,
) -> Result<(), ReaderStop> {
    tokio::select! {
        () = cancellation.cancelled() => Err(ReaderStop::Cancelled),
        // A closed channel means the feeder already stopped because the
        // pipeline stopped accepting data; there is nothing left to report.
        result = feed.send(item) => result.map_err(|_| ReaderStop::Cancelled),
    }
}

fn run_feeder(source: &gst::Element, mut feed: mpsc::Receiver<FeedItem>) {
    while let Some(item) = feed.blocking_recv() {
        match item {
            FeedItem::Data(buffer) => {
                let flow = source.emit_by_name::<gst::FlowReturn>(PUSH_BUFFER_SIGNAL, &[&buffer]);
                if flow != gst::FlowReturn::Ok {
                    return;
                }
            }
            FeedItem::End => {
                let _ = source.emit_by_name::<gst::FlowReturn>(END_OF_STREAM_SIGNAL, &[]);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process::Command;

    use super::*;
    use crate::controller::OperationGeneration;
    use crate::domain::{ChannelKey, DeviceId, GuideNumber};
    use crate::playback::source_policy;
    use crate::playback::test_support::{
        ConnectionTrap, FIXTURE_BYTES, FixtureStreamServer, StreamBehavior, closed_port_address,
        fixture_response, http_response, open_ended_response_head,
    };

    const PROXY_TRAP_CHILD_ENV: &str = "BALUN_PLAYBACK_PROXY_TRAP_CHILD";
    const PROXY_TRAP_CHILD_TEST: &str = "playback::transport::tests::proxy_trap_child";
    const SECRET_MARKERS: [&str; 4] = ["127.0.0.1", "/auto/v5.1", "http://", "user-secret"];
    const QUICK: TransportConfig = TransportConfig::new(
        Duration::from_millis(500),
        Duration::from_millis(1_500),
        Duration::from_millis(300),
    );

    struct FeedFixture {
        pipeline: gst::Pipeline,
        source: gst::Element,
        sink: gst::Element,
        bus: gst::Bus,
    }

    impl FeedFixture {
        /// A bare `appsrc ! fakesink` pipeline configured exactly as the
        /// production source policy configures the accepted element.
        fn new() -> Option<Self> {
            gst::init().ok()?;
            let pipeline = gst::Pipeline::new();
            let source = gst::ElementFactory::make("appsrc").build().ok()?;
            let sink = gst::ElementFactory::make("fakesink").build().ok()?;
            assert!(source_policy::configure_and_verify(&source));
            pipeline.add_many([&source, &sink]).ok()?;
            source.link(&sink).ok()?;
            let bus = pipeline.bus()?;
            Some(Self {
                pipeline,
                source,
                sink,
                bus,
            })
        }

        fn start(&self, handoff: StreamHandoff, config: TransportConfig) -> StreamTransport {
            StreamTransport::start(handoff, self.source.clone(), &self.pipeline, config)
                .expect("start the loopback transport")
        }

        fn wait_terminal(&self, timeout: Duration) -> Terminal {
            let Some(message) = self.bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(timeout.as_nanos() as u64),
                &[
                    gst::MessageType::Eos,
                    gst::MessageType::Error,
                    gst::MessageType::Application,
                ],
            ) else {
                return Terminal::Timeout;
            };
            match message.view() {
                gst::MessageView::Eos(_) => Terminal::Eos,
                gst::MessageView::Error(_) => Terminal::NativeError,
                gst::MessageView::Application(application) => {
                    let structure = application.structure().expect("marker structure");
                    assert_eq!(structure.name(), TRANSPORT_FAILURE_MESSAGE);
                    assert_eq!(structure.n_fields(), 1);
                    assert_eq!(
                        message.src(),
                        Some(self.pipeline.upcast_ref::<gst::Object>())
                    );
                    let rendered = structure.to_string();
                    for secret in SECRET_MARKERS {
                        assert!(!rendered.contains(secret), "{rendered}");
                    }
                    let code = structure.get::<u32>(TRANSPORT_FAILURE_FIELD).unwrap();
                    Terminal::Failure(
                        PlaybackPipelineFailure::from_code(code).expect("closed code"),
                    )
                }
                _ => unreachable!("filtered"),
            }
        }

        fn stop(&self, mut transport: StreamTransport) -> Result<(), TransportJoinError> {
            transport.cancel();
            self.pipeline.set_state(gst::State::Null).unwrap();
            let (transition, current, _) = self.pipeline.state(gst::ClockTime::from_seconds(5));
            assert!(transition.is_ok());
            assert_eq!(current, gst::State::Null);
            transport.join(Instant::now() + Duration::from_secs(5))
        }

        fn rendered_buffers(&self) -> u64 {
            self.sink
                .property::<gst::Structure>("stats")
                .get::<u64>("rendered")
                .unwrap_or(0)
        }
    }

    impl Drop for FeedFixture {
        fn drop(&mut self) {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Terminal {
        Eos,
        NativeError,
        Failure(PlaybackPipelineFailure),
        Timeout,
    }

    fn channel_key() -> ChannelKey {
        ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("5.1").unwrap(),
        )
    }

    fn handoff(url: &str) -> StreamHandoff {
        StreamHandoff::test_fixture(channel_key(), OperationGeneration::new(3), url)
    }

    #[test]
    fn private_stream_urls_require_direct_numeric_http_without_credentials() {
        for accepted in [
            "http://192.0.2.10:5004/auto/v5.1",
            "http://[2001:db8::10]:5004/auto/v5.1",
            "http://127.0.0.1:65001/auto/v5.1",
        ] {
            assert!(parse_private_stream_url(accepted).is_some(), "{accepted}");
        }
        for rejected in [
            "https://192.0.2.10:5004/auto/v5.1",
            "http://tuner.example:5004/auto/v5.1",
            "http://localhost:5004/auto/v5.1",
            "http://user@192.0.2.10:5004/auto/v5.1",
            "http://user:secret@192.0.2.10:5004/auto/v5.1",
            "http://192.0.2.10:5004/auto/v5.1?token=1",
            "http://192.0.2.10:5004/auto/v5.1#frag",
            "http://192.0.2.10:0/auto/v5.1",
            "file:///tmp/fixture.ts",
            "appsrc://balun",
            "not a url",
        ] {
            assert!(parse_private_stream_url(rejected).is_none(), "{rejected}");
        }
    }

    #[test]
    fn invalid_handoffs_and_foreign_sources_fail_to_start_without_threads() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        assert_eq!(
            StreamTransport::start(
                handoff("file:///tmp/fixture.ts"),
                fixture.source.clone(),
                &fixture.pipeline,
                QUICK,
            )
            .err(),
            Some(TransportStartError::InvalidHandoff)
        );
        let foreign = gst::ElementFactory::make("fakesrc").build().unwrap();
        assert_eq!(
            StreamTransport::start(
                handoff("http://127.0.0.1:9/auto/v5.1"),
                foreign,
                &fixture.pipeline,
                QUICK,
            )
            .err(),
            Some(TransportStartError::SignalSchema)
        );
    }

    #[test]
    fn complete_body_is_fed_in_bounded_pushes_and_ends_with_eos() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        let server = FixtureStreamServer::start(fixture_response(), StreamBehavior::Close);
        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let transport = fixture.start(handoff(&server.stream_url()), QUICK);

        assert_eq!(fixture.wait_terminal(Duration::from_secs(5)), Terminal::Eos);
        let request = String::from_utf8_lossy(&server.request(Duration::from_secs(3)).unwrap())
            .to_ascii_lowercase();
        assert!(request.starts_with("get /auto/v5.1 http/1.1\r\n"));
        assert!(request.contains("user-agent: balun/"));
        assert!(!request.contains("referer:"));
        assert!(!request.contains("accept-encoding:"));
        assert!(!request.contains("proxy-"));
        assert!(
            fixture.rendered_buffers() >= 1,
            "the sink must have received at least one pushed buffer"
        );
        assert_eq!(fixture.stop(transport), Ok(()));
    }

    #[test]
    fn oversized_chunks_are_split_to_the_fixed_maximum() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        let body = vec![0x47_u8; MAX_PUSH_BYTES * 3 + 17];
        let response = http_response(
            "200 OK",
            &[("Content-Length", body.len().to_string())],
            &body,
        );
        let server = FixtureStreamServer::start(response, StreamBehavior::Close);
        let largest = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&largest);
        let probe = fixture
            .source
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(buffer) = info.buffer() {
                    observed.fetch_max(buffer.size(), std::sync::atomic::Ordering::AcqRel);
                }
                gst::PadProbeReturn::Ok
            })
            .unwrap();
        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let transport = fixture.start(handoff(&server.stream_url()), QUICK);

        assert_eq!(fixture.wait_terminal(Duration::from_secs(5)), Terminal::Eos);
        fixture
            .source
            .static_pad("src")
            .unwrap()
            .remove_probe(probe);
        let largest = largest.load(std::sync::atomic::Ordering::Acquire);
        assert!(largest > 0 && largest <= MAX_PUSH_BYTES, "{largest}");
        assert_eq!(fixture.stop(transport), Ok(()));
    }

    #[test]
    fn http_statuses_map_to_fixed_categories_without_following_redirects() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        let trap = ConnectionTrap::start();
        for (status, headers, expected) in [
            (
                "503 Service Unavailable",
                vec![("Content-Length", "0".to_owned())],
                PlaybackPipelineFailure::TunerBusy,
            ),
            (
                "404 Not Found",
                vec![("Content-Length", "0".to_owned())],
                PlaybackPipelineFailure::ChannelMissing,
            ),
            (
                "500 Internal Server Error",
                vec![("Content-Length", "0".to_owned())],
                PlaybackPipelineFailure::HttpRejected,
            ),
            (
                "204 No Content",
                vec![],
                PlaybackPipelineFailure::HttpRejected,
            ),
            (
                "302 Found",
                vec![
                    ("Content-Length", "0".to_owned()),
                    ("Location", format!("http://{}/auto/v5.1", trap.address())),
                ],
                PlaybackPipelineFailure::HttpRejected,
            ),
        ] {
            let server = FixtureStreamServer::start(
                http_response(status, &headers, b""),
                StreamBehavior::Close,
            );
            fixture.pipeline.set_state(gst::State::Playing).unwrap();
            let transport = fixture.start(handoff(&server.stream_url()), QUICK);
            assert_eq!(
                fixture.wait_terminal(Duration::from_secs(5)),
                Terminal::Failure(expected),
                "{status}"
            );
            assert_eq!(fixture.stop(transport), Ok(()));
        }
        assert_eq!(
            trap.connections(),
            0,
            "a redirect target must never be contacted"
        );
    }

    #[test]
    fn refused_stalled_and_truncated_streams_are_offline() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        fixture.pipeline.set_state(gst::State::Playing).unwrap();

        let refused = handoff(&format!("http://{}/auto/v5.1", closed_port_address()));
        let transport = fixture.start(refused, QUICK);
        assert_eq!(
            fixture.wait_terminal(Duration::from_secs(5)),
            Terminal::Failure(PlaybackPipelineFailure::Offline)
        );
        assert_eq!(fixture.stop(transport), Ok(()));

        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let stalled = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let transport = fixture.start(handoff(&stalled.stream_url()), QUICK);
        assert_eq!(
            fixture.wait_terminal(Duration::from_secs(5)),
            Terminal::Failure(PlaybackPipelineFailure::Offline)
        );
        assert_eq!(fixture.stop(transport), Ok(()));
        assert!(stalled.client_disconnected_within(Duration::from_secs(3)));

        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let truncated = FixtureStreamServer::start(
            http_response(
                "200 OK",
                &[("Content-Length", (FIXTURE_BYTES.len() * 4).to_string())],
                FIXTURE_BYTES,
            ),
            StreamBehavior::Close,
        );
        let transport = fixture.start(handoff(&truncated.stream_url()), QUICK);
        assert_eq!(
            fixture.wait_terminal(Duration::from_secs(5)),
            Terminal::Failure(PlaybackPipelineFailure::Offline)
        );
        assert_eq!(fixture.stop(transport), Ok(()));
    }

    #[test]
    fn cancellation_while_reading_closes_the_socket_without_eos_or_failure() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        let server = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let generous = TransportConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        let transport = fixture.start(handoff(&server.stream_url()), generous);
        assert!(server.request(Duration::from_secs(3)).is_some());

        let stopped_at = Instant::now();
        assert_eq!(fixture.stop(transport), Ok(()));
        assert!(server.client_disconnected_within(Duration::from_secs(3)));
        assert!(stopped_at.elapsed() < Duration::from_secs(4));
        assert_eq!(
            fixture.wait_terminal(Duration::from_millis(200)),
            Terminal::Timeout,
            "cancellation must not be reported as EOS or failure"
        );
    }

    #[test]
    fn queue_growth_is_bounded_and_teardown_unblocks_a_blocked_push() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        let server = FixtureStreamServer::start(
            open_ended_response_head(),
            StreamBehavior::Repeat(vec![0x47_u8; 32 * 1_024]),
        );
        // A live appsrc under a paused sink accepts nothing downstream, so the
        // feeder must block on the byte limit instead of growing memory.
        fixture.pipeline.set_state(gst::State::Paused).unwrap();
        let generous = TransportConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        let transport = fixture.start(handoff(&server.stream_url()), generous);
        let max_bytes = fixture.source.property::<u64>("max-bytes");
        let deadline = Instant::now() + Duration::from_millis(1_500);
        while Instant::now() < deadline {
            let level = fixture.source.property::<u64>("current-level-bytes");
            assert!(
                level <= max_bytes + MAX_PUSH_BYTES as u64,
                "{level} > {max_bytes}"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let level = fixture.source.property::<u64>("current-level-bytes");
        assert!(
            level >= max_bytes.saturating_sub(MAX_PUSH_BYTES as u64),
            "{level}"
        );

        let stopped_at = Instant::now();
        assert_eq!(fixture.stop(transport), Ok(()));
        assert!(stopped_at.elapsed() < Duration::from_secs(4));
        assert!(server.client_disconnected_within(Duration::from_secs(3)));
    }

    #[test]
    fn rapid_replacement_joins_the_predecessor_while_the_successor_streams() {
        let Some(first) = FeedFixture::new() else {
            return;
        };
        let Some(second) = FeedFixture::new() else {
            return;
        };
        let first_server =
            FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let second_server =
            FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let generous = TransportConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        first.pipeline.set_state(gst::State::Playing).unwrap();
        let first_transport = first.start(handoff(&first_server.stream_url()), generous);
        assert!(first_server.request(Duration::from_secs(3)).is_some());
        second.pipeline.set_state(gst::State::Playing).unwrap();
        let second_transport = second.start(handoff(&second_server.stream_url()), generous);
        assert!(second_server.request(Duration::from_secs(3)).is_some());

        assert_eq!(first.stop(first_transport), Ok(()));
        assert!(first_server.client_disconnected_within(Duration::from_secs(3)));
        assert!(
            !second_server.client_disconnected_within(Duration::from_millis(300)),
            "retiring the predecessor must not touch the successor's connection"
        );
        assert_eq!(second.stop(second_transport), Ok(()));
        assert!(second_server.client_disconnected_within(Duration::from_secs(3)));
    }

    #[test]
    fn join_before_null_times_out_and_can_be_retried_after_null() {
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        // An unpaced multi-megabyte body keeps the reader saturated, so once
        // appsrc reaches its byte limit the very next push blocks the feeder.
        let server = FixtureStreamServer::start(
            open_ended_response_head(),
            StreamBehavior::Repeat(vec![0x47_u8; 8 * 1_024 * 1_024]),
        );
        fixture.pipeline.set_state(gst::State::Paused).unwrap();
        let generous = TransportConfig::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(30),
        );
        let mut transport = fixture.start(handoff(&server.stream_url()), generous);
        let max_bytes = fixture.source.property::<u64>("max-bytes");
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.source.property::<u64>("current-level-bytes") < max_bytes
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(100));

        // The feeder is blocked inside appsrc, so it cannot finish yet.
        let level = fixture.source.property::<u64>("current-level-bytes");
        assert_eq!(
            transport.join(Instant::now() + Duration::from_millis(200)),
            Err(TransportJoinError),
            "level={level} max={max_bytes}"
        );
        assert_eq!(fixture.stop(transport), Ok(()));
    }

    #[test]
    fn proxy_trap_child() {
        let Ok(trap) = env::var(PROXY_TRAP_CHILD_ENV) else {
            return;
        };
        let Some(fixture) = FeedFixture::new() else {
            return;
        };
        // The ambient proxy configuration must be visible to a default client
        // so the trap is proven reachable before the transport is judged.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let probe = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let _ = runtime.block_on(async { probe.get("http://192.0.2.1/").send().await });
        drop(probe);
        runtime.shutdown_timeout(Duration::from_secs(1));
        assert!(!trap.is_empty());

        let server = FixtureStreamServer::start(fixture_response(), StreamBehavior::Close);
        fixture.pipeline.set_state(gst::State::Playing).unwrap();
        let transport = fixture.start(handoff(&server.stream_url()), QUICK);
        assert_eq!(fixture.wait_terminal(Duration::from_secs(5)), Terminal::Eos);
        assert!(server.request(Duration::from_secs(3)).is_some());
        assert_eq!(fixture.stop(transport), Ok(()));
    }

    #[test]
    fn ambient_proxy_configuration_is_never_consulted_by_the_transport() {
        if FeedFixture::new().is_none() {
            return;
        }
        let trap = ConnectionTrap::start();
        let proxy = format!("http://{}", trap.address());
        let status = Command::new(env::current_exe().unwrap())
            .args(["--exact", PROXY_TRAP_CHILD_TEST, "--nocapture"])
            .env(PROXY_TRAP_CHILD_ENV, &proxy)
            .env("http_proxy", &proxy)
            .env("HTTP_PROXY", &proxy)
            .env("all_proxy", &proxy)
            .env("ALL_PROXY", &proxy)
            .env_remove("no_proxy")
            .env_remove("NO_PROXY")
            .status()
            .expect("spawn proxy trap child");
        assert!(status.success());
        assert_eq!(
            trap.connections(),
            1,
            "only the deliberate default-client probe may reach the trap"
        );
    }
}
