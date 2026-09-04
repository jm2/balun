//! Packaged-Windows playback probe: prove the staged runtime closure decodes.
//!
//! The Windows packaging helper runs this hidden probe against the staged
//! tree before archiving it. The probe initializes the bundled GStreamer
//! through the production runtime owner, requires every structural factory,
//! the frozen Windows decoder contract, and the Windows audio sink to resolve
//! to plugin files inside the package, then plays the checked-in synthetic
//! MPEG-2 fixture from a loopback HTTP server through the production `appsrc`
//! source policy and stream transport to end of stream, and tears the
//! pipeline down to `NULL` inside the playback bound. Every element the
//! pipeline autoplugs must also come from the package. Diagnostics carry no
//! URL, path, or native error text.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use thiserror::Error;

use super::session::configure_playbin_video;
use super::source_policy::{SourcePolicy, is_rejection_marker};
use super::transport::{
    PIPELINE_URI, STREAM_STARTED_MESSAGE, TRANSPORT_FAILURE_MESSAGE, TransportConfig,
};
use super::{PlaybackFactory, PlaybackRuntime};
use crate::controller::{OperationGeneration, StreamHandoff};
use crate::domain::{ChannelKey, DeviceId, GuideNumber};

/// The checked-in, video-only synthetic MPEG-2 transport stream, embedded
/// solely for this probe.
const FIXTURE_BYTES: &[u8] = include_bytes!("../../tests/fixtures/synthetic-mpeg2.ts");
/// Checksum-valid synthetic identity, the same one the fake device uses.
const PROBE_DEVICE_ID: u32 = 0x105A_1232;
const PROBE_GUIDE_NUMBER: &str = "5.1";
const PROBE_STREAM_PATH: &str = "/auto/v5.1";
const PLAYBACK_DEADLINE: Duration = Duration::from_secs(20);
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(5);
const ACCEPT_DEADLINE: Duration = Duration::from_secs(10);
const REQUEST_DEADLINE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(20);
const MAX_REQUEST_BYTES: usize = 16 * 1_024;
const MIN_DECODED_FRAMES: usize = 2;

/// Broadcast stream types the Windows package must decode, with the caps a
/// decoder has to accept. AC-4 has no open decoder and is deliberately absent.
const REQUIRED_DECODERS: [(&str, &str); 7] = [
    (
        "MPEG-2 video",
        "video/mpeg,mpegversion=2,systemstream=false",
    ),
    ("H.264 video", "video/x-h264"),
    ("HEVC video", "video/x-h265"),
    ("MPEG-1/2 audio", "audio/mpeg,mpegversion=1"),
    ("AAC audio", "audio/mpeg,mpegversion=4"),
    ("AC-3 audio", "audio/x-ac3"),
    ("E-AC-3 audio", "audio/x-eac3"),
];

/// Fixed, path-free reason the packaged playback probe failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PackagedProbeError {
    #[error("the packaged plugin directory is unavailable")]
    PluginDirectory,
    #[error("the packaged probe could not start its loopback stream server")]
    Server,
    #[error("the packaged GStreamer runtime could not initialize")]
    Initialize,
    #[error("the packaged GStreamer runtime is missing the {0} factory")]
    FactoryMissing(&'static str),
    #[error("the packaged {0} factory did not come from the package")]
    FactoryNotBundled(&'static str),
    #[error("the package has no bundled {0} decoder")]
    DecoderMissing(&'static str),
    #[error("the packaged {0} element could not be created")]
    ElementConstruction(&'static str),
    #[error("the packaged playback pipeline could not be constructed")]
    PipelineConstruction,
    #[error("the packaged playback pipeline could not start")]
    PipelineStart,
    #[error("the packaged source policy rejected the pipeline's source")]
    SourceRejected,
    #[error("the packaged stream transport reported a failure")]
    TransportFailed,
    #[error("the packaged playback pipeline reported an error")]
    PipelineError,
    #[error("the packaged playback probe timed out")]
    Timeout,
    #[error("the packaged playback pipeline never reached PLAYING")]
    NotPlaying,
    #[error("the packaged playback pipeline did not stop cleanly")]
    Teardown,
    #[error("the packaged decoder produced too few video frames")]
    TooFewDecodedFrames,
    #[error("an autoplugged element did not come from the package")]
    ElementNotBundled,
    #[error("the loopback stream server saw no acceptable request")]
    StreamRequest,
    #[error("the packaged probe identity could not be constructed")]
    Identity,
}

/// Run the complete packaged playback probe against `plugin_dir`.
pub(super) fn run(plugin_dir: &Path) -> Result<(), PackagedProbeError> {
    let canonical_plugin_dir = plugin_dir
        .canonicalize()
        .map_err(|_| PackagedProbeError::PluginDirectory)?;
    if !canonical_plugin_dir.is_dir() {
        return Err(PackagedProbeError::PluginDirectory);
    }

    let main_context = gst::glib::MainContext::default();
    let _main_context_guard = main_context
        .acquire()
        .map_err(|_| PackagedProbeError::Initialize)?;
    let runtime = PlaybackRuntime::initialize().map_err(|_| PackagedProbeError::Initialize)?;
    if let Some(missing) = runtime.capabilities().missing_required().next() {
        return Err(PackagedProbeError::FactoryMissing(missing.name()));
    }
    for factory in PlaybackFactory::ALL {
        bundled_factory(factory.name(), &canonical_plugin_dir)?;
    }
    verify_decoder_contract(&canonical_plugin_dir)?;
    #[cfg(target_os = "macos")]
    let _ranks = prefer_software_mpeg2_decoders();
    #[cfg(target_os = "windows")]
    let sink_name = "wasapi2sink";
    #[cfg(target_os = "macos")]
    let sink_name = "osxaudiosink";

    let audio_sink_factory = bundled_factory(sink_name, &canonical_plugin_dir)?;
    audio_sink_factory
        .create()
        .build()
        .map_err(|_| PackagedProbeError::ElementConstruction(sink_name))?;
    bundled_factory("autoaudiosink", &canonical_plugin_dir)?;
    let fakesink_factory = bundled_factory("fakesink", &canonical_plugin_dir)?;
    let playbin_factory = bundled_factory("playbin3", &canonical_plugin_dir)?;

    let mut server = FixtureServer::start()?;
    let handoff = probe_handoff(server.address)?;

    let pipeline = playbin_factory
        .create()
        .build()
        .map_err(|_| PackagedProbeError::ElementConstruction("playbin3"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| PackagedProbeError::PipelineConstruction)?;
    let _null_on_drop = NullOnDrop(pipeline.clone());
    let video_sink = fakesink_factory
        .create()
        .property("sync", false)
        .property("signal-handoffs", true)
        .build()
        .map_err(|_| PackagedProbeError::ElementConstruction("fakesink"))?;
    let decoded_frames = Arc::new(AtomicUsize::new(0));
    let counted_frames = Arc::clone(&decoded_frames);
    video_sink.connect("handoff", false, move |_| {
        counted_frames.fetch_add(1, Ordering::SeqCst);
        None
    });
    let audio_sink = fakesink_factory
        .create()
        .property("sync", false)
        .build()
        .map_err(|_| PackagedProbeError::ElementConstruction("fakesink"))?;
    configure_playbin_video(&pipeline, &video_sink)
        .map_err(|_| PackagedProbeError::PipelineConstruction)?;
    pipeline.set_property("audio-sink", &audio_sink);

    // playbin3 documents element-setup as the convenient equivalent of
    // deep-element-added, so it observes every dynamically autoplugged
    // element and where each one came from.
    let element_origins = Arc::new(Mutex::new(Vec::<ElementOrigin>::new()));
    let observed_origins = Arc::clone(&element_origins);
    pipeline.connect("element-setup", true, move |args| {
        let element = args
            .get(1)
            .and_then(|value| value.get::<gst::Element>().ok())?;
        let origin = element_origin(&element);
        if let Ok(mut observed) = observed_origins.lock() {
            observed.push(origin);
        }
        None
    });

    let source_policy = SourcePolicy::install(&pipeline, handoff, TransportConfig::PRODUCTION)
        .map_err(|_| PackagedProbeError::PipelineConstruction)?;
    pipeline.set_property("uri", PIPELINE_URI);
    if pipeline.property::<Option<String>>("uri").as_deref() != Some(PIPELINE_URI) {
        return Err(PackagedProbeError::PipelineConstruction);
    }
    let bus = pipeline
        .bus()
        .ok_or(PackagedProbeError::PipelineConstruction)?;

    let playback = run_to_end_of_stream(&pipeline, &bus, &source_policy);
    let teardown = teardown_to_null(&pipeline, &source_policy);
    let request = server.finish();
    playback?;
    teardown?;

    if decoded_frames.load(Ordering::SeqCst) < MIN_DECODED_FRAMES {
        return Err(PackagedProbeError::TooFewDecodedFrames);
    }
    let element_origins = element_origins
        .lock()
        .map(|observed| observed.clone())
        .unwrap_or_default();
    if element_origins.is_empty()
        || element_origins
            .iter()
            .any(|origin| !element_origin_is_bundled(origin, &canonical_plugin_dir))
    {
        return Err(PackagedProbeError::ElementNotBundled);
    }
    if !request.is_some_and(|request| request_is_acceptable(&request, server.address)) {
        return Err(PackagedProbeError::StreamRequest);
    }
    Ok(())
}

/// Build the loopback handoff for the probe's synthetic channel.
fn probe_handoff(address: SocketAddr) -> Result<StreamHandoff, PackagedProbeError> {
    let device_id = DeviceId::new(PROBE_DEVICE_ID).map_err(|_| PackagedProbeError::Identity)?;
    let guide_number =
        GuideNumber::new(PROBE_GUIDE_NUMBER).map_err(|_| PackagedProbeError::Identity)?;
    Ok(StreamHandoff::packaged_probe_fixture(
        ChannelKey::new(device_id, guide_number),
        OperationGeneration::INITIAL,
        &format!("http://{address}{PROBE_STREAM_PATH}"),
    ))
}

/// Every required broadcast decoder must be present and come from the package.
fn verify_decoder_contract(canonical_plugin_dir: &Path) -> Result<(), PackagedProbeError> {
    for (label, caps) in REQUIRED_DECODERS {
        let caps =
            gst::Caps::from_str(caps).map_err(|_| PackagedProbeError::DecoderMissing(label))?;
        let bundled_decoder = gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::DECODER,
            gst::Rank::MARGINAL,
        )
        .into_iter()
        .filter(|factory| factory.can_sink_any_caps(&caps))
        .any(|factory| plugin_is_bundled(&factory, canonical_plugin_dir));
        if !bundled_decoder {
            return Err(PackagedProbeError::DecoderMissing(label));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitingForStream,
    Playing,
}

/// Hold at PAUSED until the transport's first bytes exist, then play to EOS,
/// exactly as the production session does.
fn run_to_end_of_stream(
    pipeline: &gst::Pipeline,
    bus: &gst::Bus,
    source_policy: &SourcePolicy,
) -> Result<(), PackagedProbeError> {
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| PackagedProbeError::PipelineStart)?;
    if source_policy.is_rejected() {
        return Err(PackagedProbeError::SourceRejected);
    }
    let deadline = Instant::now() + PLAYBACK_DEADLINE;
    let mut phase = Phase::WaitingForStream;
    let mut observed_playing = false;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(PackagedProbeError::Timeout)?;
        let Some(message) = bus.timed_pop_filtered(
            clock_time(remaining),
            &[
                gst::MessageType::Eos,
                gst::MessageType::Error,
                gst::MessageType::Application,
                gst::MessageType::StateChanged,
            ],
        ) else {
            return Err(PackagedProbeError::Timeout);
        };
        let from_pipeline = message
            .src()
            .is_some_and(|source| source == pipeline.upcast_ref::<gst::Object>());
        match message.view() {
            gst::MessageView::Eos(_) => {
                return if observed_playing {
                    Ok(())
                } else {
                    Err(PackagedProbeError::NotPlaying)
                };
            }
            // Never format native GStreamer errors: their message and debug
            // fields can contain the complete source URI.
            gst::MessageView::Error(_) => return Err(PackagedProbeError::PipelineError),
            gst::MessageView::Application(application) if from_pipeline => {
                let Some(structure) = application.structure() else {
                    continue;
                };
                if structure.name() == STREAM_STARTED_MESSAGE && phase == Phase::WaitingForStream {
                    phase = Phase::Playing;
                    pipeline
                        .set_state(gst::State::Playing)
                        .map_err(|_| PackagedProbeError::PipelineStart)?;
                } else if structure.name() == TRANSPORT_FAILURE_MESSAGE {
                    return Err(PackagedProbeError::TransportFailed);
                } else if is_rejection_marker(structure) {
                    return Err(PackagedProbeError::SourceRejected);
                }
            }
            gst::MessageView::StateChanged(state_changed)
                if from_pipeline && state_changed.current() == gst::State::Playing =>
            {
                observed_playing = true;
            }
            _ => {}
        }
    }
}

/// Retire the transport, settle the pipeline to `NULL`, and join the workers.
fn teardown_to_null(
    pipeline: &gst::Pipeline,
    source_policy: &SourcePolicy,
) -> Result<(), PackagedProbeError> {
    let deadline = Instant::now() + TEARDOWN_DEADLINE;
    // Cancel the private HTTP request first so the connection starts closing
    // while the pipeline settles; join only after NULL, because a flushing
    // appsrc is what unblocks a feeder waiting on the bounded byte limit.
    let mut transport = source_policy.retire();
    let request = pipeline.set_state(gst::State::Null);
    let (transition, current, pending) = pipeline.state(clock_time(
        deadline.saturating_duration_since(Instant::now()),
    ));
    let settled = request.is_ok()
        && transition.is_ok()
        && current == gst::State::Null
        && pending == gst::State::VoidPending;
    let joined = transport
        .as_mut()
        .is_none_or(|transport| transport.join(deadline).is_ok());
    if settled && joined {
        Ok(())
    } else {
        Err(PackagedProbeError::Teardown)
    }
}

/// Convert a duration into a GStreamer clock time without overflow.
fn clock_time(duration: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(duration.as_nanos().min(u128::from(u64::MAX)) as u64)
}

struct NullOnDrop(gst::Pipeline);

impl Drop for NullOnDrop {
    /// Fail-safe `NULL` request if the probe unwinds before teardown.
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// Find a factory and require its plugin file to be inside the package.
fn bundled_factory(
    name: &'static str,
    canonical_plugin_dir: &Path,
) -> Result<gst::ElementFactory, PackagedProbeError> {
    let factory =
        gst::ElementFactory::find(name).ok_or(PackagedProbeError::FactoryMissing(name))?;
    if !plugin_is_bundled(&factory, canonical_plugin_dir) {
        return Err(PackagedProbeError::FactoryNotBundled(name));
    }
    Ok(factory)
}

/// Where one autoplugged element came from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ElementOrigin {
    /// A private element type a plugin instantiates directly, such as
    /// playsink's converter bins; it has no factory of its own.
    PrivateType,
    /// A factory registered by GStreamer core's built-in static plugin.
    StaticPlugin(String),
    /// A factory registered from a plugin file.
    PluginFile(Option<PathBuf>),
}

/// Classify where an autoplugged element's factory came from.
fn element_origin(element: &gst::Element) -> ElementOrigin {
    let Some(factory) = element.factory() else {
        return ElementOrigin::PrivateType;
    };
    let Some(plugin) = factory.plugin() else {
        return ElementOrigin::PluginFile(None);
    };
    match plugin.filename() {
        Some(filename) => ElementOrigin::PluginFile(Some(filename)),
        None => ElementOrigin::StaticPlugin(plugin.plugin_name().to_string()),
    }
}

/// Only GStreamer core's own static element plugin, private element types,
/// and factories from plugin files inside the package are acceptable.
fn element_origin_is_bundled(origin: &ElementOrigin, canonical_plugin_dir: &Path) -> bool {
    match origin {
        ElementOrigin::PrivateType => true,
        ElementOrigin::StaticPlugin(name) => name == "staticelements",
        ElementOrigin::PluginFile(filename) => {
            plugin_filename_is_bundled(filename.as_deref(), canonical_plugin_dir)
        }
    }
}

/// Whether a factory's plugin file lies inside the package.
fn plugin_is_bundled(factory: &gst::ElementFactory, canonical_plugin_dir: &Path) -> bool {
    let filename = factory.plugin().and_then(|plugin| plugin.filename());
    plugin_filename_is_bundled(filename.as_deref(), canonical_plugin_dir)
}

/// Whether a plugin filename canonicalizes to a file inside the package.
fn plugin_filename_is_bundled(filename: Option<&Path>, canonical_plugin_dir: &Path) -> bool {
    filename
        .and_then(|filename| filename.canonicalize().ok())
        .is_some_and(|filename| filename.is_file() && filename.starts_with(canonical_plugin_dir))
}

/// The transport must ask for exactly the probe path on the loopback host,
/// with Balun's own agent and without a Referer or any proxy header.
fn request_is_acceptable(request: &[u8], address: SocketAddr) -> bool {
    let Ok(text) = std::str::from_utf8(request) else {
        return false;
    };
    let mut lines = text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return false;
    };
    if request_line != format!("GET {PROBE_STREAM_PATH} HTTP/1.1") {
        return false;
    }
    let mut saw_host = false;
    let mut saw_agent = false;
    for line in lines.take_while(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "host" => saw_host = value == address.to_string(),
            "user-agent" => saw_agent = value.starts_with("Balun/"),
            "referer" | "proxy-authorization" | "proxy-connection" => return false,
            _ => {}
        }
    }
    saw_host && saw_agent
}

/// Serves the fixture once over loopback and records the request it saw.
struct FixtureServer {
    address: SocketAddr,
    request: Arc<Mutex<Option<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// Bind a loopback listener and serve the fixture once on a worker thread.
    fn start() -> Result<Self, PackagedProbeError> {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| PackagedProbeError::Server)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| PackagedProbeError::Server)?;
        let address = listener
            .local_addr()
            .map_err(|_| PackagedProbeError::Server)?;
        let request = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_request = Arc::clone(&request);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("balun-packaged-probe-server".to_owned())
            .spawn(move || {
                let Some(mut stream) = accept_one(&listener, &worker_stop) else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
                let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
                let observed = read_request(&mut stream, &worker_stop);
                if let Ok(mut slot) = worker_request.lock() {
                    *slot = Some(observed);
                }
                let _ = write_fully(&mut stream, &fixture_response(), &worker_stop);
                let _ = stream.shutdown(Shutdown::Both);
            })
            .map_err(|_| PackagedProbeError::Server)?;
        Ok(Self {
            address,
            request,
            stop,
            worker: Some(worker),
        })
    }

    /// Stop the server, join it, and return the request it recorded.
    fn finish(&mut self) -> Option<Vec<u8>> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.request.lock().ok().and_then(|slot| slot.clone())
    }
}

impl Drop for FixtureServer {
    /// Stop the server and join its worker.
    fn drop(&mut self) {
        self.finish();
    }
}

/// A complete `200 OK` response carrying the fixture with its exact length.
fn fixture_response() -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mpeg\r\nContent-Length: {}\r\n\r\n",
        FIXTURE_BYTES.len()
    )
    .into_bytes();
    response.extend_from_slice(FIXTURE_BYTES);
    response
}

/// Accept the first connection inside the bound, or none.
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

/// Read one bounded HTTP request head.
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

/// Write every byte unless the server is stopped or the socket closes.
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

/// Whether an I/O error is a socket timeout.
fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

#[cfg(target_os = "macos")]
struct DecoderRankGuard {
    original: Vec<(gst::PluginFeature, gst::Rank)>,
}

#[cfg(target_os = "macos")]
impl Drop for DecoderRankGuard {
    fn drop(&mut self) {
        for (feature, rank) in self.original.drain(..) {
            feature.set_rank(rank);
        }
    }
}

#[cfg(target_os = "macos")]
fn prefer_software_mpeg2_decoders() -> DecoderRankGuard {
    let registry = gst::Registry::get();
    let mut original = Vec::new();
    for name in ["vtdec_hw", "vtdec"] {
        if let Some(feature) = registry.lookup_feature(name) {
            original.push((feature.clone(), feature.rank()));
            feature.set_rank(gst::Rank::NONE);
        }
    }
    DecoderRankGuard { original }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_policy_requires_exact_path_host_and_agent_without_referer() {
        let address: SocketAddr = "127.0.0.1:5004".parse().unwrap();
        let accepted =
            "GET /auto/v5.1 HTTP/1.1\r\nhost: 127.0.0.1:5004\r\nuser-agent: Balun/0.1.0\r\n\r\n";
        assert!(request_is_acceptable(accepted.as_bytes(), address));
        for rejected in [
            "GET /auto/v5.2 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\nUser-Agent: Balun/0.1.0\r\n\r\n",
            "GET /auto/v5.1 HTTP/1.1\r\nHost: 127.0.0.1:5005\r\nUser-Agent: Balun/0.1.0\r\n\r\n",
            "GET /auto/v5.1 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\n\r\n",
            "GET /auto/v5.1 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\nUser-Agent: Balun/0.1.0\r\nReferer: x\r\n\r\n",
            "GET /auto/v5.1 HTTP/1.1\r\nHost: 127.0.0.1:5004\r\nUser-Agent: Balun/0.1.0\r\nProxy-Authorization: x\r\n\r\n",
            "",
        ] {
            assert!(
                !request_is_acceptable(rejected.as_bytes(), address),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn fixture_response_declares_the_exact_fixture_length() {
        let response = fixture_response();
        let head_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let head = std::str::from_utf8(&response[..head_end]).unwrap();
        assert!(head.contains(&format!("Content-Length: {}", FIXTURE_BYTES.len())));
        assert_eq!(&response[head_end + 4..], FIXTURE_BYTES);
    }

    #[test]
    fn probe_identity_is_the_synthetic_fake_device_channel() {
        let address: SocketAddr = "127.0.0.1:5004".parse().unwrap();
        let handoff = probe_handoff(address).unwrap();
        assert_eq!(handoff.channel_key().device_id().get(), PROBE_DEVICE_ID);
        assert_eq!(handoff.channel_key().guide_number().as_str(), "5.1");
    }

    #[test]
    fn element_origin_policy_accepts_private_types_core_statics_and_bundled_files() {
        let temp = tempfile::tempdir().unwrap();
        let plugin_dir = temp.path().canonicalize().unwrap();
        let bundled = plugin_dir.join("libgstplayback.dll");
        std::fs::write(&bundled, b"plugin").unwrap();
        let foreign = temp.path().parent().unwrap().join("libgstelsewhere.dll");
        assert!(element_origin_is_bundled(
            &ElementOrigin::PrivateType,
            &plugin_dir
        ));
        assert!(element_origin_is_bundled(
            &ElementOrigin::StaticPlugin("staticelements".to_owned()),
            &plugin_dir
        ));
        assert!(!element_origin_is_bundled(
            &ElementOrigin::StaticPlugin("otherstatic".to_owned()),
            &plugin_dir
        ));
        assert!(element_origin_is_bundled(
            &ElementOrigin::PluginFile(Some(bundled)),
            &plugin_dir
        ));
        assert!(!element_origin_is_bundled(
            &ElementOrigin::PluginFile(Some(foreign)),
            &plugin_dir
        ));
        assert!(!element_origin_is_bundled(
            &ElementOrigin::PluginFile(None),
            &plugin_dir
        ));
    }

    #[test]
    fn errors_are_fixed_and_path_free() {
        assert_eq!(
            PackagedProbeError::FactoryNotBundled("tsdemux").to_string(),
            "the packaged tsdemux factory did not come from the package"
        );
        assert_eq!(
            PackagedProbeError::DecoderMissing("AC-3 audio").to_string(),
            "the package has no bundled AC-3 audio decoder"
        );
    }
}
