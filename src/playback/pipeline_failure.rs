//! Closed, endpoint-free classification of native playback failures.

use std::fmt;

use gst::prelude::*;
use gstreamer as gst;
use thiserror::Error;

use super::source_policy;
use super::transport::{TRANSPORT_FAILURE_FIELD, TRANSPORT_FAILURE_MESSAGE};

const MISSING_PLUGIN_MESSAGE: &str = "missing-plugin";
const MISSING_PLUGIN_DETAIL_FIELD: &str = "detail";

/// Closed list of stream types a missing decoder can be reported for.
///
/// Only the media type name and MPEG version of the reported caps are read;
/// every other value, field, or text maps to [`Self::Unknown`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MissingMedia {
    Mpeg2Video,
    H264Video,
    HevcVideo,
    MpegAudio,
    AacAudio,
    Ac3Audio,
    Eac3Audio,
    Ac4Audio,
    /// The stream type was not identified or is outside the table.
    Unknown,
}

impl MissingMedia {
    /// Every value in stable order.
    pub const ALL: [Self; 9] = [
        Self::Mpeg2Video,
        Self::H264Video,
        Self::HevcVideo,
        Self::MpegAudio,
        Self::AacAudio,
        Self::Ac3Audio,
        Self::Eac3Audio,
        Self::Ac4Audio,
        Self::Unknown,
    ];

    /// Plain-language name for user-facing copy, or `None` for
    /// [`Self::Unknown`].
    #[must_use]
    pub const fn description(self) -> Option<&'static str> {
        match self {
            Self::Mpeg2Video => Some("MPEG-2 video"),
            Self::H264Video => Some("H.264 video"),
            Self::HevcVideo => Some("HEVC video"),
            Self::MpegAudio => Some("MPEG audio"),
            Self::AacAudio => Some("AAC audio"),
            Self::Ac3Audio => Some("AC-3 audio"),
            Self::Eac3Audio => Some("E-AC-3 audio"),
            Self::Ac4Audio => Some("AC-4 audio"),
            Self::Unknown => None,
        }
    }

    fn from_caps(caps: &gst::CapsRef) -> Self {
        let Some(structure) = caps.structure(0) else {
            return Self::Unknown;
        };
        let name: &str = structure.name();
        let mpeg_version = structure.get::<i32>("mpegversion").ok();
        match (name, mpeg_version) {
            ("video/mpeg", Some(2)) => Self::Mpeg2Video,
            ("video/x-h264", _) => Self::H264Video,
            ("video/x-h265", _) => Self::HevcVideo,
            ("audio/mpeg", Some(1)) => Self::MpegAudio,
            ("audio/mpeg", Some(2 | 4)) => Self::AacAudio,
            ("audio/x-ac3", _) => Self::Ac3Audio,
            ("audio/x-eac3", _) => Self::Eac3Audio,
            ("audio/x-ac4", _) => Self::Ac4Audio,
            _ => Self::Unknown,
        }
    }

    fn from_missing_plugin(element: &gst::message::Element) -> Self {
        missing_plugin_caps(element).map_or(Self::Unknown, |caps| Self::from_caps(&caps))
    }
}

fn missing_plugin_caps(element: &gst::message::Element) -> Option<gst::Caps> {
    element
        .structure()
        .and_then(|structure| structure.get::<gst::Caps>(MISSING_PLUGIN_DETAIL_FIELD).ok())
}

/// Closed label for a missing plugin that only affects a stream Balun never
/// presents, such as teletext or DVB subtitles. GStreamer plays on without
/// such a decoder, so the report is logged and is not terminal.
///
/// Only the media type prefix of each reported caps structure is read. `None`
/// keeps the report terminal: any audio, video, or image structure, and caps
/// that are absent, unreadable, empty, or ANY, since none of those prove the
/// missing plugin is irrelevant to playback.
fn unpresented_stream(element: &gst::message::Element) -> Option<&'static str> {
    let caps = missing_plugin_caps(element)?;
    if caps.iter().any(|structure| {
        ["audio/", "video/", "image/"]
            .into_iter()
            .any(|prefix| structure.name().starts_with(prefix))
    }) {
        return None;
    }
    let name = caps.structure(0)?.name();
    Some(if name == "application/x-teletext" {
        "teletext"
    } else if name.starts_with("subpicture/") || name.starts_with("text/") {
        "subtitle"
    } else if name.starts_with("closedcaption/") {
        "closed-caption"
    } else {
        "other"
    })
}

impl fmt::Display for MissingMedia {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description().unwrap_or("an unidentified stream type"))
    }
}

/// URL-free category for a native playback pipeline or transport failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum PlaybackPipelineFailure {
    /// The device answered the stream request with HTTP 503.
    #[error("all tuners on the selected device are busy")]
    TunerBusy,
    /// The device answered the stream request with HTTP 404.
    #[error("the selected channel is unavailable")]
    ChannelMissing,
    /// The device answered the stream request with another HTTP status.
    #[error("the tuner rejected the stream request")]
    HttpRejected,
    /// The stream request could not connect, receive headers, or keep reading,
    /// or its body ended before any stream data.
    #[error("the selected tuner is offline or unreachable")]
    Offline,
    /// GStreamer reported an exact missing codec or plugin condition that can
    /// affect presented audio or video, naming the stream type when its caps
    /// were in the closed table.
    #[error("a required playback codec or plugin is unavailable ({0})")]
    MissingCodecOrPlugin(MissingMedia),
    /// GStreamer reported that the stream could not be decrypted.
    #[error("the selected channel is protected")]
    Protected,
    /// No narrower endpoint-free category was proven.
    #[error("an internal playback failure occurred")]
    Internal,
}

impl PlaybackPipelineFailure {
    /// Every category in stable order.
    pub const ALL: [Self; 7] = [
        Self::TunerBusy,
        Self::ChannelMissing,
        Self::HttpRejected,
        Self::Offline,
        Self::MissingCodecOrPlugin(MissingMedia::Unknown),
        Self::Protected,
        Self::Internal,
    ];

    /// Fixed numeric code carried by the transport's bus marker.
    pub(super) const fn code(self) -> u32 {
        match self {
            Self::TunerBusy => 1,
            Self::ChannelMissing => 2,
            Self::HttpRejected => 3,
            Self::Offline => 4,
            Self::MissingCodecOrPlugin(_) => 5,
            Self::Protected => 6,
            Self::Internal => 7,
        }
    }

    /// Decode a marker code; anything outside the closed table is `None`.
    pub(super) const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::TunerBusy),
            2 => Some(Self::ChannelMissing),
            3 => Some(Self::HttpRejected),
            4 => Some(Self::Offline),
            5 => Some(Self::MissingCodecOrPlugin(MissingMedia::Unknown)),
            6 => Some(Self::Protected),
            7 => Some(Self::Internal),
            _ => None,
        }
    }
}

/// Log closed native categories and typed counters, never native message text.
///
/// The constant pipeline URI does not make plugin-supplied strings safe:
/// errors, caps and marker names can contain arbitrary stream-derived data.
pub(super) fn log_pipeline_message(message: &gst::MessageRef) {
    let source = message
        .src()
        .and_then(|source| source.downcast_ref::<gst::Element>())
        .map_or("<none>", factory_name);
    match message.view() {
        gst::MessageView::Error(error) => {
            let native = error.error();
            tracing::warn!(
                target: "balun::playback",
                source = %source,
                domain = native_error_domain(&native),
                code = native_error_code(&native),
                "GStreamer reported an error"
            );
        }
        gst::MessageView::Element(element) if element.has_name(MISSING_PLUGIN_MESSAGE) => {
            if let Some(stream) = unpresented_stream(element) {
                tracing::info!(
                    target: "balun::playback",
                    source = %source,
                    stream = %stream,
                    "GStreamer reported a missing plugin for a stream Balun does not present"
                );
            } else {
                tracing::warn!(
                    target: "balun::playback",
                    source = %source,
                    media = %MissingMedia::from_missing_plugin(element).description().unwrap_or("unknown"),
                    "GStreamer reported a missing plugin"
                );
            }
        }
        gst::MessageView::Warning(warning) => {
            let native = warning.error();
            tracing::warn!(
                target: "balun::playback",
                source = %source,
                domain = native_error_domain(&native),
                code = native_error_code(&native),
                "GStreamer reported a warning"
            );
        }
        gst::MessageView::StreamCollection(collection) => {
            let collection = collection.stream_collection();
            tracing::info!(
                target: "balun::playback",
                source = %source,
                streams = collection.len(),
                collection = %describe_stream_collection(&collection),
                "stream collection"
            );
        }
        gst::MessageView::StreamsSelected(selected) => {
            let streams = selected
                .streams()
                .take(16)
                .map(|stream| describe_stream(&stream))
                .collect::<Vec<_>>()
                .join("; ");
            tracing::info!(
                target: "balun::playback",
                source = %source,
                selected = %streams,
                "streams selected"
            );
        }
        gst::MessageView::ClockLost(_) => {
            tracing::warn!(target: "balun::playback", source = %source, "pipeline clock lost");
        }
        gst::MessageView::Latency(_) => {
            tracing::debug!(target: "balun::playback", source = %source, "latency changed");
        }
        gst::MessageView::Qos(qos) => {
            let (processed, dropped) = qos.stats();
            tracing::debug!(
                target: "balun::playback",
                source = %source,
                live = qos.get().0,
                processed = ?processed,
                dropped = ?dropped,
                "sink quality of service"
            );
        }
        gst::MessageView::Application(application) => {
            let name = application.structure().map_or("unknown", |structure| {
                if source_policy::is_rejection_marker(structure) {
                    "source-policy-rejected"
                } else if structure.name() == TRANSPORT_FAILURE_MESSAGE {
                    "transport-failure"
                } else {
                    "other"
                }
            });
            tracing::debug!(target: "balun::playback", marker = %name, "application marker");
        }
        _ => {}
    }
}

/// Log what the running pipeline actually renders audio with: every audio
/// sink element, its negotiated raw caps, and the pipeline's live latency.
/// Only known caps labels and typed numeric fields enter the report.
pub(super) fn log_playing_diagnostics(pipeline: &gst::Element) {
    let mut query = gst::query::Latency::new();
    let latency = if pipeline.query(&mut query) {
        let (live, minimum, maximum) = query.result();
        format!(
            "live={live} min={minimum} max={}",
            maximum.map_or_else(|| String::from("none"), |value| value.to_string())
        )
    } else {
        String::from("unknown")
    };
    let audio_sinks = sink_elements(pipeline, "Audio")
        .into_iter()
        .map(|element| {
            let caps = element
                .static_pad("sink")
                .and_then(|pad| pad.current_caps())
                .map_or_else(
                    || String::from("not negotiated"),
                    |caps| describe_caps(&caps),
                );
            format!("{} [{caps}]", factory_name(&element))
        })
        .collect::<Vec<_>>();
    tracing::info!(
        target: "balun::playback",
        latency = %latency,
        audio_sinks = %join_or_none(audio_sinks),
        deinterlacing = %super::deinterlace::describe(pipeline),
        "pipeline playing"
    );
}

/// Log the base-sink buffer counters before teardown. An audio sink can still
/// discard late samples inside its ringbuffer while reporting a rendered
/// buffer, so zero drops do not establish that audible samples reached the device.
pub(super) fn log_teardown_diagnostics(pipeline: &gst::Element) {
    let sinks = sink_elements(pipeline, "Audio")
        .into_iter()
        .chain(sink_elements(pipeline, "Video"))
        .map(|element| {
            let stats = element
                .has_property("stats")
                .then(|| element.property::<gst::Structure>("stats"))
                .map_or_else(
                    || String::from("no stats"),
                    |stats| {
                        let field = |name: &str| {
                            stats
                                .get::<u64>(name)
                                .map_or_else(|_| String::from("?"), |value| value.to_string())
                        };
                        format!(
                            "rendered={} dropped={}",
                            field("rendered"),
                            field("dropped")
                        )
                    },
                );
            format!("{} [{stats}]", factory_name(&element))
        })
        .collect::<Vec<_>>();
    tracing::info!(
        target: "balun::playback",
        sinks = %join_or_none(sinks),
        "pipeline sink statistics at teardown"
    );
}

fn sink_elements(pipeline: &gst::Element, klass_word: &str) -> Vec<gst::Element> {
    pipeline
        .downcast_ref::<gst::Bin>()
        .map_or_else(Vec::new, |bin| {
            bin.iterate_recurse()
                .into_iter()
                .flatten()
                .filter(|element| {
                    element.factory().is_some_and(|factory| {
                        factory.has_type(gst::ElementFactoryType::SINK)
                            && factory
                                .metadata(gst::ELEMENT_METADATA_KLASS)
                                .is_some_and(|klass| klass.contains(klass_word))
                    })
                })
                .collect()
        })
}

fn native_error_domain(error: &gst::glib::Error) -> &'static str {
    use gst::glib::error::ErrorDomain;
    let domain = error.domain();
    if domain == gst::CoreError::domain() {
        "core"
    } else if domain == gst::StreamError::domain() {
        "stream"
    } else if domain == gst::ResourceError::domain() {
        "resource"
    } else if domain == gst::LibraryError::domain() {
        "library"
    } else {
        "other"
    }
}

fn native_error_code(error: &gst::glib::Error) -> Option<i32> {
    use gst::glib::error::ErrorDomain;
    // The bindings map unknown numeric codes in these domains to Failed.
    // Require an exact round trip so neither arbitrary plugin codes nor that
    // fallback are reported as a known error code.
    error
        .kind::<gst::CoreError>()
        .map(ErrorDomain::code)
        .or_else(|| error.kind::<gst::StreamError>().map(ErrorDomain::code))
        .or_else(|| error.kind::<gst::ResourceError>().map(ErrorDomain::code))
        .or_else(|| error.kind::<gst::LibraryError>().map(ErrorDomain::code))
        .filter(|code| *code == error.code())
}

fn closed_label<'a>(value: &str, labels: &[&'a str]) -> &'a str {
    labels
        .iter()
        .copied()
        .find(|label| *label == value)
        .unwrap_or("unknown")
}

fn factory_name(element: &gst::Element) -> &'static str {
    element.factory().map_or("<none>", |factory| {
        closed_label(
            factory.name().as_str(),
            &[
                "playbin3",
                "playsink",
                "uridecodebin3",
                "decodebin3",
                "parsebin",
                "subtitleoverlay",
                "appsrc",
                "queue",
                "multiqueue",
                "tsdemux",
                "mpegtsparse",
                "deinterlace",
                "audioconvert",
                "audioresample",
                "videoconvert",
                "autovideosink",
                "autoaudiosink",
                "gtk4paintablesink",
                "glsinkbin",
                "glimagesink",
                "pulsesink",
                "pipewiresink",
                "alsasink",
                "osxaudiosink",
                "wasapisink",
                "wasapi2sink",
                "directsoundsink",
                "fakesink",
            ],
        )
    })
}

fn join_or_none(parts: Vec<String>) -> String {
    if parts.is_empty() {
        String::from("none")
    } else {
        parts.join("; ")
    }
}

fn describe_stream_collection(collection: &gst::StreamCollection) -> String {
    let streams = collection
        .iter()
        .take(16)
        .map(|stream| describe_stream(&stream))
        .collect::<Vec<_>>();
    if streams.is_empty() {
        String::from("none")
    } else {
        streams.join("; ")
    }
}

fn describe_stream(stream: &gst::Stream) -> String {
    let kind = stream.stream_type();
    let kind = if kind.contains(gst::StreamType::AUDIO) {
        "audio"
    } else if kind.contains(gst::StreamType::VIDEO) {
        "video"
    } else if kind.contains(gst::StreamType::TEXT) {
        "text"
    } else {
        "other"
    };
    let flags = stream.stream_flags();
    let caps = stream
        .caps()
        .map_or_else(|| String::from("no caps"), |caps| describe_caps(&caps));
    let mut text = format!("{kind} {caps}");
    if flags.contains(gst::StreamFlags::SPARSE) {
        text.push_str(" sparse");
    }
    if flags.contains(gst::StreamFlags::SELECT) {
        text.push_str(" default");
    }
    text
}

/// Closed labels and typed integers from the first caps structure only.
/// Even a familiar field name can hold arbitrary text, lists or nested values.
fn describe_caps(caps: &gst::CapsRef) -> String {
    let Some(structure) = caps.structure(0) else {
        return String::from("empty caps");
    };
    let mut text = closed_label(
        structure.name().as_str(),
        &[
            "audio/x-raw",
            "audio/mpeg",
            "audio/x-ac3",
            "audio/x-eac3",
            "audio/x-ac4",
            "video/x-raw",
            "video/mpeg",
            "video/x-h264",
            "video/x-h265",
            "video/x-av1",
            "video/x-vp8",
            "video/x-vp9",
            "video/mpegts",
            "text/x-raw",
            "subpicture/x-dvb",
        ],
    )
    .to_owned();
    for field in ["mpegversion", "channels", "rate", "width", "height"] {
        if let Ok(value) = structure.get::<i32>(field)
            && (1..=1_000_000).contains(&value)
        {
            text.push_str(&format!(" {field}={value}"));
        }
    }
    for (field, labels) in [
        (
            "profile",
            &[
                "main",
                "high",
                "baseline",
                "constrained-baseline",
                "main-10",
                "lc",
                "he-aac-v1",
                "he-aac-v2",
                "simple",
                "advanced-simple",
            ][..],
        ),
        (
            "interlace-mode",
            &["progressive", "interleaved", "mixed", "fields", "alternate"][..],
        ),
        (
            "format",
            &[
                "S8",
                "U8",
                "S16LE",
                "S16BE",
                "S24LE",
                "S24BE",
                "S24_32LE",
                "S24_32BE",
                "S32LE",
                "S32BE",
                "F32LE",
                "F32BE",
                "F64LE",
                "F64BE",
                "I420",
                "YV12",
                "NV12",
                "NV21",
                "YUY2",
                "UYVY",
                "RGBA",
                "BGRA",
                "RGBx",
                "BGRx",
                "P010_10LE",
                "P010_10BE",
            ][..],
        ),
    ] {
        if let Ok(value) = structure.get::<&str>(field) {
            text.push_str(&format!(" {field}={}", closed_label(value, labels)));
        }
    }
    text
}

/// Native error and debug text, source names, details, and every structure
/// field other than the transport marker's bounded code and the missing
/// plugin's media type are ignored. A missing plugin reported only for a
/// stream Balun never presents is not a failure.
pub(super) fn classify_pipeline_message(
    message: &gst::MessageRef,
    pipeline: &gst::Pipeline,
) -> Option<PlaybackPipelineFailure> {
    match message.view() {
        gst::MessageView::Error(error) => {
            let native = error.error();
            if native.matches(gst::CoreError::MissingPlugin)
                || native.matches(gst::StreamError::CodecNotFound)
            {
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin(
                    MissingMedia::Unknown,
                ))
            } else if native.matches(gst::StreamError::Decrypt)
                || native.matches(gst::StreamError::DecryptNokey)
            {
                Some(PlaybackPipelineFailure::Protected)
            } else {
                Some(PlaybackPipelineFailure::Internal)
            }
        }
        gst::MessageView::Element(element) if element.has_name(MISSING_PLUGIN_MESSAGE) => {
            if unpresented_stream(element).is_some() {
                None
            } else {
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin(
                    MissingMedia::from_missing_plugin(element),
                ))
            }
        }
        gst::MessageView::Application(application) => {
            if message.src() != Some(pipeline.upcast_ref::<gst::Object>()) {
                return None;
            }
            let structure = application.structure()?;
            if source_policy::is_rejection_marker(structure) {
                Some(PlaybackPipelineFailure::Internal)
            } else if structure.name() == TRANSPORT_FAILURE_MESSAGE {
                Some(decode_transport_failure(structure))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn decode_transport_failure(structure: &gst::StructureRef) -> PlaybackPipelineFailure {
    if structure.n_fields() != 1 {
        return PlaybackPipelineFailure::Internal;
    }
    structure
        .get::<u32>(TRANSPORT_FAILURE_FIELD)
        .ok()
        .and_then(PlaybackPipelineFailure::from_code)
        .unwrap_or(PlaybackPipelineFailure::Internal)
}

#[cfg(test)]
mod tests {
    #[derive(Clone, Default)]
    struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    struct UntrustedErrorDomain;

    impl gst::glib::error::ErrorDomain for UntrustedErrorDomain {
        fn domain() -> gst::glib::Quark {
            gst::glib::Quark::from_str(SECRET_TOKEN)
        }

        fn code(self) -> i32 {
            SECRET_CODE
        }

        fn from(_code: i32) -> Option<Self> {
            Some(Self)
        }
    }

    #[test]
    fn emitted_native_logs_discard_plugin_text_and_stream_values() {
        gst::init().unwrap();
        let capture = CapturedLog::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .without_time()
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let source = gst::ElementFactory::make("uridecodebin3")
                .name(SECRET_TOKEN)
                .build()
                .unwrap();
            let pipeline = gst::Pipeline::builder().name(SECRET_TOKEN).build();
            pipeline.add(&source).unwrap();
            let caps = gst::Caps::builder("application/x-secret-user-password-192-0-2-77")
                .field("mpegversion", SECRET_URI)
                .field("channels", SECRET_URI)
                .field("rate", SECRET_URI)
                .field("width", SECRET_URI)
                .field("height", SECRET_URI)
                .field("profile", SECRET_URI)
                .field("interlace-mode", SECRET_URI)
                .field("format", SECRET_URI)
                .field("unrelated-secret", SECRET_TOKEN)
                .build();
            let stream = gst::Stream::new(
                Some(SECRET_URI),
                Some(&caps),
                gst::StreamType::AUDIO,
                gst::StreamFlags::SELECT,
            );
            let collection = gst::StreamCollection::builder(Some(SECRET_URI))
                .stream(stream.clone())
                .build();
            // Native plugins can supply arbitrary domains and numeric codes,
            // including out-of-table codes under recognized GStreamer domains.
            for native in [
                gst::glib::Error::new(UntrustedErrorDomain, SECRET_URI),
                gst::glib::Error::new(gst::CoreError::__Unknown(SECRET_CODE), SECRET_URI),
                gst::glib::Error::new(gst::StreamError::__Unknown(SECRET_CODE), SECRET_URI),
                gst::glib::Error::new(gst::ResourceError::__Unknown(SECRET_CODE), SECRET_URI),
                gst::glib::Error::new(gst::LibraryError::__Unknown(SECRET_CODE), SECRET_URI),
            ] {
                for mut message in [
                    error_message(gst::CoreError::Failed, &source, poison_details(503)),
                    gst::message::Warning::builder(gst::CoreError::Failed, SECRET_URI)
                        .src(&source)
                        .build(),
                ] {
                    message
                        .make_mut()
                        .structure_mut()
                        .set("gerror", native.clone());
                    log_pipeline_message(&message);
                }
            }
            let messages = [
                error_message(gst::StreamError::Failed, &source, poison_details(503)),
                gst::message::Warning::builder(gst::ResourceError::Failed, SECRET_URI)
                    .debug(SECRET_URI)
                    .details(poison_details(503))
                    .src(&source)
                    .build(),
                gst::message::Element::builder(
                    gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                        .field(MISSING_PLUGIN_DETAIL_FIELD, caps)
                        .field("name", SECRET_URI)
                        .build(),
                )
                .src(&source)
                .build(),
                gst::message::Element::builder(
                    gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                        .field(
                            MISSING_PLUGIN_DETAIL_FIELD,
                            gst::Caps::builder("audio/x-secret-user-password-192-0-2-77")
                                .field("uri", SECRET_URI)
                                .build(),
                        )
                        .field("name", SECRET_URI)
                        .build(),
                )
                .src(&source)
                .build(),
                gst::message::StreamCollection::builder(&collection)
                    .src(&source)
                    .build(),
                gst::message::StreamsSelected::builder(&collection)
                    .streams([&stream])
                    .src(&source)
                    .build(),
                application_message(
                    &pipeline,
                    gst::Structure::builder(SECRET_TOKEN)
                        .field("message", SECRET_URI)
                        .build(),
                ),
                gst::message::Latency::builder().src(&source).build(),
            ];
            for message in messages {
                log_pipeline_message(&message);
            }
            let sink = gst::ElementFactory::make("fakesink")
                .name(format!("{SECRET_TOKEN}-sink"))
                .build()
                .unwrap();
            pipeline.add(&sink).unwrap();
            log_playing_diagnostics(pipeline.upcast_ref());
            log_teardown_diagnostics(pipeline.upcast_ref());
            let filter = gst::ElementFactory::make("deinterlace").build().unwrap();
            pipeline.add(&filter).unwrap();
            let pad = filter.static_pad("src").unwrap();
            pad.set_active(true).unwrap();
            for rate in [
                gst::Fraction::new(SECRET_CODE, 1),
                gst::Fraction::new(1, SECRET_CODE),
                gst::Fraction::new(-1, 1),
                gst::Fraction::new(0, 1),
                gst::Fraction::new(17, 2),
                gst::Fraction::new(60_000, 1_001),
            ] {
                let caps = gst::Caps::builder("video/x-raw")
                    .field("framerate", rate)
                    .build();
                pad.store_sticky_event(&gst::event::Caps::new(&caps))
                    .unwrap();
                assert_eq!(
                    pad.current_caps()
                        .unwrap()
                        .structure(0)
                        .unwrap()
                        .get::<gst::Fraction>("framerate")
                        .unwrap(),
                    rate
                );
                log_playing_diagnostics(pipeline.upcast_ref());
            }
            pad.set_active(false).unwrap();
        });
        let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        for expected in [
            "GStreamer reported an error",
            "GStreamer reported a warning",
            "GStreamer reported a missing plugin source=uridecodebin3 media=unknown",
            "GStreamer reported a missing plugin for a stream Balun does not present",
            "stream=other",
            "stream collection",
            "streams selected",
            "application marker",
            "latency changed",
            "pipeline playing",
            "pipeline sink statistics at teardown",
            "uridecodebin3",
            "output-framerate=60000/1001",
            "unknown",
            "stream",
            "resource",
            "other",
        ] {
            assert!(
                output.contains(expected),
                "missing diagnostic category: {expected}"
            );
        }
        for forbidden in [
            SECRET_TOKEN,
            SECRET_URI,
            "192.0.2.77",
            "/auto/v999",
            "unrelated-secret",
        ] {
            assert!(
                !output.contains(forbidden),
                "native diagnostic leaked fixture value"
            );
        }
        assert!(!output.contains(&SECRET_CODE.to_string()));
        assert_eq!(output.matches("output-framerate=other").count(), 5);
        for line in output.lines().filter(|line| line.contains("code=")) {
            assert!(
                line.ends_with("code=1"),
                "unexpected native numeric code: {line}"
            );
        }
        assert_eq!(
            output
                .lines()
                .filter(|line| line.ends_with("code=1"))
                .count(),
            2
        );
    }

    #[test]
    fn native_error_codes_require_known_domain_and_exact_enum_round_trip() {
        use gst::glib::error::ErrorDomain;
        gst::init().unwrap();
        for native in [
            gst::glib::Error::new(gst::CoreError::Disabled, SECRET_URI),
            gst::glib::Error::new(gst::StreamError::DecryptNokey, SECRET_URI),
            gst::glib::Error::new(gst::ResourceError::NotAuthorized, SECRET_URI),
            gst::glib::Error::new(gst::LibraryError::Encode, SECRET_URI),
        ] {
            assert_eq!(native_error_code(&native), Some(native.code()));
        }
        for code in [i32::MIN, -1, 0, 42, i32::MAX] {
            for native in [
                gst::glib::Error::new(gst::CoreError::__Unknown(code), SECRET_URI),
                gst::glib::Error::new(gst::StreamError::__Unknown(code), SECRET_URI),
                gst::glib::Error::new(gst::ResourceError::__Unknown(code), SECRET_URI),
                gst::glib::Error::new(gst::LibraryError::__Unknown(code), SECRET_URI),
            ] {
                assert_eq!(native_error_code(&native), None);
            }
        }
        assert_eq!(
            native_error_code(&gst::glib::Error::new(UntrustedErrorDomain, SECRET_URI)),
            None
        );
        assert_eq!(
            native_error_code(&gst::glib::Error::new(gst::CoreError::Failed, SECRET_URI)),
            Some(gst::CoreError::Failed.code())
        );
    }

    #[test]
    fn diagnostic_caps_require_typed_bounded_fields_and_known_labels() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("width", gst::List::new([SECRET_URI]))
            .field("height", -1i32)
            .field("rate", i32::MAX)
            .field("profile", SECRET_TOKEN)
            .field("format", "I420")
            .field("interlace-mode", "mixed")
            .build();
        assert_eq!(
            describe_caps(&caps),
            "video/x-raw profile=unknown interlace-mode=mixed format=I420"
        );
        let caps = gst::Caps::builder("audio/x-raw")
            .field("channels", 2i32)
            .field("rate", 48_000i32)
            .field("format", "F32LE")
            .build();
        assert_eq!(
            describe_caps(&caps),
            "audio/x-raw channels=2 rate=48000 format=F32LE"
        );
        assert_eq!(
            native_error_domain(&gst::glib::Error::new(gst::CoreError::Failed, SECRET_URI)),
            "core"
        );
        assert_eq!(
            native_error_domain(&gst::glib::Error::new(
                gst::LibraryError::Failed,
                SECRET_URI
            )),
            "library"
        );
    }

    #[test]
    fn caps_and_stream_descriptions_name_the_format_only() {
        gst::init().unwrap();
        let caps = gst::Caps::builder("audio/x-ac3")
            .field("channels", 6i32)
            .field("rate", 48_000i32)
            .field("alignment", "frame")
            .build();
        assert_eq!(
            super::describe_caps(&caps),
            "audio/x-ac3 channels=6 rate=48000"
        );
        let video = gst::Caps::builder("video/mpeg")
            .field("mpegversion", 2i32)
            .field("width", 1920i32)
            .field("height", 1080i32)
            .field("interlace-mode", "interleaved")
            .build();
        assert_eq!(
            super::describe_caps(&video),
            "video/mpeg mpegversion=2 width=1920 height=1080 interlace-mode=interleaved"
        );
        assert_eq!(super::describe_caps(&gst::Caps::new_empty()), "empty caps");

        let stream = gst::Stream::new(
            None,
            Some(&caps),
            gst::StreamType::AUDIO,
            gst::StreamFlags::SELECT,
        );
        assert_eq!(
            super::describe_stream(&stream),
            "audio audio/x-ac3 channels=6 rate=48000 default"
        );
        let collection = gst::StreamCollection::builder(None).stream(stream).build();
        assert_eq!(
            super::describe_stream_collection(&collection),
            "audio audio/x-ac3 channels=6 rate=48000 default"
        );
    }

    use super::*;

    const SECRET_TOKEN: &str = "secret-user-password-192-0-2-77";
    const SECRET_CODE: i32 = 1_234_567_891;
    const SECRET_URI: &str = "http://secret-user-password-192-0-2-77@192.0.2.77:5004/auto/v999";

    fn pipeline() -> Option<gst::Pipeline> {
        gst::init().ok()?;
        Some(gst::Pipeline::new())
    }

    fn error_message(
        error: impl gst::message::MessageErrorDomain,
        source: &impl IsA<gst::Object>,
        details: gst::Structure,
    ) -> gst::Message {
        gst::message::Error::builder(error, SECRET_URI)
            .debug(SECRET_URI)
            .details(details)
            .src(source)
            .build()
    }

    fn poison_details<T: Into<gst::glib::Value> + Send>(status: T) -> gst::Structure {
        gst::Structure::builder("http-error")
            .field("http-status-code", status)
            .field("request-uri", SECRET_URI)
            .field("unrelated-secret", SECRET_TOKEN)
            .build()
    }

    fn application_message(pipeline: &gst::Pipeline, structure: gst::Structure) -> gst::Message {
        gst::message::Application::builder(structure)
            .src(pipeline)
            .build()
    }

    #[test]
    fn codes_round_trip_and_reject_everything_else() {
        for failure in PlaybackPipelineFailure::ALL {
            assert_eq!(
                PlaybackPipelineFailure::from_code(failure.code()),
                Some(failure)
            );
        }
        let codes = PlaybackPipelineFailure::ALL.map(PlaybackPipelineFailure::code);
        assert_eq!(
            codes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            codes.len()
        );
        for code in [0, 8, 255, u32::MAX] {
            assert_eq!(PlaybackPipelineFailure::from_code(code), None);
        }
    }

    #[test]
    fn transport_markers_from_the_exact_pipeline_map_to_their_category() {
        let Some(pipeline) = pipeline() else {
            return;
        };
        for failure in PlaybackPipelineFailure::ALL {
            let marker = gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, failure.code())
                .build();
            assert_eq!(
                classify_pipeline_message(&application_message(&pipeline, marker), &pipeline),
                Some(failure)
            );
        }

        let foreign = gst::Pipeline::new();
        let marker = gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
            .field(
                TRANSPORT_FAILURE_FIELD,
                PlaybackPipelineFailure::TunerBusy.code(),
            )
            .build();
        assert_eq!(
            classify_pipeline_message(&application_message(&foreign, marker), &pipeline),
            None,
            "a marker from another pipeline is not this owner's failure"
        );
        let rejection = gst::Structure::builder("balun-source-policy-rejected").build();
        assert_eq!(
            classify_pipeline_message(&application_message(&pipeline, rejection), &pipeline),
            Some(PlaybackPipelineFailure::Internal)
        );
        let unrelated = gst::Structure::builder("something-else").build();
        assert_eq!(
            classify_pipeline_message(&application_message(&pipeline, unrelated), &pipeline),
            None
        );
    }

    #[test]
    fn malformed_transport_markers_close_to_internal() {
        let Some(pipeline) = pipeline() else {
            return;
        };
        let malformed = [
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE).build(),
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, 0_u32)
                .build(),
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, 99_u32)
                .build(),
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, 1_i32)
                .build(),
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, "1")
                .build(),
            gst::Structure::builder(TRANSPORT_FAILURE_MESSAGE)
                .field(TRANSPORT_FAILURE_FIELD, 1_u32)
                .field("request-uri", SECRET_URI)
                .build(),
        ];
        for structure in malformed {
            assert_eq!(
                classify_pipeline_message(&application_message(&pipeline, structure), &pipeline),
                Some(PlaybackPipelineFailure::Internal)
            );
        }
    }

    #[test]
    fn exact_plugin_codec_and_protection_signals_are_closed_categories() {
        let Some(pipeline) = pipeline() else {
            return;
        };
        let source = gst::ElementFactory::make("fakesrc").build().unwrap();
        source.set_property("name", SECRET_TOKEN);
        for (message, expected) in [
            (
                error_message(
                    gst::CoreError::MissingPlugin,
                    &source,
                    poison_details(200_u32),
                ),
                PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Unknown),
            ),
            (
                error_message(
                    gst::StreamError::CodecNotFound,
                    &source,
                    poison_details(200_u32),
                ),
                PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Unknown),
            ),
            (
                error_message(gst::StreamError::Decrypt, &source, poison_details(200_u32)),
                PlaybackPipelineFailure::Protected,
            ),
            (
                error_message(
                    gst::StreamError::DecryptNokey,
                    &source,
                    poison_details(200_u32),
                ),
                PlaybackPipelineFailure::Protected,
            ),
        ] {
            assert_eq!(
                classify_pipeline_message(&message, &pipeline),
                Some(expected)
            );
        }

        let marker = gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
            .field("detail", SECRET_URI)
            .build();
        let message = gst::message::Element::builder(marker).src(&source).build();
        assert_eq!(
            classify_pipeline_message(&message, &pipeline),
            Some(PlaybackPipelineFailure::MissingCodecOrPlugin(
                MissingMedia::Unknown
            ))
        );
    }

    #[test]
    fn missing_plugin_details_map_to_the_closed_media_table() {
        let Some(pipeline) = pipeline() else {
            return;
        };
        let source = gst::ElementFactory::make("fakesrc").build().unwrap();
        source.set_property("name", SECRET_TOKEN);
        let cases = [
            (
                gst::Caps::builder("audio/x-ac4").build(),
                MissingMedia::Ac4Audio,
            ),
            (
                gst::Caps::builder("video/x-h265").build(),
                MissingMedia::HevcVideo,
            ),
            (
                gst::Caps::builder("video/x-h264").build(),
                MissingMedia::H264Video,
            ),
            (
                gst::Caps::builder("audio/x-ac3").build(),
                MissingMedia::Ac3Audio,
            ),
            (
                gst::Caps::builder("audio/x-eac3").build(),
                MissingMedia::Eac3Audio,
            ),
            (
                gst::Caps::builder("video/mpeg")
                    .field("mpegversion", 2_i32)
                    .build(),
                MissingMedia::Mpeg2Video,
            ),
            (
                gst::Caps::builder("audio/mpeg")
                    .field("mpegversion", 4_i32)
                    .build(),
                MissingMedia::AacAudio,
            ),
            (
                gst::Caps::builder("audio/mpeg")
                    .field("mpegversion", 1_i32)
                    .build(),
                MissingMedia::MpegAudio,
            ),
            (
                gst::Caps::builder("video/mpeg")
                    .field("mpegversion", 4_i32)
                    .build(),
                MissingMedia::Unknown,
            ),
            (
                gst::Caps::builder("video/x-secret")
                    .field("uri", SECRET_URI)
                    .build(),
                MissingMedia::Unknown,
            ),
            (gst::Caps::new_empty(), MissingMedia::Unknown),
        ];
        for (caps, expected) in cases {
            let marker = gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                .field("type", "decoder")
                .field("name", SECRET_URI)
                .field("detail", &caps)
                .build();
            let message = gst::message::Element::builder(marker).src(&source).build();
            let failure = classify_pipeline_message(&message, &pipeline);
            assert_eq!(
                failure,
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin(expected)),
                "{caps:?}"
            );
            let text = failure.unwrap().to_string();
            assert!(
                !text.contains(SECRET_TOKEN) && !text.contains("192"),
                "{text}"
            );
        }
        for media in MissingMedia::ALL {
            assert_eq!(
                media.description().is_none(),
                media == MissingMedia::Unknown
            );
        }
        assert_eq!(
            PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Ac4Audio).to_string(),
            "a required playback codec or plugin is unavailable (AC-4 audio)"
        );
    }

    /// The fields `gst_missing_decoder_message_new` posts, as parsebin
    /// (teletext), playsink's subtitle overlay (DVB subtitles), and decodebin3
    /// (audio and video) report a missing decoder.
    fn missing_decoder_message(source: &gst::Element, caps: &gst::Caps) -> gst::Message {
        gst::message::Element::builder(
            gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                .field("type", "decoder")
                .field(MISSING_PLUGIN_DETAIL_FIELD, caps)
                .field("name", SECRET_URI)
                .field("stream-id", SECRET_TOKEN)
                .build(),
        )
        .src(source)
        .build()
    }

    fn unpresented_label(message: &gst::Message) -> Option<&'static str> {
        let gst::MessageView::Element(element) = message.view() else {
            panic!("missing-plugin reports are element messages");
        };
        unpresented_stream(element)
    }

    #[test]
    fn missing_plugins_for_unpresented_streams_do_not_end_playback() {
        let pipeline = pipeline().expect("initialize GStreamer");
        let source = gst::ElementFactory::make("fakesrc").build().unwrap();
        let cases = [
            (
                gst::Caps::builder("application/x-teletext").build(),
                "teletext",
            ),
            (gst::Caps::builder("subpicture/x-dvb").build(), "subtitle"),
            (gst::Caps::builder("subpicture/x-pgs").build(), "subtitle"),
            (
                gst::Caps::builder("text/x-raw")
                    .field("format", SECRET_URI)
                    .build(),
                "subtitle",
            ),
            (
                gst::Caps::builder("closedcaption/x-cea-608").build(),
                "closed-caption",
            ),
            (
                gst::Caps::builder("application/x-secret-user-password-192-0-2-77")
                    .field("uri", SECRET_URI)
                    .build(),
                "other",
            ),
            (
                gst::Caps::builder_full()
                    .structure(gst::Structure::new_empty("application/x-teletext"))
                    .structure(gst::Structure::new_empty("subpicture/x-dvb"))
                    .build(),
                "teletext",
            ),
        ];
        for (caps, label) in cases {
            let message = missing_decoder_message(&source, &caps);
            assert_eq!(
                classify_pipeline_message(&message, &pipeline),
                None,
                "{caps:?}"
            );
            assert_eq!(unpresented_label(&message), Some(label), "{caps:?}");
        }
    }

    #[test]
    fn missing_plugins_for_audio_or_video_still_end_playback() {
        let pipeline = pipeline().expect("initialize GStreamer");
        let source = gst::ElementFactory::make("fakesrc").build().unwrap();
        let cases = [
            (
                gst::Caps::builder("audio/x-ac4").build(),
                MissingMedia::Ac4Audio,
            ),
            (
                gst::Caps::builder("video/x-h265").build(),
                MissingMedia::HevcVideo,
            ),
            (
                gst::Caps::builder("video/mpeg")
                    .field("mpegversion", 2_i32)
                    .build(),
                MissingMedia::Mpeg2Video,
            ),
            (
                gst::Caps::builder("audio/x-dts").build(),
                MissingMedia::Unknown,
            ),
            (
                gst::Caps::builder("video/x-av1").build(),
                MissingMedia::Unknown,
            ),
            (
                gst::Caps::builder("image/x-jpc").build(),
                MissingMedia::Unknown,
            ),
            // One presented structure keeps a mixed report terminal.
            (
                gst::Caps::builder_full()
                    .structure(gst::Structure::new_empty("application/x-teletext"))
                    .structure(gst::Structure::new_empty("audio/x-ac4"))
                    .build(),
                MissingMedia::Unknown,
            ),
            // Nothing proves an unreadable report irrelevant.
            (gst::Caps::new_empty(), MissingMedia::Unknown),
            (gst::Caps::new_any(), MissingMedia::Unknown),
        ];
        for (caps, expected) in cases {
            let message = missing_decoder_message(&source, &caps);
            assert_eq!(
                classify_pipeline_message(&message, &pipeline),
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin(expected)),
                "{caps:?}"
            );
            assert_eq!(unpresented_label(&message), None, "{caps:?}");
        }
        for structure in [
            gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                .field("type", "element")
                .field(MISSING_PLUGIN_DETAIL_FIELD, "videoconvert")
                .build(),
            gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
                .field("type", "decoder")
                .build(),
        ] {
            let message = gst::message::Element::builder(structure)
                .src(&source)
                .build();
            assert_eq!(
                classify_pipeline_message(&message, &pipeline),
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin(
                    MissingMedia::Unknown
                ))
            );
            assert_eq!(unpresented_label(&message), None);
        }
    }

    #[test]
    fn native_http_looking_errors_no_longer_carry_tuner_meaning() {
        let Some(pipeline) = pipeline() else {
            return;
        };
        let source = gst::ElementFactory::make("fakesrc").build().unwrap();
        source.set_property("name", SECRET_TOKEN);
        let messages = vec![
            error_message(gst::ResourceError::Read, &source, poison_details(503_u32)),
            error_message(
                gst::ResourceError::NotFound,
                &source,
                poison_details(404_u32),
            ),
            error_message(
                gst::ResourceError::OpenRead,
                &source,
                poison_details(200_u32),
            ),
            error_message(gst::CoreError::Failed, &source, poison_details(503_u32)),
            error_message(gst::ResourceError::Busy, &source, poison_details(200_u32)),
            error_message(
                gst::StreamError::TypeNotFound,
                &source,
                poison_details(200_u32),
            ),
            error_message(
                gst::ResourceError::Failed,
                &pipeline,
                poison_details(503_u32),
            ),
        ];

        for message in messages {
            let failure = classify_pipeline_message(&message, &pipeline).unwrap();
            assert_eq!(failure, PlaybackPipelineFailure::Internal);
            for rendered in [format!("{failure:?}"), failure.to_string()] {
                assert!(!rendered.contains(SECRET_TOKEN));
                assert!(!rendered.contains(SECRET_URI));
            }
        }
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn failed_session_state_stays_endpoint_free_for_every_category() {
        use crate::domain::{ChannelKey, DeviceId, GuideNumber};
        use crate::playback::{PlaybackSessionFailure, PlaybackSessionState, TuneGeneration};

        for failure in PlaybackPipelineFailure::ALL {
            let session_failure = PlaybackSessionFailure::Pipeline(failure);
            let state = PlaybackSessionState::Failed {
                generation: TuneGeneration::default(),
                channel_key: ChannelKey::new(
                    DeviceId::new(0x105A_1232).unwrap(),
                    GuideNumber::new("7.1").unwrap(),
                ),
                failure: session_failure,
            };
            for rendered in [
                format!("{session_failure:?}"),
                session_failure.to_string(),
                format!("{state:?}"),
            ] {
                assert!(!rendered.contains(SECRET_TOKEN));
                assert!(!rendered.contains(SECRET_URI));
                assert!(!rendered.contains("http"));
            }
        }
    }
}
