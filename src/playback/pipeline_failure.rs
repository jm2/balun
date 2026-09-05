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
        element
            .structure()
            .and_then(|structure| structure.get::<gst::Caps>(MISSING_PLUGIN_DETAIL_FIELD).ok())
            .map_or(Self::Unknown, |caps| Self::from_caps(&caps))
    }
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
    /// The stream request could not connect, receive headers, or keep reading.
    #[error("the selected tuner is offline or unreachable")]
    Offline,
    /// GStreamer reported an exact missing codec or plugin condition, naming
    /// the stream type when its caps were in the closed table.
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

/// Reduce one bus message from the exact owned pipeline to a fixed category.
///
/// Log what a bus message says before it is reduced to a closed category.
///
/// This is the only place native GStreamer error text reaches anything, and
/// it reaches only the process's standard error. GStreamer never receives a
/// device address or stream URL, so the text cannot contain one.
pub(super) fn log_pipeline_message(message: &gst::MessageRef) {
    let source = message
        .src()
        .and_then(|source| source.downcast_ref::<gst::Element>().cloned())
        .and_then(|element| element.factory())
        .map_or_else(
            || String::from("<none>"),
            |factory| factory.name().to_string(),
        );
    match message.view() {
        gst::MessageView::Error(error) => {
            let native = error.error();
            tracing::warn!(
                target: "balun::playback",
                source = %source,
                domain = %native.domain().as_str(),
                code = native.code(),
                message = %native.message(),
                debug = %error.debug().map(|text| text.to_string()).unwrap_or_default(),
                "GStreamer reported an error"
            );
        }
        gst::MessageView::Element(element) if element.has_name(MISSING_PLUGIN_MESSAGE) => {
            tracing::warn!(
                target: "balun::playback",
                source = %source,
                media = %MissingMedia::from_missing_plugin(element).description().unwrap_or("unknown"),
                "GStreamer reported a missing plugin"
            );
        }
        gst::MessageView::Warning(warning) => {
            let native = warning.error();
            tracing::warn!(
                target: "balun::playback",
                source = %source,
                domain = %native.domain().as_str(),
                code = native.code(),
                message = %native.message(),
                debug = %warning.debug().map(|text| text.to_string()).unwrap_or_default(),
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
            let name = application.structure().map_or_else(
                || String::from("<none>"),
                |structure| structure.name().to_string(),
            );
            tracing::debug!(target: "balun::playback", marker = %name, "application marker");
        }
        _ => {}
    }
}

/// Log what the running pipeline actually renders audio with: every audio
/// sink element, its negotiated raw caps, and the pipeline's live latency.
/// Caps carry codec and format fields only, never an address or URL.
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
        "pipeline playing"
    );
}

/// Log how many buffers every audio and video sink rendered and dropped as
/// late before the pipeline is torn down; a silent tune shows up here as a
/// sink that dropped what it received.
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

fn factory_name(element: &gst::Element) -> String {
    element.factory().map_or_else(
        || String::from("<none>"),
        |factory| factory.name().to_string(),
    )
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

/// The first structure's name plus the few fields that identify a stream's
/// format; other fields (and every other structure) are omitted.
fn describe_caps(caps: &gst::CapsRef) -> String {
    let Some(structure) = caps.structure(0) else {
        return String::from("empty caps");
    };
    let mut text = structure.name().to_string();
    for field in [
        "mpegversion",
        "profile",
        "channels",
        "rate",
        "width",
        "height",
        "interlace-mode",
        "format",
    ] {
        if let Ok(value) = structure.value(field)
            && let Ok(rendered) = value.serialize()
        {
            text.push_str(&format!(" {field}={rendered}"));
        }
    }
    text
}

/// Native error and debug text, source names, details, and every structure
/// field other than the transport marker's bounded code and the missing
/// plugin's media type are ignored.
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
            Some(PlaybackPipelineFailure::MissingCodecOrPlugin(
                MissingMedia::from_missing_plugin(element),
            ))
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
