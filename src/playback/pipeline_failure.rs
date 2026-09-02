//! Closed, endpoint-free classification of native playback failures.

use gst::prelude::*;
use gstreamer as gst;
use thiserror::Error;

use super::source_policy;
use super::transport::{TRANSPORT_FAILURE_FIELD, TRANSPORT_FAILURE_MESSAGE};

const MISSING_PLUGIN_MESSAGE: &str = "missing-plugin";

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
    /// GStreamer reported an exact missing codec or plugin condition.
    #[error("a required playback codec or plugin is unavailable")]
    MissingCodecOrPlugin,
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
        Self::MissingCodecOrPlugin,
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
            Self::MissingCodecOrPlugin => 5,
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
            5 => Some(Self::MissingCodecOrPlugin),
            6 => Some(Self::Protected),
            7 => Some(Self::Internal),
            _ => None,
        }
    }
}

/// Reduce one bus message from the exact owned pipeline to a fixed category.
///
/// Native error and debug text, source names, details, and every structure
/// field other than the transport marker's bounded code are ignored.
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
                Some(PlaybackPipelineFailure::MissingCodecOrPlugin)
            } else if native.matches(gst::StreamError::Decrypt)
                || native.matches(gst::StreamError::DecryptNokey)
            {
                Some(PlaybackPipelineFailure::Protected)
            } else {
                Some(PlaybackPipelineFailure::Internal)
            }
        }
        gst::MessageView::Element(element) if element.has_name(MISSING_PLUGIN_MESSAGE) => {
            Some(PlaybackPipelineFailure::MissingCodecOrPlugin)
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
                PlaybackPipelineFailure::MissingCodecOrPlugin,
            ),
            (
                error_message(
                    gst::StreamError::CodecNotFound,
                    &source,
                    poison_details(200_u32),
                ),
                PlaybackPipelineFailure::MissingCodecOrPlugin,
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
            Some(PlaybackPipelineFailure::MissingCodecOrPlugin)
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
