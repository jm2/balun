//! Closed, endpoint-free classification of native playback failures.

use gst::glib::error::ErrorDomain;
use gstreamer as gst;
use thiserror::Error;

use super::source_policy::SourcePolicyMonitor;

const HTTP_STATUS_CODE: &str = "http-status-code";
const MISSING_PLUGIN_MESSAGE: &str = "missing-plugin";

/// URL-free category for a native playback pipeline failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum PlaybackPipelineFailure {
    /// The trusted tuner source reported HTTP 503.
    #[error("all tuners on the selected device are busy")]
    TunerBusy,
    /// The trusted tuner source reported HTTP 404.
    #[error("the selected channel is unavailable")]
    ChannelMissing,
    /// The trusted tuner source reported another HTTP rejection.
    #[error("the tuner rejected the stream request")]
    HttpRejected,
    /// The trusted tuner source could not open or continue reading.
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

pub(super) fn classify_pipeline_message(
    message: &gst::MessageRef,
    source_policy: &SourcePolicyMonitor,
) -> Option<PlaybackPipelineFailure> {
    match message.view() {
        gst::MessageView::Error(error) => {
            let native = error.error();
            let trusted_source = message
                .src()
                .is_some_and(|source| source_policy.is_trusted_http_source(source));

            if trusted_source && native.domain() == gst::ResourceError::domain() {
                let status = error
                    .details()
                    .and_then(|details| details.get::<u32>(HTTP_STATUS_CODE).ok())
                    .filter(|status| (100..=599).contains(status));
                match status {
                    Some(503) => return Some(PlaybackPipelineFailure::TunerBusy),
                    Some(404) => return Some(PlaybackPipelineFailure::ChannelMissing),
                    Some(300..=599) => return Some(PlaybackPipelineFailure::HttpRejected),
                    _ => {}
                }
                if native.matches(gst::ResourceError::NotFound)
                    || native.matches(gst::ResourceError::OpenRead)
                    || native.matches(gst::ResourceError::Read)
                {
                    return Some(PlaybackPipelineFailure::Offline);
                }
            }

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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelKey, DeviceId, GuideNumber};
    use crate::playback::PlaybackSessionFailure;
    use crate::playback::source_policy::SourcePolicy;
    use crate::playback::{PlaybackSessionState, TuneGeneration};
    use gst::prelude::*;

    const SECRET_TOKEN: &str = "secret-user-password-192-0-2-77";
    const SECRET_URI: &str = "http://secret-user-password-192-0-2-77@192.0.2.77:5004/auto/v999";

    fn trusted_source() -> Option<(gst::Pipeline, SourcePolicy, gst::Element)> {
        gst::init().ok()?;
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .ok()?
            .downcast::<gst::Pipeline>()
            .ok()?;
        let policy = SourcePolicy::install(&playbin).ok()?;
        let source = gst::ElementFactory::make("souphttpsrc").build().ok()?;
        source.set_property("name", SECRET_TOKEN);
        assert_eq!(source.name(), SECRET_TOKEN);
        playbin.emit_by_name::<()>("source-setup", &[&source]);
        Some((playbin, policy, source))
    }

    fn error_message(
        error: impl gst::message::MessageErrorDomain,
        source: &gst::Element,
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
            .field(HTTP_STATUS_CODE, status)
            .field("request-uri", SECRET_URI)
            .field("unrelated-secret", SECRET_TOKEN)
            .build()
    }

    #[test]
    fn trusted_numeric_http_statuses_and_offline_codes_are_exact() {
        let Some((_playbin, policy, source)) = trusted_source() else {
            return;
        };
        let monitor = policy.monitor();
        for (status, expected) in [
            (503, PlaybackPipelineFailure::TunerBusy),
            (404, PlaybackPipelineFailure::ChannelMissing),
            (302, PlaybackPipelineFailure::HttpRejected),
            (599, PlaybackPipelineFailure::HttpRejected),
        ] {
            let message = error_message(
                gst::ResourceError::Read,
                &source,
                poison_details(status as u32),
            );
            assert_eq!(
                classify_pipeline_message(&message, &monitor),
                Some(expected)
            );
        }

        for code in [
            gst::ResourceError::NotFound,
            gst::ResourceError::OpenRead,
            gst::ResourceError::Read,
        ] {
            let message = error_message(code, &source, poison_details(200_u32));
            assert_eq!(
                classify_pipeline_message(&message, &monitor),
                Some(PlaybackPipelineFailure::Offline)
            );
        }
    }

    #[test]
    fn exact_plugin_codec_and_protection_signals_are_closed_categories() {
        let Some((_playbin, policy, source)) = trusted_source() else {
            return;
        };
        let monitor = policy.monitor();
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
                classify_pipeline_message(&message, &monitor),
                Some(expected)
            );
        }

        let marker = gst::Structure::builder(MISSING_PLUGIN_MESSAGE)
            .field("detail", SECRET_URI)
            .build();
        let message = gst::message::Element::builder(marker).src(&source).build();
        assert_eq!(
            classify_pipeline_message(&message, &monitor),
            Some(PlaybackPipelineFailure::MissingCodecOrPlugin)
        );
    }

    #[test]
    fn untrusted_or_non_numeric_poison_falls_back_without_leaking() {
        let Some((_playbin, policy, source)) = trusted_source() else {
            return;
        };
        let monitor = policy.monitor();
        let untrusted = gst::ElementFactory::make("fakesrc").build().unwrap();
        untrusted.set_property("name", SECRET_TOKEN);
        let messages = vec![
            error_message(
                gst::ResourceError::Read,
                &untrusted,
                poison_details(503_u32),
            ),
            error_message(gst::CoreError::Failed, &source, poison_details(503_u32)),
            error_message(gst::ResourceError::Busy, &source, poison_details(200_u32)),
            error_message(
                gst::StreamError::TypeNotFound,
                &source,
                poison_details(200_u32),
            ),
            error_message(gst::ResourceError::Failed, &source, poison_details(503_i32)),
            error_message(gst::ResourceError::Failed, &source, poison_details("503")),
            error_message(gst::ResourceError::Failed, &source, poison_details(99_u32)),
            error_message(gst::ResourceError::Failed, &source, poison_details(600_u32)),
        ];

        for message in messages {
            let failure = classify_pipeline_message(&message, &monitor).unwrap();
            assert_eq!(failure, PlaybackPipelineFailure::Internal);
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
                format!("{failure:?}"),
                failure.to_string(),
                format!("{session_failure:?}"),
                session_failure.to_string(),
                format!("{state:?}"),
            ] {
                assert!(!rendered.contains(SECRET_TOKEN));
                assert!(!rendered.contains(SECRET_URI));
            }
        }
    }
}
